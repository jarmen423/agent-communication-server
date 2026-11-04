use crate::{
    api,
    local::{self, Agent, State},
};
use anyhow::{bail, Context, Result};
use reqwest::Method;
use serde_json::json;
use std::path::Path;

pub fn check_id(id: &str) -> Result<()> {
    nats_hub_client::protocol::require_valid_identity(id)?;
    if nats_hub_client::protocol::reserved_identity(id)
        || nats_hub_client::query_api::OP_NAMESPACES.contains(&id)
    {
        bail!("reserved identity");
    }
    Ok(())
}

/// The named identity, or the only one when none is named.
pub fn select<'a>(state: &'a State, identity: Option<&'a str>) -> Result<&'a str> {
    let id = match identity {
        Some(id) => id,
        None if state.agents.len() == 1 => state.agents.keys().next().unwrap(),
        None => {
            bail!("choose an identity with --identity (required unless exactly one is configured)")
        }
    };
    check_id(id)?;
    if !state.agents.contains_key(id) {
        bail!("identity is not configured on this device");
    }
    Ok(id)
}

pub async fn add(root: &Path, id: &str, role: &str) -> Result<()> {
    check_id(id)?;
    if !["worker", "orchestrator"].contains(&role) {
        bail!("role must be worker or orchestrator");
    }
    let mut state = local::load(root)?;
    if state.agents.contains_key(id) {
        bail!("identity already configured on this device");
    }
    let api = api::authorized(root, &mut state).await?;
    let key = nkeys::KeyPair::new_user();
    let seed = key.seed()?;
    let public = key.public_key();
    // Persist the local key before sending the public half. A retry can refresh
    // a successfully registered identity even after an interrupted JWT request.
    local::write_creds(root, id, "PENDING", &seed)?;
    let registration = api
        .request(
            Method::POST,
            "/v1/device/identities",
            Some(&state.access_token),
            json!({"identity": id, "role": role, "public_key": public, "tenant":state.tenant}),
        )
        .await;
    if let Err(e) = &registration {
        std::fs::remove_file(local::creds_path(root, id))?;
        if e.to_string().ends_with("access_denied") {
            bail!("cannot add an identity in this tenant; check your tenant access and whether the tenant is ready");
        }
        bail!("{e}");
    }
    let registered = registration?;
    if registered.get("tenant").is_some() && registered["tenant"].as_str() != Some(&state.tenant) {
        std::fs::remove_file(local::creds_path(root, id))?;
        bail!("registration tenant mismatch");
    }
    state.agents.insert(
        id.to_owned(),
        Agent {
            role: role.into(),
            public_key: public,
            expires_at: 0,
        },
    );
    local::save(root, &state)?;
    refresh(root, &mut state, id).await?;
    println!(
        "Added {id} ({role}): {}",
        local::creds_path(root, id).display()
    );
    Ok(())
}

pub async fn refresh(root: &Path, state: &mut State, id: &str) -> Result<()> {
    check_id(id)?;
    let api = api::authorized(root, state).await?;
    let agent = state.agents.get(id).context("identity not configured")?;
    let seed = local::seed(root, id)?;
    let key =
        nkeys::KeyPair::from_seed(&seed).map_err(|_| anyhow::anyhow!("invalid local nkey"))?;
    if key.public_key() != agent.public_key {
        bail!("local key does not match registered public key");
    }
    let value = api
        .request(
            Method::POST,
            "/v1/credentials/nats",
            Some(&state.access_token),
            json!({"identity": id, "public_key": agent.public_key, "tenant":state.tenant}),
        )
        .await?;
    if value["tenant"].as_str() != Some(state.tenant.as_str()) {
        bail!("credential tenant mismatch");
    }
    let jwt = api::string(&value, "jwt")?;
    use base64::Engine;
    let claims: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(jwt.split('.').nth(1).context("invalid credential JWT")?)?,
    )?;
    if claims["sub"].as_str() != Some(agent.public_key.as_str()) {
        bail!("credential public key mismatch");
    }
    let expiry = value["expires_at"]
        .as_u64()
        .context("missing NATS expiry")?;
    local::write_creds(root, id, &jwt, &seed)?;
    state.agents.get_mut(id).unwrap().expires_at = expiry;
    local::save(root, state)?;
    eprintln!("Refreshed {id}; {}", local::expiry(expiry));
    Ok(())
}

pub async fn remove(root: &Path, id: &str) -> Result<()> {
    check_id(id)?;
    let mut state = crate::tenants::load_local(root, false)?;
    if !state.agents.contains_key(id) {
        bail!("identity is not configured on this device");
    }
    let mut member = crate::tenants::names(root)?.contains(&state.tenant);
    if member {
        let api = api::authorized(root, &mut state).await?;
        let result = api
            .request(
                Method::DELETE,
                &format!("/v1/device/identities/{id}?tenant={}", state.tenant),
                Some(&state.access_token),
                json!({}),
            )
            .await;
        if let Err(e) = result {
            if !e.to_string().starts_with("HTTP 403 ") {
                return Err(e);
            }
            crate::tenants::refresh_membership(crate::tenants::device_root(root)).await?;
            member = crate::tenants::names(root)?.contains(&state.tenant);
            if member {
                return Err(e);
            }
        }
    }
    // The daemon reloads state under the same operation lock before every refresh.
    state.agents.remove(id);
    local::save(root, &state)?;
    let path = local::creds_path(root, id);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if member {
        println!("Revoked {id}; local credential removed");
    } else {
        println!(
            "Removed {id} locally; tenant access is unavailable, remote identity was not revoked"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_reserved_components_and_api_namespaces() {
        for name in [
            "hub-server",
            "hub-orchestrator",
            "hub-locks",
            "visualizer",
            "wave",
            "session",
            "agent",
            "history",
            "thread",
            "envelope",
            "stats",
            "ping",
            "backup",
            "lock",
        ] {
            assert!(
                check_id(name).is_err(),
                "reserved identity accepted: {name}"
            );
        }
        for name in ["bot", "Lock", "hub-locks-1", "worker_42"] {
            check_id(name).unwrap();
        }
        for name in ["", "a.b", "*", "../bad"] {
            assert!(check_id(name).is_err());
        }
    }
}

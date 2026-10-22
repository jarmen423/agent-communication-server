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
    if let Err(e) = api
        .request(
            Method::POST,
            "/v1/device/identities",
            Some(&state.access_token),
            json!({"identity": id, "role": role, "public_key": public}),
        )
        .await
    {
        std::fs::remove_file(local::creds_path(root, id))?;
        return Err(e);
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
            json!({"identity": id, "public_key": agent.public_key}),
        )
        .await?;
    let jwt = api::string(&value, "jwt")?;
    let expiry = value["expires_at"]
        .as_u64()
        .context("missing NATS expiry")?;
    local::write_creds(root, id, &jwt, &seed)?;
    state.agents.get_mut(id).unwrap().expires_at = expiry;
    local::save(root, state)?;
    eprintln!("Refreshed {id}; expires at {expiry}");
    Ok(())
}

pub async fn remove(root: &Path, id: &str) -> Result<()> {
    check_id(id)?;
    let mut state = local::load(root)?;
    if !state.agents.contains_key(id) {
        bail!("identity is not configured on this device");
    }
    let api = api::authorized(root, &mut state).await?;
    api.request(
        Method::DELETE,
        &format!("/v1/device/identities/{id}"),
        Some(&state.access_token),
        json!({}),
    )
    .await?;
    // The daemon reloads state under the same operation lock before every refresh.
    state.agents.remove(id);
    local::save(root, &state)?;
    let path = local::creds_path(root, id);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    println!("Revoked {id}; local credential removed");
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

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

/// Longest agent client name: the control plane tags credentials with
/// `acs-provider:<name>`, and NATS tags are at most 64 characters.
const MAX_PROVIDER: usize = 64 - "acs-provider:".len();

/// The control plane's rule for an agent client name (`identities.provider`):
/// `^[a-z0-9][a-z0-9._-]{0,50}$`.
pub fn check_provider(p: &str) -> Result<()> {
    let b = p.as_bytes();
    let ok = (1..=MAX_PROVIDER).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(c));
    if !ok {
        bail!(
            "invalid provider: use lowercase letters, digits, '.', '_' or '-', \
             starting with a letter or digit, at most {MAX_PROVIDER} characters (e.g. claude-code)"
        );
    }
    Ok(())
}

/// clap value parser for `--provider`.
pub fn parse_provider(p: &str) -> Result<String> {
    check_provider(p)?;
    Ok(p.to_owned())
}

/// The agent client a worker backend or MCP client runs as, when known.
pub fn provider_for(client: &str) -> Option<&'static str> {
    Some(match client {
        "claude" => "claude-code",
        "codex" => "codex",
        "cursor" => "cursor",
        "hermes" | "hermes_acp" => "hermes",
        "grok" | "grok_acp" => "grok",
        "kilo" | "kilo_acp" => "kilo",
        "opencode" | "opencode_acp" => "opencode",
        _ => return None,
    })
}

/// A one-line hint when an identity's recorded agent client is missing or
/// differs from `expected`. The control plane has no way to change it in
/// place, so the fix is to register the identity again.
pub fn provider_hint(state: &State, id: &str, expected: Option<&str>) -> Option<String> {
    let agent = state.agents.get(id)?;
    let role = if agent.role == "worker" {
        " --role worker"
    } else {
        ""
    };
    let fix = |p: &str| {
        format!(
            "acs agent rm {id} --tenant {t} && acs agent add {id}{role} --tenant {t} --provider {p}",
            t = state.tenant
        )
    };
    match (agent.provider.as_deref(), expected) {
        (None, e) => Some(format!(
            "note: {id} has no agent client recorded, so its messages carry no origin.agent; \
             to record one: {}",
            fix(e.unwrap_or("<client>"))
        )),
        (Some(p), Some(e)) if p != e => Some(format!(
            "note: {id} is recorded as agent client {p}, so its messages carry origin.agent = {p}"
        )),
        _ => None,
    }
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

pub async fn add(root: &Path, id: &str, role: &str, provider: Option<&str>) -> Result<()> {
    check_id(id)?;
    if !["worker", "orchestrator"].contains(&role) {
        bail!("role must be worker or orchestrator");
    }
    if let Some(p) = provider {
        check_provider(p)?;
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
    let mut body =
        json!({"identity": id, "role": role, "public_key": public, "tenant": state.tenant});
    if let Some(p) = provider {
        body["provider"] = json!(p);
    }
    let registration = api
        .request(
            Method::POST,
            "/v1/device/identities",
            Some(&state.access_token),
            body,
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
            provider: provider.map(str::to_owned),
        },
    );
    local::save(root, &state)?;
    refresh(root, &mut state, id).await?;
    let kind = match provider {
        Some(p) => format!("{role}, {p}"),
        None => role.to_owned(),
    };
    println!(
        "Added {id} ({kind}): {}",
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
    fn provider_names_follow_the_control_plane_rule() {
        for ok in [
            "claude-code",
            "codex",
            "hermes",
            "claude.ai",
            "0x",
            "a",
            &"a".repeat(51),
        ] {
            check_provider(ok).unwrap();
        }
        for bad in [
            "",
            "Claude",
            "-x",
            ".x",
            "_x",
            "a b",
            "a/b",
            "a:b",
            "é",
            &"a".repeat(52),
        ] {
            assert!(check_provider(bad).is_err(), "accepted {bad:?}");
        }
    }

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

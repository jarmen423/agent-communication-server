//! Tenant selection and the device-wide token cache.
use crate::{
    api,
    local::{self, State},
};
use anyhow::{bail, Context, Result};
use reqwest::Method;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn validate(slug: &str) -> Result<()> {
    if slug.is_empty()
        || slug.len() > 64
        || !slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        bail!("invalid tenant slug; expected 1–64 letters, digits, underscores or hyphens");
    }
    Ok(())
}
pub fn device_root(root: &Path) -> &Path {
    if !root.join("device.json").exists()
        && root.parent().is_some_and(|p| {
            p.file_name().is_some_and(|n| n == "tenants")
                && p.parent()
                    .is_some_and(|base| base.join("device.json").exists())
        })
    {
        root.parent().unwrap().parent().unwrap()
    } else {
        root
    }
}
pub fn path(root: &Path, tenant: &str) -> Result<PathBuf> {
    validate(tenant)?;
    Ok(device_root(root).join("tenants").join(tenant))
}
pub fn device(root: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(
        &fs::read(device_root(root).join("device.json")).context("not logged in; run acs login")?,
    )?)
}
pub fn names(root: &Path) -> Result<Vec<String>> {
    let d = device(root)?;
    let mut names = vec![];
    for t in d["tenants"]
        .as_array()
        .context("missing tenant membership")?
    {
        let s = t["tenant"].as_str().context("missing tenant slug")?;
        validate(s)?;
        names.push(s.to_owned());
    }
    Ok(names)
}
pub fn select(root: &Path, explicit: Option<&str>) -> Result<PathBuf> {
    let names = names(root)?;
    let current = fs::read_to_string(root.join("current")).ok();
    let chosen = explicit.or(current.as_deref()).or_else(|| {
        if names.len() == 1 {
            Some(names[0].as_str())
        } else {
            None
        }
    });
    let chosen = chosen.with_context(|| format!("choose --tenant from: {}", names.join(", ")))?;
    validate(chosen)?;
    if !names.iter().any(|n| n == chosen) {
        bail!("tenant is not available to this device");
    }
    path(root, chosen)
}
pub fn load(root: &Path) -> Result<State> {
    let root = if root == device_root(root) {
        select(root, None)?
    } else {
        root.to_owned()
    };
    let mut d = device(&root)?;
    let tenant = root
        .file_name()
        .unwrap()
        .to_str()
        .context("invalid tenant")?;
    validate(tenant)?;
    if !names(&root)?.iter().any(|n| n == tenant) {
        bail!("tenant is not available to this device");
    }
    let t: Value = match fs::read(root.join("state.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            json!({"nats_url": d["nats_url"], "agents": {}})
        }
        Err(e) => return Err(e.into()),
    };
    d["tenant"] = json!(tenant);
    d["nats_url"] = t["nats_url"].clone();
    d["agents"] = t["agents"].clone();
    Ok(serde_json::from_value(d)?)
}
pub fn save(root: &Path, state: &State) -> Result<()> {
    let base = device_root(root);
    let tenant = path(base, &state.tenant)?;
    if root != base && root != tenant {
        bail!("tenant state mismatch");
    }
    local::atomic(
        &tenant.join("state.json"),
        &serde_json::to_vec_pretty(&json!({"nats_url":state.nats_url,"agents":state.agents}))?,
    )?;
    let mut d = serde_json::to_value(state)?;
    d.as_object_mut().unwrap().remove("agents");
    let previous = device(base).ok();
    d["tenants"] = previous.as_ref().map(|v| v["tenants"].clone()).unwrap_or_else(|| json!([{"tenant":state.tenant,"personal":true,"kind":"personal","role":"owner","handle":"","state":"ready"}]));
    d["tenant"] = previous
        .as_ref()
        .map(|v| v["tenant"].clone())
        .unwrap_or_else(|| json!(state.tenant));
    if let Some(p) = previous {
        d["nats_url"] = p["nats_url"].clone();
    }
    local::atomic(&base.join("device.json"), &serde_json::to_vec_pretty(&d)?)
}
pub fn normalize(value: &Value, personal: &str) -> Result<Value> {
    let mut result = vec![];
    for t in value.as_array().context("invalid tenants response")? {
        let slug = t
            .as_str()
            .or_else(|| t["tenant"].as_str())
            .context("missing tenant")?;
        validate(slug)?;
        result.push(if t.is_string() {
            json!({"tenant":slug,"personal":slug == personal})
        } else {
            t.clone()
        });
    }
    if result.is_empty() {
        bail!("device has no tenants");
    }
    Ok(json!(result))
}
pub fn membership(root: &Path, value: &Value) -> Result<()> {
    if let Some(ts) = value.get("tenants") {
        let mut d = device(root)?;
        d["tenants"] = normalize(ts, d["tenant"].as_str().context("missing personal tenant")?)?;
        local::atomic(
            &device_root(root).join("device.json"),
            &serde_json::to_vec_pretty(&d)?,
        )?;
    }
    Ok(())
}
pub async fn command(root: &Path, use_tenant: Option<&str>) -> Result<()> {
    let personal = device(root)?["tenant"]
        .as_str()
        .context("missing default tenant")?
        .to_owned();
    let mut state = load(&path(root, &personal)?)?;
    let api = api::authorized(root, &mut state).await?;
    let value = api
        .request(
            Method::GET,
            "/v1/tenants",
            Some(&state.access_token),
            json!({}),
        )
        .await;
    let value = match value {
        Ok(value) => value,
        Err(e) if e.to_string().starts_with("HTTP 404 ") => {
            json!({"tenants": device(root)?["tenants"]})
        }
        Err(e) => return Err(e),
    };
    membership(root, &value)?;
    if let Some(t) = use_tenant {
        select(root, Some(t))?;
        local::atomic(&root.join("current"), t.as_bytes())?;
        println!("Using tenant {t}");
    } else {
        let current = fs::read_to_string(root.join("current")).unwrap_or_default();
        println!("TENANT  ROLE  HANDLE  STATE");
        let mut ts = value["tenants"]
            .as_array()
            .context("missing tenants")?
            .clone();
        ts.sort_by_key(|t| !t["personal"].as_bool().unwrap_or(false));
        for t in ts {
            println!(
                "{}{}  {}  {}  {}",
                if t["tenant"] == current { "*" } else { " " },
                t["tenant"].as_str().unwrap_or(""),
                t["role"].as_str().unwrap_or(""),
                t["handle"].as_str().unwrap_or(""),
                t["state"].as_str().unwrap_or("")
            );
        }
    }
    Ok(())
}

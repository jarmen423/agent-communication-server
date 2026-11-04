//! Crash-recoverable upgrade; device.json is the commit marker.
use crate::{local, tenants};
use anyhow::Result;
use serde_json::{json, Value};
use std::{fs, path::Path};

pub fn run(root: &Path) -> Result<()> {
    if root.join("device.json").exists() || !root.join("state.json").exists() {
        return Ok(());
    }
    let _lock = local::lock(root, "operation.lock")?;
    if root.join("device.json").exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .and_then(|s| s.strip_prefix(".migrate-"))
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            && entry.file_type()?.is_dir()
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    let bytes = fs::read(root.join("state.json"))?;
    let state: local::State = serde_json::from_slice(&bytes)?;
    tenants::validate(&state.tenant)?;
    for id in state.agents.keys() {
        crate::agents::check_id(id)?;
    }
    let stage = root.join(format!(".migrate-{}", std::process::id()));
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    let target = stage.join("tenants").join(&state.tenant);
    local::atomic(
        &target.join("state.json"),
        &serde_json::to_vec_pretty(&json!({"nats_url":state.nats_url,"agents":state.agents}))?,
    )?;
    for id in state.agents.keys() {
        match fs::read(local::creds_path(root, id)) {
            Ok(bytes) => local::atomic(&local::creds_path(&target, id), &bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    let mut d: Value = serde_json::from_slice(&bytes)?;
    d.as_object_mut().unwrap().remove("agents");
    d["tenants"] = json!([{"tenant":state.tenant,"personal":true,"kind":"personal","role":"owner","handle":"","state":"ready"}]);
    local::atomic(&stage.join("device.json"), &serde_json::to_vec_pretty(&d)?)?;
    local::atomic(&root.join("state.json.bak"), &bytes)?;
    if !root.join("tenants").exists() {
        fs::rename(stage.join("tenants"), root.join("tenants"))?;
    }
    // Individual links preserve the old paths even if interrupted between links.
    local::private_dir(&root.join("creds"))?;
    #[cfg(unix)]
    for id in state.agents.keys() {
        let old = local::creds_path(root, id);
        let link = root.join("creds").join(format!(".{id}.link"));
        if link.is_symlink() {
            fs::remove_file(&link)?;
        }
        std::os::unix::fs::symlink(
            local::creds_path(&tenants::path(root, &state.tenant)?, id),
            &link,
        )?;
        fs::rename(link, old)?;
    }
    local::atomic(&root.join("current"), state.tenant.as_bytes())?;
    fs::rename(stage.join("device.json"), root.join("device.json"))?;
    fs::File::open(root)?.sync_all()?;
    fs::remove_file(root.join("state.json"))?;
    fs::remove_dir_all(stage)?;
    Ok(())
}

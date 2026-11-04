//! Crash-recoverable upgrade; device.json is the commit marker.
use crate::{local, tenants};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::{fs, path::Path};

pub fn sweep(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(".migrate-")
            && entry.file_type()?.is_dir()
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    if root.join("device.json").exists() && root.join("state.json").exists() {
        fs::remove_file(root.join("state.json"))?;
    }
    Ok(())
}

pub async fn run(root: &Path) -> Result<()> {
    let _lock = local::lock(root, "operation.lock")?;
    if root.join("device.json").exists() || !root.join("state.json").exists() {
        let restart = root.join("state.json").exists() && crate::daemon::running(root);
        sweep(root)?;
        drop(_lock);
        if restart {
            crate::daemon::restart_after_upgrade().await;
        }
        return Ok(());
    }
    let was_running = crate::daemon::running(root);
    let bytes = fs::read(root.join("state.json"))?;
    let state: local::State = serde_json::from_slice(&bytes)?;
    tenants::validate(&state.tenant)?;
    let mut ids: std::collections::BTreeSet<String> = state.agents.keys().cloned().collect();
    if root.join("creds").is_dir() {
        for entry in fs::read_dir(root.join("creds"))? {
            let source = entry?.path();
            if source.extension().is_some_and(|s| s == "creds") {
                ids.insert(
                    source
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| anyhow::anyhow!("invalid credential filename"))?
                        .to_owned(),
                );
            }
        }
    }
    for id in &ids {
        crate::agents::check_id(id)?;
    }
    let destination = tenants::path(root, &state.tenant)?;
    if root.join("tenants").exists() {
        if !destination.join("state.json").is_file() {
            bail!("incomplete tenant restore; preserve the legacy state and credentials before retrying");
        }
        for id in &ids {
            let source = local::creds_path(root, id);
            if source.exists()
                && fs::read(&source)?
                    != fs::read(local::creds_path(&destination, id)).map_err(|_| {
                        anyhow::anyhow!("incomplete tenant restore: missing credential")
                    })?
            {
                bail!("incomplete tenant restore: credential content differs");
            }
        }
    }
    sweep(root)?;
    let stage = root.join(format!(".migrate-{}", std::process::id()));
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    let target = stage.join("tenants").join(&state.tenant);
    local::atomic_private(
        &target.join("state.json"),
        &serde_json::to_vec_pretty(&json!({"nats_url":state.nats_url,"agents":state.agents}))?,
    )?;
    for id in &ids {
        match fs::read(local::creds_path(root, id)) {
            Ok(bytes) => local::atomic_private(&local::creds_path(&target, id), &bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    let mut d: Value = serde_json::from_slice(&bytes)?;
    d.as_object_mut().unwrap().remove("agents");
    d["tenants"] = json!([{"tenant":state.tenant,"personal":true,"kind":"personal","role":"owner","handle":"","state":"ready"}]);
    local::atomic_private(&stage.join("device.json"), &serde_json::to_vec_pretty(&d)?)?;
    local::atomic_private(&root.join("state.json.bak"), &bytes)?;
    if !root.join("tenants").exists() {
        fs::rename(stage.join("tenants"), root.join("tenants"))?;
        fs::File::open(root)?.sync_all()?;
    }
    // Individual links preserve the old paths even if interrupted between links.
    local::private_dir(&root.join("creds"))?;
    #[cfg(unix)]
    for id in &ids {
        let old = local::creds_path(root, id);
        let target = local::creds_path(&destination, id);
        if !old.exists() {
            continue;
        }
        if fs::read(&old)? != fs::read(&target)? {
            bail!("migration credential content differs");
        }
        let link = root.join("creds").join(format!(".{id}.link"));
        if link.is_symlink() {
            fs::remove_file(&link)?;
        }
        std::os::unix::fs::symlink(
            local::creds_path(&tenants::path(root, &state.tenant)?, id),
            &link,
        )?;
        fs::rename(link, old)?;
        fs::File::open(root.join("creds"))?.sync_all()?;
    }
    local::atomic_private(&root.join("current"), state.tenant.as_bytes())?;
    fs::rename(stage.join("device.json"), root.join("device.json"))?;
    fs::File::open(root)?.sync_all()?;
    fs::remove_file(root.join("state.json"))?;
    fs::remove_dir_all(stage)?;
    drop(_lock);
    if was_running {
        crate::daemon::restart_after_upgrade().await;
    }
    Ok(())
}

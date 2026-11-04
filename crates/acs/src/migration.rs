//! Crash-recoverable upgrade; device.json is the commit marker.
use crate::{local, tenants};
use anyhow::{bail, Context, Result};
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
    upgrade(root).await.with_context(|| format!(
        "migration of {} failed; preserve this file and creds/, repair the named restore file and retry, or run acs logout to revoke and clear the device",
        root.join("state.json").display()
    ))
}

async fn upgrade(root: &Path) -> Result<()> {
    if root.join("device.json").exists()
        && !root.join("state.json").exists()
        && !fs::read_dir(root)?.any(|entry| {
            entry.is_ok_and(|e| e.file_name().to_string_lossy().starts_with(".migrate-"))
        })
    {
        return Ok(());
    }
    let _lock = local::lock(root, "operation.lock")?;
    if root.join("device.json").exists() || !root.join("state.json").exists() {
        let restart = root.join("state.json").exists() && crate::daemon::running(root);
        sweep(root)?;
        drop(_lock);
        if restart {
            crate::daemon::restart_after_upgrade(root).await;
        }
        return Ok(());
    }
    let was_running = crate::daemon::running(root);
    let bytes = fs::read(root.join("state.json"))?;
    let state: local::State = serde_json::from_slice(&bytes)?;
    tenants::validate(&state.tenant)?;
    let ids = state.agents.keys().collect::<Vec<_>>();
    if root.join("creds").is_dir() {
        for entry in fs::read_dir(root.join("creds"))? {
            let source = entry?.path();
            if source.extension().is_some_and(|s| s == "creds")
                && !source
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|id| state.agents.contains_key(id))
            {
                eprintln!(
                    "WARNING: leaving unconfigured credential {} untouched",
                    source.display()
                );
            }
        }
    }
    for id in &ids {
        nats_hub_client::protocol::require_valid_identity(id).with_context(|| {
            format!(
                "{}: invalid configured identity {id}",
                root.join("state.json").display()
            )
        })?;
        if crate::agents::check_id(id).is_err() {
            eprintln!(
                "WARNING: preserving existing reserved identity {id} from {}",
                root.join("state.json").display()
            );
        }
    }
    let destination = tenants::path(root, &state.tenant)?;
    let restored = root.join("tenants").exists();
    let mut credentials = std::collections::BTreeMap::new();
    if restored {
        let file = destination.join("state.json");
        let restored: Value =
            serde_json::from_slice(&fs::read(&file).with_context(|| {
                format!("incomplete tenant restore: restore {}", file.display())
            })?)
            .with_context(|| {
                format!(
                    "invalid restored state {}; restore it from the legacy state",
                    file.display()
                )
            })?;
        for (id, agent) in &state.agents {
            if restored["agents"][id]["public_key"].as_str() != Some(&agent.public_key) {
                bail!("incomplete tenant restore: {} must contain identity {id} with the legacy public_key; restore it before retrying", file.display());
            }
        }
    }
    for id in &ids {
        let source = local::creds_path(root, id);
        let bytes = match fs::read(&source) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "cannot read {}; restore it before retrying",
                        source.display()
                    )
                })
            }
        };
        let chosen = if restored {
            let target = local::creds_path(&destination, id);
            let other = fs::read(&target).with_context(|| {
                format!(
                    "incomplete tenant restore: restore {} from {}",
                    target.display(),
                    source.display()
                )
            })?;
            reconcile(&bytes, &other).with_context(|| format!("incomplete tenant restore: {} differs from {}; restore the matching credential before retrying", target.display(), source.display()))?
        } else {
            bytes
        };
        credentials.insert(*id, chosen);
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
    for (id, bytes) in &credentials {
        local::atomic_private(&local::creds_path(&target, id), bytes)?;
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
    if restored {
        for (id, bytes) in &credentials {
            local::atomic_private(&local::creds_path(&destination, id), bytes)?;
        }
    }
    local::private_dir(&destination.join("creds"))?;
    for dir in [
        destination.join("creds"),
        destination.clone(),
        root.join("tenants"),
    ] {
        fs::File::open(dir)?.sync_all()?;
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
        reconcile(&fs::read(&old)?, &fs::read(&target)?).with_context(|| {
            format!(
                "{}: restore the matching credential before retrying",
                old.display()
            )
        })?;
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
        crate::daemon::restart_after_upgrade(root).await;
    }
    Ok(())
}

/// Compare only parsed values; errors never include credential contents.
fn reconcile(source: &[u8], target: &[u8]) -> Result<Vec<u8>> {
    if source == target {
        return Ok(target.to_vec());
    }
    fn parse(bytes: &[u8]) -> Option<(&str, (u64, u64))> {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let text = std::str::from_utf8(bytes).ok()?;
        let seed = text
            .lines()
            .skip_while(|l| !l.contains("BEGIN USER NKEY SEED"))
            .nth(1)?;
        nkeys::KeyPair::from_seed(seed).ok()?;
        let jwt = text
            .lines()
            .skip_while(|l| !l.contains("BEGIN NATS USER JWT"))
            .nth(1)?;
        let claims: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(jwt.split('.').nth(1)?).ok()?).ok()?;
        Some((seed, (claims["exp"].as_u64()?, claims["iat"].as_u64()?)))
    }
    if let (Some((a, age_a)), Some((b, age_b))) = (parse(source), parse(target)) {
        if a == b {
            return Ok(if age_a > age_b { source } else { target }.to_vec());
        }
    }
    bail!("credential content differs")
}

//! `acs worker run`: start a worker as one identity.
use crate::{agents, exec, local, runtime};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

pub const BACKENDS: &[&str] = &[
    "hub-worker",
    "echo",
    "claude",
    "codex",
    "cursor",
    "hermes",
    "hermes_acp",
    "grok",
    "grok_acp",
    "kilo",
    "kilo_acp",
    "opencode",
    "opencode_acp",
    "agy",
];

/// `dir/.venv/bin/python` when that venv exists, else `python3`.
fn dir_python(dir: &Path) -> PathBuf {
    let py = dir.join(".venv/bin/python");
    if py.is_file() {
        py
    } else {
        "python3".into()
    }
}

/// `(script, python)` for a Python backend. Like `mcp install`, this never
/// runs code from the current directory: the worker gets the identity's
/// credentials. In order: the trusted checkout named by `ACS_WORKER_DIR`, the
/// runtime built into this binary, then the worker bundle.
async fn python_worker(root: &Path, backend: &str) -> Result<(PathBuf, PathBuf)> {
    let python = std::env::var_os("ACS_PYTHON").map(PathBuf::from);
    let file = format!("{backend}_worker.py");
    if let Some(dir) = std::env::var_os("ACS_WORKER_DIR").map(PathBuf::from) {
        let script = dir.join(&file);
        if !script.is_file() {
            bail!("{file} not found in ACS_WORKER_DIR ({})", dir.display());
        }
        return Ok((script, python.unwrap_or_else(|| dir_python(&dir))));
    }
    if runtime::has_worker(backend) {
        return runtime::worker(root, backend, python).await;
    }
    let bundle = local::home()?.join(".local/share/nats-hub/remote");
    let script = bundle.join(&file);
    if script.is_file() {
        return Ok((script, python.unwrap_or_else(|| dir_python(&bundle))));
    }
    bail!(
        "the {backend} worker is not built into acs: set ACS_WORKER_DIR to a trusted \
         checkout, and ACS_PYTHON to an interpreter with its SDK"
    )
}

pub async fn run(root: &Path, backend: &str, identity: &str, args: &[String]) -> Result<()> {
    let state = local::load(root)?;
    let id = agents::select(&state, Some(identity))?;
    if backend != "echo" {
        if let Some(hint) = agents::provider_hint(&state, id, agents::provider_for(backend)) {
            eprintln!("{hint}");
        }
    }
    let mut cmd = if backend == "hub-worker" {
        let path = std::env::current_exe()?.with_file_name("hub-worker");
        tokio::process::Command::new(if path.is_file() {
            path
        } else {
            PathBuf::from("hub-worker")
        })
    } else if BACKENDS.contains(&backend) {
        let (script, python) = python_worker(crate::tenants::device_root(root), backend).await?;
        let mut cmd = tokio::process::Command::new(python);
        cmd.arg(script);
        cmd
    } else {
        bail!("unknown worker backend");
    };
    // Workers take the tenant URL as a flag too (hub-worker's default does
    // not read NATS_URL); extra args follow, so the caller can still override.
    cmd.arg("--identity")
        .arg(id)
        .arg("--nats-url")
        .arg(&state.nats_url)
        .args(args);
    exec::apply(&mut cmd, root, &state, id);
    exec::handoff(cmd).await
}

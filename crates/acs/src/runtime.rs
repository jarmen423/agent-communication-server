//! Canonical Apache-2.0 Python runtimes embedded for standalone installs: the
//! MCP server (`acs mcp install`) and the workers (`acs worker run`).
use crate::local;
use anyhow::{bail, Result};
use fs2::FileExt;
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/mcp_sources.rs"));
include!(concat!(env!("OUT_DIR"), "/worker_sources.rs"));

/// nats-py is pinned for the expiry hook in nats_connect.py (requirements.txt).
const MCP_DEPS: &[&str] = &[
    "nats-py==2.16.0",
    "nkeys>=0.2",
    "aiohttp>=3.9",
    "mcp>=1.19,<2",
];
/// httpx serves the ACP-over-HTTP backends (kilo_acp). Bump the worker venv
/// marker when this list changes so existing venvs pick the change up.
const WORKER_DEPS: &[&str] = &[
    "nats-py==2.16.0",
    "nkeys>=0.2",
    "aiohttp>=3.9",
    "httpx>=0.27",
];
const WORKER_VENV_MARKER: &str = "ready-v1";

/// Create `base/.venv` once and install `deps`; `marker` records success.
async fn venv(base: &Path, deps: &[&str], marker: &str, what: &str) -> Result<PathBuf> {
    let dir = base.join(".venv");
    let python = dir.join("bin/python");
    if !python.is_file() {
        eprintln!("Creating a private Python venv in {}", dir.display());
        let created = tokio::process::Command::new("python3")
            .arg("-m")
            .arg("venv")
            .arg(&dir)
            .status()
            .await
            .is_ok_and(|s| s.success());
        if !created {
            // A half-made venv (no ensurepip, say) would fail every retry.
            let _ = tokio::fs::remove_dir_all(&dir).await;
            bail!("Python venv creation failed; install Python 3.10+ and its venv package");
        }
        // A rebuilt venv (its interpreter vanished in a Python upgrade, say)
        // has none of the packages an earlier marker vouched for.
        let _ = tokio::fs::remove_file(base.join(marker)).await;
    }
    if !base.join(marker).exists() {
        eprintln!(
            "Installing {what} Python dependencies in {}",
            base.display()
        );
        if !tokio::process::Command::new(&python)
            .args(["-m", "pip", "install", "--disable-pip-version-check"])
            .args(deps)
            .status()
            .await?
            .success()
        {
            bail!("{what} dependency installation failed; check network access and retry");
        }
        local::atomic_private(&base.join(marker), b"ready")?;
    }
    Ok(python)
}

pub async fn install(root: &Path, external_python: Option<PathBuf>) -> Result<(PathBuf, PathBuf)> {
    let base = root.join("mcp");
    let server = base.join("server");
    for (name, bytes) in SOURCES {
        local::atomic_private(&server.join(name), bytes)?;
    }
    if let Some(python) = external_python {
        let python = if python.is_file() && !python.is_absolute() {
            std::env::current_dir()?.join(python)
        } else {
            python
        };
        return Ok((server.join("nats_hub_mcp.py"), python));
    }
    let python = venv(&base, MCP_DEPS, "ready-v1", "MCP").await?;
    Ok((server.join("nats_hub_mcp.py"), python))
}

/// Whether `<backend>_worker.py` is built into this binary.
pub fn has_worker(backend: &str) -> bool {
    let name = format!("{backend}_worker.py");
    WORKER_SOURCES.iter().any(|(rel, _)| *rel == name)
}

/// Materialize the embedded workers into `ACS_HOME/worker/<version>-<hash>/`
/// and return `(script, python)`. The directory is private and named after
/// the embedded code, so an upgraded `acs` never mixes old and new modules.
/// Without `external_python` a private venv is shared across versions.
pub async fn worker(
    root: &Path,
    backend: &str,
    external_python: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf)> {
    let base = root.join("worker");
    std::fs::create_dir_all(&base)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))?;
    }
    // Two first runs at once must not build one venv twice.
    let lock = base.join("provision.lock");
    let _lock = tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(lock)?;
        file.lock_exclusive()?;
        Ok(file)
    })
    .await??;
    let dir = base.join(WORKER_TAG);
    for (rel, bytes) in WORKER_SOURCES {
        let path = dir.join(rel);
        // Rewrite anything that differs from the binary's copy.
        if std::fs::read(&path).ok().as_deref() != Some(*bytes) {
            local::atomic_private(&path, bytes)?;
        }
    }
    let python = match external_python {
        Some(python) => python,
        None => venv(&base, WORKER_DEPS, WORKER_VENV_MARKER, "worker").await?,
    };
    Ok((dir.join(format!("{backend}_worker.py")), python))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeds_worker_entrypoints_and_their_imports() {
        for backend in ["echo", "claude", "codex", "hermes", "kilo_acp", "opencode"] {
            assert!(has_worker(backend), "{backend} is not embedded");
        }
        assert!(
            !has_worker("cursor"),
            "cursor needs cursor-sdk; checkout only"
        );
        let names: Vec<_> = WORKER_SOURCES.iter().map(|(rel, _)| *rel).collect();
        for module in [
            "worker_runtime.py",
            "worker_events.py",
            "nats_connect.py",
            "hub_delivery.py",
            "worker_backends/__init__.py",
            "worker_backends/claude_code.py",
            "worker_backends/codex_cli.py",
            "worker_backends/proc.py",
        ] {
            assert!(names.contains(&module), "{module} missing from {names:?}");
        }
        assert!(!names
            .iter()
            .any(|n| n.starts_with("mcp_server/") || n.contains("..")));
        assert!(WORKER_TAG.starts_with(env!("CARGO_PKG_VERSION")));
    }
}

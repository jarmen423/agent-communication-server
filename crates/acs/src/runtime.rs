//! Canonical Apache-2.0 Python MCP runtime embedded for standalone installs.
use crate::local;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/mcp_sources.rs"));

pub async fn install(root: &Path, external_python: Option<PathBuf>) -> Result<(PathBuf, PathBuf)> {
    let base = root.join("mcp");
    let server = base.join("server");
    for (name, bytes) in SOURCES {
        local::atomic(&server.join(name), bytes)?;
    }
    if let Some(python) = external_python {
        let python = if python.is_file() && !python.is_absolute() {
            std::env::current_dir()?.join(python)
        } else {
            python
        };
        return Ok((server.join("nats_hub_mcp.py"), python));
    }
    let python = base.join(".venv/bin/python");
    if !python.is_file()
        && !tokio::process::Command::new("python3")
            .arg("-m")
            .arg("venv")
            .arg(base.join(".venv"))
            .status()
            .await?
            .success()
    {
        bail!("Python venv creation failed; install Python 3.10+ and its venv package");
    }
    if !base.join("ready-v1").exists() {
        eprintln!("Installing MCP Python dependencies in {}", base.display());
        if !tokio::process::Command::new(&python)
            .args([
                "-m",
                "pip",
                "install",
                "--disable-pip-version-check",
                "nats-py==2.16.0",
                "nkeys>=0.2",
                "aiohttp>=3.9",
                "mcp>=1.19,<2",
            ])
            .status()
            .await?
            .success()
        {
            bail!("MCP dependency installation failed; retry acs mcp install");
        }
        local::atomic(&base.join("ready-v1"), b"ready")?;
    }
    Ok((server.join("nats_hub_mcp.py"), python))
}

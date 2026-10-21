//! Canonical Apache-2.0 Python MCP runtime embedded for standalone installs.
use crate::local;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

const SOURCES: &[(&str, &[u8])] = &[
    (
        "hub_annotations.py",
        include_bytes!("../../../mcp_server/hub_annotations.py"),
    ),
    (
        "hub_app.py",
        include_bytes!("../../../mcp_server/hub_app.py"),
    ),
    (
        "hub_buffers.py",
        include_bytes!("../../../mcp_server/hub_buffers.py"),
    ),
    (
        "hub_connection.py",
        include_bytes!("../../../mcp_server/hub_connection.py"),
    ),
    (
        "hub_delivery.py",
        include_bytes!("../../../mcp_server/hub_delivery.py"),
    ),
    (
        "hub_handlers.py",
        include_bytes!("../../../mcp_server/hub_handlers.py"),
    ),
    (
        "hub_primitives.py",
        include_bytes!("../../../mcp_server/hub_primitives.py"),
    ),
    (
        "hub_providers.py",
        include_bytes!("../../../mcp_server/hub_providers.py"),
    ),
    (
        "hub_queries.py",
        include_bytes!("../../../mcp_server/hub_queries.py"),
    ),
    (
        "hub_query_tools.py",
        include_bytes!("../../../mcp_server/hub_query_tools.py"),
    ),
    (
        "hub_sessions.py",
        include_bytes!("../../../mcp_server/hub_sessions.py"),
    ),
    (
        "hub_tools.py",
        include_bytes!("../../../mcp_server/hub_tools.py"),
    ),
    (
        "hub_validate.py",
        include_bytes!("../../../mcp_server/hub_validate.py"),
    ),
    (
        "nats_connect.py",
        include_bytes!("../../../mcp_server/nats_connect.py"),
    ),
    (
        "nats_hub_mcp.py",
        include_bytes!("../../../mcp_server/nats_hub_mcp.py"),
    ),
];

pub async fn install(root: &Path) -> Result<(PathBuf, PathBuf)> {
    let base = root.join("mcp");
    let server = base.join("server");
    for (name, bytes) in SOURCES {
        local::atomic(&server.join(name), bytes)?;
    }
    let python = base.join(".venv/bin/python");
    if !python.is_file() {
        if !tokio::process::Command::new("python3")
            .arg("-m")
            .arg("venv")
            .arg(base.join(".venv"))
            .status()
            .await?
            .success()
        {
            bail!("Python venv creation failed; install Python 3.10+ and its venv package");
        }
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

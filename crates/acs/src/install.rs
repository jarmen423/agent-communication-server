//! Merge just the nats-hub MCP entry; preserve unrelated client settings.
use crate::{
    agents,
    local::{self, State},
};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn select<'a>(state: &'a State, identity: Option<&'a str>) -> Result<&'a str> {
    let id = match identity {
        Some(id) => id,
        None if state.agents.len() == 1 => state.agents.keys().next().unwrap(),
        None => {
            bail!("choose an identity with --identity (required unless exactly one is configured)")
        }
    };
    agents::check_id(id)?;
    if !state.agents.contains_key(id) {
        bail!("identity is not configured on this device");
    }
    Ok(id)
}

async fn runtime(
    root: &Path,
    server: Option<PathBuf>,
    python: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf)> {
    // Checkout/bundle discovery is runtime-only: never bake a build-machine path
    // into the distributed binary. Released binaries accept ACS_MCP_SERVER.
    let exe = std::env::current_exe()?;
    let cwd = std::env::current_dir()?;
    let roots = [
        Some(cwd),
        exe.parent().map(Path::to_path_buf),
        local::home()
            .ok()
            .map(|h| h.join(".local/share/nats-hub/remote")),
    ];
    let discovered = server.or_else(|| {
        roots.iter().flatten().find_map(|r| {
            [
                r.join("mcp_server/nats_hub_mcp.py"),
                r.join("server/nats_hub_mcp.py"),
            ]
            .into_iter()
            .find(|p| p.is_file())
        })
    });
    let Some(server) = discovered else {
        return crate::runtime::install(root, python).await;
    };
    let server = server.canonicalize()?;
    let python = python
        .or_else(|| {
            server
                .parent()?
                .parent()
                .map(|p| p.join(".venv/bin/python"))
                .filter(|p| p.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("python3"));
    // Preserve a venv interpreter's symlink: canonicalizing it selects the
    // system interpreter and silently drops the venv's installed packages.
    let python = if python.is_file() && !python.is_absolute() {
        std::env::current_dir()?.join(python)
    } else {
        python
    };
    Ok((server, python))
}

pub async fn mcp(
    root: &Path,
    client: &str,
    identity: Option<&str>,
    home: Option<PathBuf>,
    server: Option<PathBuf>,
    python: Option<PathBuf>,
) -> Result<()> {
    let state = local::load(root)?;
    let id = select(&state, identity)?;
    let (server, python) = runtime(root, server, python).await?;
    let home = match home {
        Some(path) => path,
        None => local::home()?,
    };
    let env = json!({"NATS_URL": state.nats_url,
        "NATS_CREDENTIALS_FILE": local::creds_path(root, id), "NATS_HUB_IDENTITY": id});
    let config = json!({"command": python, "args": [server], "env": env});
    let path = match client {
        "claude" => {
            let path = home.join(".claude.json");
            let mut doc: Value = if path.exists() {
                serde_json::from_slice(&std::fs::read(&path)?)?
            } else {
                json!({})
            };
            let obj = doc
                .as_object_mut()
                .context("Claude config must be an object")?;
            let servers = obj
                .entry("mcpServers")
                .or_insert(json!({}))
                .as_object_mut()
                .context("mcpServers must be an object")?;
            servers.insert("nats-hub".into(), config);
            local::atomic(&path, &serde_json::to_vec_pretty(&doc)?)?;
            path
        }
        "codex" => {
            let path = home.join(".codex/config.toml");
            let text = if path.exists() {
                std::fs::read_to_string(&path)?
            } else {
                String::new()
            };
            let mut doc: toml_edit::DocumentMut = text.parse()?;
            let mut table = toml_edit::Table::new();
            table["command"] = toml_edit::value(python.to_string_lossy().as_ref());
            let mut args = toml_edit::Array::new();
            args.push(server.to_string_lossy().as_ref());
            table["args"] = toml_edit::value(args);
            let mut vars = toml_edit::Table::new();
            for (key, value) in env.as_object().unwrap() {
                vars[key] = toml_edit::value(value.as_str().unwrap());
            }
            table["env"] = toml_edit::Item::Table(vars);
            if doc.get("mcp_servers").is_none() {
                doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            let servers = doc["mcp_servers"]
                .as_table_like_mut()
                .context("mcp_servers must be a table")?;
            servers.insert("nats-hub", toml_edit::Item::Table(table));
            local::atomic(&path, doc.to_string().as_bytes())?;
            path
        }
        "hermes" => {
            let path = home.join(".hermes/config.yaml");
            let mut doc: serde_yaml::Value = if path.exists() {
                serde_yaml::from_slice(&std::fs::read(&path)?)?
            } else {
                serde_yaml::Value::Mapping(Default::default())
            };
            let object = doc
                .as_mapping_mut()
                .context("Hermes config must be a mapping")?;
            let servers = object
                .entry(serde_yaml::Value::String("mcp_servers".into()))
                .or_insert(serde_yaml::Value::Mapping(Default::default()))
                .as_mapping_mut()
                .context("mcp_servers must be a mapping")?;
            servers.insert(
                serde_yaml::Value::String("nats-hub".into()),
                serde_yaml::to_value(config)?,
            );
            local::atomic(&path, serde_yaml::to_string(&doc)?.as_bytes())?;
            path
        }
        _ => bail!("client must be claude, codex or hermes"),
    };
    println!("Installed nats-hub MCP for {client}: {}", path.display());
    Ok(())
}

pub async fn worker(root: &Path, backend: &str, identity: &str, args: &[String]) -> Result<()> {
    let state = local::load(root)?;
    let id = select(&state, Some(identity))?;
    let mut cmd = if backend == "hub-worker" {
        let path = std::env::current_exe()?.with_file_name("hub-worker");
        let mut cmd = tokio::process::Command::new(if path.is_file() {
            path
        } else {
            PathBuf::from("hub-worker")
        });
        cmd.arg("--identity").arg(id);
        cmd
    } else {
        if ![
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
        ]
        .contains(&backend)
        {
            bail!("unknown worker backend");
        }
        let repo = std::env::var_os("ACS_WORKER_DIR")
            .map(PathBuf::from)
            .unwrap_or(std::env::current_dir()?);
        let script = repo.join(format!("{backend}_worker.py"));
        if !script.is_file() {
            bail!("worker runtime not found; set ACS_WORKER_DIR to its directory");
        }
        let py = std::env::var_os("ACS_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let py = repo.join(".venv/bin/python");
                if py.is_file() {
                    py
                } else {
                    "python3".into()
                }
            });
        let mut cmd = tokio::process::Command::new(py);
        cmd.arg(script).arg("--identity").arg(id);
        cmd
    };
    cmd.args(args)
        .env("NATS_URL", &state.nats_url)
        .env("NATS_CREDENTIALS_FILE", local::creds_path(root, id))
        .env("NATS_HUB_IDENTITY", id)
        .env_remove("NATS_USER")
        .env_remove("NATS_PASSWORD")
        .env_remove("NATS_TOKEN")
        .env_remove("NATS_NKEY")
        .env_remove("NATS_NKEYS_SEED")
        .env_remove("ACS_SERVICE_KEY");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec transfers signals and exit status directly to the worker.
        Err(cmd.as_std_mut().exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status().await?;
        if !status.success() {
            bail!("worker exited: {status}");
        }
        Ok(())
    }
}

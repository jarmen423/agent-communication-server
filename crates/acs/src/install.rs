//! Merge just the nats-hub MCP entry; preserve unrelated client settings.
use crate::{agents, exec, local};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

async fn runtime(
    root: &Path,
    server: Option<PathBuf>,
    python: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf)> {
    // Checkout/bundle discovery is runtime-only: never bake a build-machine path
    // into the distributed binary. Released binaries accept ACS_MCP_SERVER.
    // The current directory is deliberately NOT searched: the discovered
    // interpreter + script are written into the agent's MCP config and run on
    // every session with the identity's credentials, so running `acs mcp
    // install` inside an untrusted checkout must not adopt its code.
    let exe = std::env::current_exe()?;
    let roots = [
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
    let mut state = local::load(root)?;
    let provider = agents::provider_for(client);
    // Naming an identity this device doesn't have yet registers it, recording
    // the client as its agent (origin.agent on every message it sends).
    if let Some(new) = identity.filter(|id| !state.agents.contains_key(*id)) {
        agents::add(root, new, "orchestrator", provider).await?;
        state = local::load(root)?;
    } else if let Some(hint) = agents::select(&state, identity)
        .ok()
        .and_then(|id| agents::provider_hint(&state, id, provider))
    {
        eprintln!("{hint}");
    }
    let id = agents::select(&state, identity)?;
    let d = crate::tenants::device(root)?;
    let name = if d["tenants"].as_array().is_some_and(|ts| {
        ts.iter().any(|t| {
            t["tenant"] == state.tenant && (t["personal"] == true || t["kind"] == "personal")
        })
    }) {
        "nats-hub".to_owned()
    } else {
        format!("nats-hub-{}", state.tenant)
    };
    let (server, python) = runtime(crate::tenants::device_root(root), server, python).await?;
    let home = match home {
        Some(path) => path,
        None => local::home()?,
    };
    let vars = exec::vars(root, &state, id);
    let env: serde_json::Map<String, Value> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect();
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
            servers.insert(name.clone(), config);
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
            let mut table_env = toml_edit::Table::new();
            for (key, value) in &vars {
                table_env[*key] = toml_edit::value(value.as_str());
            }
            table["env"] = toml_edit::Item::Table(table_env);
            if doc.get("mcp_servers").is_none() {
                doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            let servers = doc["mcp_servers"]
                .as_table_like_mut()
                .context("mcp_servers must be a table")?;
            servers.insert(&name, toml_edit::Item::Table(table));
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
                serde_yaml::Value::String(name.clone()),
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

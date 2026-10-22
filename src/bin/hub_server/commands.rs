use anyhow::{Context, Result};
use nats_hub::{Storage, SurrealStorage};

/// `hub-server backup`: ask the running hub-server to export its DB.
pub(super) async fn run_backup(
    nats_url: &str,
    identity: Option<&str>,
    name: Option<&str>,
) -> Result<()> {
    let name = name
        .map(String::from)
        .unwrap_or_else(|| format!("hub-{}.surql", chrono::Utc::now().format("%Y%m%dT%H%M%SZ")));
    let api = match identity {
        Some(id) => nats_hub::ApiClient::connect_with_identity(nats_url, id).await?,
        None => nats_hub::ApiClient::connect(nats_url).await?,
    };
    let resp = api
        .request("backup.export", serde_json::json!({ "name": name }))
        .await?;
    let path = resp.get("path").and_then(|v| v.as_str()).unwrap_or("?");
    let bytes = resp.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0);
    println!("backup written: {path} ({bytes} bytes)");
    Ok(())
}

/// `hub-server restore`: import an export into a fresh DB path. The file
/// carries its own schema (`DEFINE TABLE`/`FIELD`/`INDEX`), so the path
/// must be fresh — then `migrate()` runs to normalize whatever the export
/// predates.
#[cfg(feature = "storage-surreal")]
pub(super) async fn run_restore(db_path: &str, from: &std::path::Path) -> Result<()> {
    let p = std::path::Path::new(db_path);
    let non_empty = p.is_file()
        || (p.is_dir()
            && std::fs::read_dir(p)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false));
    anyhow::ensure!(
        !non_empty,
        "--db-path '{db_path}' is not empty; restore requires a fresh path"
    );
    let storage = SurrealStorage::connect(db_path)
        .await
        .with_context(|| format!("failed to open SurrealDB at '{db_path}'"))?;
    nats_hub::storage::backup::import_from(&storage.db(), from).await?;
    storage
        .migrate()
        .await
        .context("post-restore migration failed")?;
    let counts = nats_hub::storage::backup::table_counts(&storage.db()).await?;
    println!("restored '{}' into '{db_path}':", from.display());
    for (table, n) in counts {
        println!("  {table:<12} {n}");
    }
    Ok(())
}

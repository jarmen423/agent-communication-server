//! Static file server for the visualizer (plain HTTP/1.1 on the same
//! listener as the WebSocket endpoint).

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub(super) async fn handle_http(
    mut stream: TcpStream,
    path: &str,
    static_dir: &Arc<Option<PathBuf>>,
) -> Result<()> {
    let mut buf = [0u8; 4096];
    let _ = stream.read(&mut buf).await;

    let dir = match static_dir.as_ref() {
        Some(d) => d,
        None => {
            let body = "Static file serving disabled. Use --static-dir.";
            write_http_response(&mut stream, 404, "Not Found", "text/plain", body.as_bytes())
                .await?;
            return Ok(());
        }
    };

    let file_path = if path == "/" {
        dir.join("index.html")
    } else {
        dir.join(path.trim_start_matches('/'))
    };

    if !file_path.starts_with(dir) {
        write_http_response(
            &mut stream,
            403,
            "Forbidden",
            "text/plain",
            b"Path traversal denied",
        )
        .await?;
        return Ok(());
    }

    if file_path.exists() {
        let content = tokio::fs::read(&file_path).await?;
        let mime = mime_type(&file_path);
        write_http_response(&mut stream, 200, "OK", mime, &content).await?;
    } else {
        write_http_response(
            &mut stream,
            404,
            "Not Found",
            "text/plain",
            b"404 Not Found",
        )
        .await?;
    }

    Ok(())
}

async fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    // no-cache so visualizer HTML edits show up without fighting browser cache
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store, max-age=0\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    Ok(())
}

fn mime_type(path: &PathBuf) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

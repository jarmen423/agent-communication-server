//! Static file server for the visualizer (plain HTTP/1.1 on the same
//! listener as the WebSocket endpoint).
//!
//! Path resolution is traversal-safe: the request path is percent-decoded,
//! any `..` component or NUL byte is rejected outright, and the result must
//! stay inside the canonicalized static root even through symlinks.

use anyhow::Result;
use std::path::{Component, Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::BridgeState;

/// Resolve a URL path to a file inside `root`.
///
/// `root` must already be canonicalized. Returns `None` when the path is
/// malformed, contains a `..` component or NUL byte (raw or percent-encoded),
/// or resolves outside `root` (e.g. through a symlink). A path to a
/// non-existent file inside `root` returns `Some` — callers turn that into
/// a 404 rather than a 403.
pub fn resolve_static_path(root: &Path, url_path: &str) -> Option<PathBuf> {
    // Strip query/fragment — the visualizer is opened as `/?token=…`.
    let end = url_path.find(['?', '#']).unwrap_or(url_path.len());
    let decoded = percent_decode(&url_path[..end]);
    if decoded.contains('\0') || decoded.contains('\\') {
        return None;
    }
    let rel = decoded.trim_start_matches('/');
    let rel_path = Path::new(if rel.is_empty() { "index.html" } else { rel });

    // Any `..` component is traversal. Component::CurDir is skipped because
    // Path normalizes interior `.` away and a leading one cannot escape root.
    if rel_path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }

    let resolved = canonicalize_existing(&root.join(rel_path))?;
    if !resolved.starts_with(root) {
        return None;
    }
    Some(resolved)
}

/// Canonicalize the longest existing ancestor of `path` (defeating symlink
/// escapes), then re-append the non-existent tail lexically. `..` components
/// must already be rejected by the caller.
fn canonicalize_existing(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cur = Some(path);
    while let Some(p) = cur {
        if let Ok(canon) = p.canonicalize() {
            let mut out = canon;
            for comp in tail.iter().rev() {
                out.push(comp);
            }
            return Some(out);
        }
        tail.push(p.file_name()?);
        cur = p.parent();
    }
    None
}

/// Decode `%XX` sequences; malformed sequences pass through literally.
/// Shared with `ws.rs` for the `?token=` query value.
pub(super) fn percent_decode(s: &str) -> String {
    if !s.as_bytes().contains(&b'%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let decoded = if b[i] == b'%' && i + 2 < b.len() {
            match (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                (Some(h), Some(l)) => Some(h * 16 + l),
                _ => None,
            }
        } else {
            None
        };
        match decoded {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub(super) async fn handle_http(
    mut stream: TcpStream,
    path: &str,
    state: &BridgeState,
) -> Result<()> {
    let mut buf = [0u8; 4096];
    let _ = stream.read(&mut buf).await;

    let root = match &state.static_root {
        Some(d) => d,
        None => {
            let body = "Static file serving disabled. Use --static-dir.";
            write_http_response(&mut stream, 404, "Not Found", "text/plain", body.as_bytes())
                .await?;
            return Ok(());
        }
    };

    let file_path = match resolve_static_path(root, path) {
        Some(p) => p,
        None => {
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
    };

    if file_path.is_file() {
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

fn mime_type(path: &Path) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture {
        root: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let root = base.join("static");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("index.html"), "<h1>viz</h1>").unwrap();
        fs::write(base.join("secret.txt"), "top secret").unwrap();
        Fixture { root, _dir: dir }
    }

    #[test]
    fn serves_index_and_ordinary_files() {
        let f = fixture();
        assert_eq!(
            resolve_static_path(&f.root, "/").unwrap(),
            f.root.join("index.html")
        );
        assert_eq!(
            resolve_static_path(&f.root, "/index.html").unwrap(),
            f.root.join("index.html")
        );
        // Query strings (e.g. /?token=…) are ignored for resolution.
        assert_eq!(
            resolve_static_path(&f.root, "/?token=abc").unwrap(),
            f.root.join("index.html")
        );
        // Missing files still resolve (caller turns them into 404).
        assert_eq!(
            resolve_static_path(&f.root, "/missing.js").unwrap(),
            f.root.join("missing.js")
        );
    }

    #[test]
    fn rejects_dotdot_traversal() {
        let f = fixture();
        for p in [
            "/../secret.txt",
            "/../../etc/passwd",
            "/a/b/../../../etc/passwd",
            "/sub/../index.html",
            "/..",
        ] {
            assert_eq!(resolve_static_path(&f.root, p), None, "path: {p}");
        }
    }

    #[test]
    fn rejects_encoded_traversal_and_nul() {
        let f = fixture();
        for p in [
            "/%2e%2e/secret.txt",
            "/%2e%2e%2f%2e%2e%2fetc%2fpasswd",
            "/%2E%2E/secret.txt",
            "/..%2fsecret.txt",
            "/%2e%2e%5cetc%5cpasswd",
            "/%00",
            "/index.html%00",
        ] {
            assert_eq!(resolve_static_path(&f.root, p), None, "path: {p}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let f = fixture();
        std::os::unix::fs::symlink(f.root.parent().unwrap(), f.root.join("link")).unwrap();
        assert_eq!(resolve_static_path(&f.root, "/link/secret.txt"), None);
    }
}

//! Bridge startup configuration helpers — kept in the library (not
//! `hub_server.rs`) so the Origin allowlist defaults and the bind guard are
//! unit-testable without a listener.

use anyhow::Result;
use std::net::ToSocketAddrs;

/// Origins allowed by default for a bridge bound at `ws_addr`.
///
/// Always contains `http://<ws_addr>` as written. When the bind resolves to
/// a loopback or wildcard address (`localhost`, `127.0.0.1`, `[::1]`,
/// `0.0.0.0`, `[::]`) all loopback spellings are added — a browser may show
/// any of them regardless of which the operator typed. A routable bind adds
/// its resolved IPs plus `localhost` (same-box browsing still works).
pub fn default_allowed_origins(ws_addr: &str) -> Vec<String> {
    let mut out = vec![format!("http://{ws_addr}")];
    let addrs: Vec<_> = match ws_addr.to_socket_addrs() {
        Ok(it) => it.collect(),
        Err(_) => return out,
    };
    for a in addrs {
        let port = a.port();
        let ip = a.ip();
        if ip.is_unspecified() || ip.is_loopback() {
            out.push(format!("http://localhost:{port}"));
            out.push(format!("http://127.0.0.1:{port}"));
            out.push(format!("http://[::1]:{port}"));
        } else {
            out.push(format!("http://{ip}:{port}"));
            out.push(format!("http://localhost:{port}"));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|o| seen.insert(o.clone()));
    out
}

/// Canonical form of an `Origin` value (or allowlist entry) for comparison:
/// lowercase scheme + host, trailing `/` removed, and a port equal to the
/// scheme's default dropped — so `http://host`, `http://host/` and
/// `http://host:80` all compare equal.
pub fn normalize_origin(origin: &str) -> String {
    let o = origin.trim().trim_end_matches('/');
    let (scheme, rest) = match o.split_once("://") {
        Some(pair) => pair,
        None => return o.to_ascii_lowercase(),
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    // Split host and port; an IPv6 literal keeps its brackets.
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        match v6.split_once("]:") {
            Some((h, p)) => (format!("[{h}]"), p.to_string()),
            None => (authority.to_string(), String::new()),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.to_string()),
            None => (authority.to_string(), String::new()),
        }
    };
    let is_default_port = matches!(
        (scheme, port.as_str()),
        ("http" | "ws", "80") | ("https" | "wss", "443")
    );
    let auth = if port.is_empty() || is_default_port {
        host
    } else {
        format!("{host}:{port}")
    };
    format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        auth.to_ascii_lowercase()
    )
}

/// Enforce the tokenless-bridge bind policy: a bridge with no `token` may
/// only bind a loopback address — it reads all bus traffic and can spawn
/// workers, so an exposed unauthenticated listener is a critical hole.
/// `insecure` opts out explicitly for trusted networks.
pub fn check_ws_bind(ws_addr: &str, token: Option<&str>, insecure: bool) -> Result<()> {
    if token.is_none() && !insecure && !is_loopback_addr(ws_addr) {
        anyhow::bail!(
            "refusing to start: --ws-addr {ws_addr} is not a loopback address and no \
             --ws-token (or HUB_WS_TOKEN) is set. Set a token, or pass --ws-insecure to \
             run unauthenticated on a trusted network."
        );
    }
    Ok(())
}

/// Percent-encode `s` for use as a query value (RFC 3986 unreserved set
/// kept, everything else %XX-encoded). Used for the token in the startup
/// banner so tokens containing `+`, `/`, `=` etc. stay literal — a raw `+`
/// would be decoded as a space by `URLSearchParams`.
pub fn url_query_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// True when `addr` resolves only to loopback IPs (`127.0.0.1:9191`,
/// `localhost:9191`, `[::1]:9191`). Unresolvable, empty, or partially
/// non-loopback answers are false — the safe choice for the bind guard.
pub fn is_loopback_addr(addr: &str) -> bool {
    let addrs: Vec<_> = match addr.to_socket_addrs() {
        Ok(it) => it.collect(),
        Err(_) => return false,
    };
    !addrs.is_empty() && addrs.iter().all(|a| a.ip().is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_origins_cover_loopback_spellings() {
        for bind in ["localhost:9191", "127.0.0.1:9191", "[::1]:9191"] {
            let got = default_allowed_origins(bind);
            assert!(
                got.contains(&"http://127.0.0.1:9191".to_string()),
                "{bind} missing 127.0.0.1: {got:?}"
            );
            assert!(got.contains(&"http://localhost:9191".to_string()), "{bind}");
        }
    }

    #[test]
    fn default_origins_wildcard_cover_loopback() {
        let got = default_allowed_origins("0.0.0.0:9191");
        for want in [
            "http://0.0.0.0:9191",
            "http://localhost:9191",
            "http://127.0.0.1:9191",
            "http://[::1]:9191",
        ] {
            assert!(got.contains(&want.to_string()), "missing {want}: {got:?}");
        }
    }

    #[test]
    fn normalize_origin_canonicalizes() {
        // trailing slash, default port, case
        assert_eq!(normalize_origin("http://A.COM/"), "http://a.com");
        assert_eq!(normalize_origin("http://a.com:80"), "http://a.com");
        assert_eq!(normalize_origin("https://a.com:443"), "https://a.com");
        assert_eq!(normalize_origin("http://a.com:8080"), "http://a.com:8080");
        assert_eq!(normalize_origin("http://[::1]:9191/"), "http://[::1]:9191");
        // ws/wss schemes also normalize on their default ports
        assert_eq!(normalize_origin("ws://a.com:80"), "ws://a.com");
    }

    #[test]
    fn bind_guard() {
        assert!(check_ws_bind("127.0.0.1:1", None, false).is_ok());
        assert!(check_ws_bind("0.0.0.0:1", Some("t"), false).is_ok());
        assert!(check_ws_bind("0.0.0.0:1", None, true).is_ok());
        assert!(check_ws_bind("0.0.0.0:1", None, false).is_err());
    }

    #[test]
    fn url_query_encode_escapes_specials() {
        assert_eq!(url_query_encode("a+b=c d"), "a%2Bb%3Dc%20d");
        assert_eq!(url_query_encode("plain-token_1.2~x"), "plain-token_1.2~x");
        assert_eq!(url_query_encode("a/b?c"), "a%2Fb%3Fc");
    }
}

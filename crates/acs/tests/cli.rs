use std::{
    path::Path,
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

fn acs(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_acs"))
        .args(args)
        .env("ACS_HOME", home)
        .env("ACS_NO_KEYRING", "1")
        .env_remove("ACS_SERVER")
        .output()
        .unwrap()
}

/// A logged-in device without a control plane: `me` is current, `old` lapsed.
fn device() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let state = serde_json::json!({
        "server": "https://acs.example", "tenant": "T", "device": "d",
        "nats_url": "wss://nats.example:443", "access_token": "token-must-not-print",
        "access_expires_at": 0, "agents": {
            "me": {"role": "orchestrator", "public_key": "U1", "expires_at": now + 23 * 3600 + 60},
            "old": {"role": "worker", "public_key": "U2", "expires_at": now - 600}}});
    std::fs::write(root.path().join("state.json"), state.to_string()).unwrap();
    root
}

#[test]
fn credentials_are_never_echoed_in_parser_errors_or_status() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_acs"))
        .args([
            "login",
            "--server",
            "http://foreign.example",
            "--key",
            "secret-service-key",
        ])
        .env("ACS_HOME", root.path())
        .env("ACS_NO_KEYRING", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("HTTPS"));
    assert!(!text.contains("secret-service-key"));
}

#[test]
fn login_defaults_to_the_hosted_server() {
    let root = tempfile::tempdir().unwrap();
    let help = acs(root.path(), &["login", "--help"]);
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(
        text.contains("[default: https://acs.agentmemorylabs.com]"),
        "{text}"
    );
}

#[test]
fn status_shows_expiry_as_rfc3339_and_relative_time() {
    let root = device();
    let out = acs(root.path(), &["status"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let me = text.lines().find(|l| l.starts_with("me:")).unwrap();
    assert!(
        me.contains("credential expires 20") && me.ends_with("Z (in 23h)"),
        "{me}"
    );
    let old = text.lines().find(|l| l.starts_with("old:")).unwrap();
    assert!(old.ends_with("Z (10m ago)"), "{old}");
    assert!(!text.contains("token-must-not-print"));
}

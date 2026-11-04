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
    std::fs::create_dir_all(root.path().join("tenants/T")).unwrap();
    std::fs::write(
        root.path().join("tenants/T/state.json"),
        serde_json::json!({"nats_url":state["nats_url"], "agents":state["agents"]}).to_string(),
    )
    .unwrap();
    let mut device = state;
    device.as_object_mut().unwrap().remove("agents");
    device["tenants"] = serde_json::json!([{"tenant":"T","personal":true}]);
    std::fs::write(root.path().join("device.json"), device.to_string()).unwrap();
    std::fs::write(root.path().join("current"), "T").unwrap();
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

#[test]
fn env_prints_eval_ready_exports_for_one_identity() {
    let root = device();
    let out = acs(root.path(), &["env", "me"]);
    assert!(out.status.success());
    let script = String::from_utf8(out.stdout).unwrap();
    let creds = root
        .path()
        .canonicalize()
        .unwrap()
        .join("tenants/T/creds/me.creds");
    for line in [
        "export NATS_URL='wss://nats.example:443'".to_owned(),
        format!("export NATS_CREDENTIALS_FILE='{}'", creds.display()),
        "export NATS_HUB_IDENTITY='me'".to_owned(),
    ] {
        assert!(script.lines().any(|l| l == line), "{line} not in {script}");
    }
    // The output really is eval-able by a POSIX shell, and clears stray auth.
    let shell = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "{script}\nprintf '%s|%s|%s' \"$NATS_URL\" \"$NATS_HUB_IDENTITY\" \"${{NATS_TOKEN-unset}}\""
        ))
        .env("NATS_TOKEN", "stale")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&shell.stdout),
        "wss://nats.example:443|me|unset"
    );
    assert!(!acs(root.path(), &["env", "nobody"]).status.success());
}

#[test]
fn exec_runs_a_command_as_the_identity_and_keeps_its_exit_status() {
    let root = device();
    let out = Command::new(env!("CARGO_BIN_EXE_acs"))
        .args(["exec", "old", "--", "sh", "-c"])
        .arg("printf '%s|%s|%s|%s' \"$NATS_URL\" \"$NATS_HUB_IDENTITY\" \"${NATS_TOKEN-unset}\" \"${ACS_SERVICE_KEY-unset}\"; exit 7")
        .env("ACS_HOME", root.path())
        .env("NATS_TOKEN", "stale")
        .env("NATS_USER", "stale")
        .env("ACS_SERVICE_KEY", "must-not-be-inherited")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "wss://nats.example:443|old|unset|unset"
    );
    // `old` lapsed: the caller is told how to fix it.
    assert!(String::from_utf8_lossy(&out.stderr).contains("acs up"));
    assert!(!acs(root.path(), &["exec", "me"]).status.success());
    let missing = acs(
        root.path(),
        &["exec", "me", "--", "no-such-command-acs-test"],
    );
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("cannot run no-such-command-acs-test")
    );
}

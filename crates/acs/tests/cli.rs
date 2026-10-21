use std::process::Command;

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

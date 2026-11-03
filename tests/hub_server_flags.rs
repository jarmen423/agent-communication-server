//! `hub-server` refuses bad provenance flags at startup, before it connects
//! to anything, so these need no NATS server.

use std::process::{Command, Output};

fn hub_server(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hub-server"));
    cmd.args(["--nats-url", "nats://127.0.0.1:1", "--db-path", ""])
        .args(args)
        .env_remove("NATS_HUB_TENANT")
        .env_remove("NATS_HUB_MANIFEST_WRITERS");
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("run hub-server")
}

#[track_caller]
fn assert_refused(output: Output, why: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "should fail: {stderr}");
    assert!(stderr.contains(why), "expected {why:?} in: {stderr}");
}

#[test]
fn bad_tenant_labels_are_refused() {
    let long = "t".repeat(65);
    for bad in ["has.dot", "has space", "a*", long.as_str()] {
        assert_refused(hub_server(&["--tenant", bad], &[]), "--tenant");
    }
    assert_refused(hub_server(&[], &[("NATS_HUB_TENANT", "a.b")]), "--tenant");
}

#[test]
fn bad_manifest_writers_are_refused() {
    for bad in ["a.b", "hub-orchestrator", "hub-server", "manifest", "wave"] {
        assert_refused(
            hub_server(&["--manifest-writer", bad], &[]),
            "--manifest-writer",
        );
    }
    assert_refused(
        hub_server(&[], &[("NATS_HUB_MANIFEST_WRITERS", "ok-one, a.b")]),
        "--manifest-writer",
    );
}

#[test]
fn a_manifest_writer_needs_storage() {
    assert_refused(
        hub_server(&["--manifest-writer", "writer-1"], &[]),
        "needs --db-path",
    );
    assert_refused(
        hub_server(&[], &[("NATS_HUB_MANIFEST_WRITERS", "writer-1")]),
        "needs --db-path",
    );
}

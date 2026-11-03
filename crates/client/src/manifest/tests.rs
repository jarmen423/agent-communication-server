//! Validation matrix and wire forms of the manifest record.

use super::*;
use serde_json::json;

const UUID: &str = "0b6b7e0e-5b0c-4f4e-9d55-0a4b0ce1c111";

fn user_record() -> ManifestRecord {
    ManifestRecord {
        identity: "alice-agent".into(),
        owner_kind: OwnerKind::User,
        user: Some("alice".into()),
        user_id: None,
        device: None,
        device_id: None,
        kind: IdentityKind::Device,
        agent: Some("claude.ai".into()),
        role: Role::Worker,
        accept: None,
        rev: 1,
    }
}

fn tenant_record() -> ManifestRecord {
    ManifestRecord {
        identity: "bot".into(),
        owner_kind: OwnerKind::Tenant,
        user: None,
        ..user_record()
    }
}

#[track_caller]
fn refused(rec: ManifestRecord, field: &str) {
    let err = rec.validate().expect_err("record should be refused");
    assert!(err.starts_with(&format!("{field}: ")), "got: {err}");
}

#[test]
fn identity_rules() {
    user_record().validate().unwrap();
    let named = |identity: &str| ManifestRecord {
        identity: identity.into(),
        ..user_record()
    };
    named(&"a".repeat(MAX_IDENTITY_BYTES)).validate().unwrap();
    named("owner").validate().unwrap();
    for bad in [
        "",
        "a.b",
        "a*",
        "hub-orchestrator",
        "hub-server",
        "manifest",
        "wave",
    ] {
        refused(named(bad), "identity");
    }
    refused(named(&"a".repeat(MAX_IDENTITY_BYTES + 1)), "identity");
}

#[test]
fn owner_kind_rules() {
    tenant_record().validate().unwrap();
    refused(
        ManifestRecord {
            user: None,
            ..user_record()
        },
        "user",
    );
    refused(
        ManifestRecord {
            user: Some("alice".into()),
            ..tenant_record()
        },
        "user",
    );
    refused(
        ManifestRecord {
            user_id: Some(UUID.into()),
            ..tenant_record()
        },
        "user_id",
    );
}

#[test]
fn user_rules() {
    let with = |user: &str| ManifestRecord {
        user: Some(user.into()),
        ..user_record()
    };
    with(&"u".repeat(MAX_IDENTITY_BYTES)).validate().unwrap();
    refused(with("a.b"), "user");
    refused(with(&"u".repeat(MAX_IDENTITY_BYTES + 1)), "user");
}

#[test]
fn uuid_rules_take_only_the_hyphenated_form() {
    let simple = UUID.replace('-', "");
    let braced = format!("{{{UUID}}}");
    let urn = format!("urn:uuid:{UUID}");
    ManifestRecord {
        user_id: Some(UUID.into()),
        device_id: Some(UUID.into()),
        ..user_record()
    }
    .validate()
    .unwrap();
    for bad in ["nope", &simple, &braced, &urn] {
        refused(
            ManifestRecord {
                user_id: Some(bad.to_string()),
                ..user_record()
            },
            "user_id",
        );
        refused(
            ManifestRecord {
                device_id: Some(bad.to_string()),
                ..user_record()
            },
            "device_id",
        );
    }
}

#[test]
fn device_rules() {
    let with = |device: String| ManifestRecord {
        device: Some(device),
        ..user_record()
    };
    with("alice's laptop".into()).validate().unwrap();
    with("d".repeat(MAX_DEVICE_BYTES)).validate().unwrap();
    refused(with(String::new()), "device");
    refused(with("d".repeat(MAX_DEVICE_BYTES + 1)), "device");
    refused(with("bad\nname".into()), "device");
}

#[test]
fn agent_rules() {
    let with = |agent: String| ManifestRecord {
        agent: Some(agent),
        ..user_record()
    };
    with("claude.ai".into()).validate().unwrap();
    with("a".repeat(MAX_AGENT_BYTES)).validate().unwrap();
    refused(with(String::new()), "agent");
    refused(with("a".repeat(MAX_AGENT_BYTES + 1)), "agent");
    refused(with("has space".into()), "agent");
}

#[test]
fn accept_users_rules() {
    let with = |users: Vec<String>| ManifestRecord {
        accept: Some(AcceptPolicy::Users { users }),
        ..user_record()
    };
    with(vec!["alice".into(), "bob".into()]).validate().unwrap();
    let full = (0..MAX_ACCEPT_USERS).map(|i| format!("u{i}")).collect();
    with(full).validate().unwrap();
    refused(with(vec![]), "accept.users");
    let over = (0..=MAX_ACCEPT_USERS).map(|i| format!("u{i}")).collect();
    refused(with(over), "accept.users");
    refused(with(vec!["a.b".into()]), "accept.users");
    refused(with(vec!["u".repeat(65)]), "accept.users");
    refused(with(vec!["alice".into(), "alice".into()]), "accept.users");
}

#[test]
fn rev_rules() {
    ManifestRecord {
        rev: 0,
        ..user_record()
    }
    .validate()
    .unwrap();
    refused(
        ManifestRecord {
            rev: -1,
            ..user_record()
        },
        "rev",
    );
}

#[test]
fn effective_accept_defaults() {
    assert_eq!(user_record().effective_accept(), AcceptPolicy::Owner);
    assert_eq!(tenant_record().effective_accept(), AcceptPolicy::Tenant);

    let handle = ManifestRecord {
        identity: "alice".into(),
        agent: None,
        ..user_record()
    };
    assert_eq!(handle.effective_accept(), AcceptPolicy::Tenant);
    let owner = ManifestRecord {
        identity: "owner".into(),
        user: Some("root".into()),
        ..user_record()
    };
    assert_eq!(owner.effective_accept(), AcceptPolicy::Tenant);

    // An agent named after its user is still an agent.
    let agent = ManifestRecord {
        identity: "alice".into(),
        ..user_record()
    };
    assert_eq!(agent.effective_accept(), AcceptPolicy::Owner);

    // An explicit policy always wins.
    let explicit = ManifestRecord {
        accept: Some(AcceptPolicy::Tenant),
        ..user_record()
    };
    assert_eq!(explicit.effective_accept(), AcceptPolicy::Tenant);
}

#[test]
fn accept_policy_json_forms() {
    let cases = [
        (json!({"from": "owner"}), AcceptPolicy::Owner),
        (json!({"from": "tenant"}), AcceptPolicy::Tenant),
        (
            json!({"from": "users", "users": ["alice", "bob"]}),
            AcceptPolicy::Users {
                users: vec!["alice".into(), "bob".into()],
            },
        ),
    ];
    for (json, policy) in cases {
        assert_eq!(serde_json::to_value(&policy).unwrap(), json);
        let back: AcceptPolicy = serde_json::from_value(json).unwrap();
        assert_eq!(back, policy);
    }
}

#[test]
fn minimal_record_gets_defaults_and_ignores_unknown_fields() {
    let rec: ManifestRecord = serde_json::from_value(json!({
        "identity": "a", "owner_kind": "tenant", "rev": 3, "added_later": {"x": 1},
    }))
    .unwrap();
    assert_eq!(rec.kind, IdentityKind::Device);
    assert_eq!(rec.role, Role::Worker);
    assert_eq!(rec.accept, None);
    assert_eq!(rec.rev, 3);
}

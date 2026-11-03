//! Router stamping of `meta.origin`.
//!
//! [`stamp`] builds a fresh [`Origin`] for every routed envelope. Of what the
//! client sent, only `reported` (already sanitized when it was decoded) is
//! kept, plus `on_behalf_of.identity` when the bound sender is the wave
//! orchestrator. So a forged `identity`, `user` or `tenant` can't survive.

use super::IdentityManifest;
use crate::orchestrator::ORCHESTRATOR_IDENTITY;
use crate::protocol::{valid_identity, Envelope, OnBehalfOf, Origin};

/// How the router knows who sent an envelope.
pub(crate) enum Sender<'a> {
    /// The identity token of a bound subject, which NATS ACLs can pin to the
    /// connection's credentials.
    Bound(&'a str),
    /// A legacy subject. `meta.from` is self-asserted there, so looking it up
    /// would turn a spoofed name into trusted-looking provenance: only the
    /// hub's own tenant label and `reported` are stamped.
    Legacy,
}

/// Replace `env.meta.origin` (see the module docs).
pub(crate) fn stamp(
    env: &mut Envelope,
    sender: Sender<'_>,
    tenant: Option<&str>,
    manifest: &IdentityManifest,
) {
    let client = env.meta.origin.take().unwrap_or_default();
    let mut origin = Origin {
        tenant: tenant.map(str::to_string),
        reported: client.reported,
        ..Origin::default()
    };
    if let Sender::Bound(identity) = sender {
        origin.identity = Some(identity.to_string());
        if let Some(record) = manifest.get(identity) {
            origin.user = record.user.clone();
            origin.user_id = record.user_id.clone();
            origin.owner_kind = Some(record.owner_kind.as_str().to_string());
            origin.device = record.device.clone();
            origin.agent = record.agent.clone();
        }
        if identity == ORCHESTRATOR_IDENTITY {
            origin.on_behalf_of = client
                .on_behalf_of
                .filter(|actor| valid_identity(&actor.identity))
                .map(|actor| OnBehalfOf {
                    user: manifest
                        .get(&actor.identity)
                        .and_then(|record| record.user.clone()),
                    identity: actor.identity,
                });
        }
    }
    env.meta.origin = Some(origin);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{IdentityKind, ManifestRecord, OwnerKind, Role};
    use crate::protocol::MessageKind;
    use serde_json::json;

    const USER_ID: &str = "0b6b7e0e-5b0c-4f4e-9d55-0a4b0ce1c111";

    fn record(identity: &str, user: &str) -> ManifestRecord {
        ManifestRecord {
            identity: identity.into(),
            owner_kind: OwnerKind::User,
            user: Some(user.into()),
            user_id: Some(USER_ID.into()),
            device: Some("laptop".into()),
            device_id: None,
            kind: IdentityKind::Device,
            agent: Some("claude-code".into()),
            role: Role::Worker,
            accept: None,
            rev: 1,
        }
    }

    async fn manifest_with(records: &[(&str, &str)]) -> IdentityManifest {
        let manifest = IdentityManifest::default();
        for (identity, user) in records {
            manifest.put(None, record(identity, user)).await.unwrap();
        }
        manifest
    }

    /// An envelope decoded from the wire, carrying `origin` as sent.
    fn envelope(from: &str, origin: serde_json::Value) -> Envelope {
        let env = Envelope::new(from, "chat", MessageKind::Message, json!({}));
        let mut raw = serde_json::to_value(&env).unwrap();
        raw["meta"]["origin"] = origin;
        serde_json::from_value(raw).unwrap()
    }

    fn stamped(
        mut env: Envelope,
        sender: Sender<'_>,
        tenant: Option<&str>,
        manifest: &IdentityManifest,
    ) -> Origin {
        stamp(&mut env, sender, tenant, manifest);
        env.meta.origin.expect("stamp always sets origin")
    }

    #[tokio::test]
    async fn bound_with_a_live_record_gets_every_trusted_field() {
        let manifest = manifest_with(&[("alice-agent", "alice")]).await;
        let env = envelope("alice-agent", json!(null));
        let origin = stamped(env, Sender::Bound("alice-agent"), Some("acme"), &manifest);
        assert_eq!(
            origin,
            Origin {
                tenant: Some("acme".into()),
                identity: Some("alice-agent".into()),
                user: Some("alice".into()),
                user_id: Some(USER_ID.into()),
                owner_kind: Some("user".into()),
                device: Some("laptop".into()),
                agent: Some("claude-code".into()),
                ..Origin::default()
            }
        );
    }

    #[tokio::test]
    async fn bound_without_a_record_gets_tenant_and_identity_only() {
        let manifest = IdentityManifest::default();
        let env = envelope("nobody", json!(null));
        let origin = stamped(env, Sender::Bound("nobody"), Some("acme"), &manifest);
        assert_eq!(
            origin,
            Origin {
                tenant: Some("acme".into()),
                identity: Some("nobody".into()),
                ..Origin::default()
            }
        );
    }

    #[tokio::test]
    async fn bound_with_a_tombstone_gets_no_user() {
        let manifest = manifest_with(&[("gone", "alice")]).await;
        manifest.remove(None, "gone", 2).await.unwrap();
        let env = envelope("gone", json!(null));
        let origin = stamped(env, Sender::Bound("gone"), None, &manifest);
        assert_eq!(origin.identity.as_deref(), Some("gone"));
        assert_eq!(origin.user, None);
        assert_eq!(origin.owner_kind, None);
    }

    #[tokio::test]
    async fn forged_trusted_fields_are_discarded() {
        let manifest = manifest_with(&[("alice-agent", "alice")]).await;
        let forged = json!({
            "identity": "root",
            "user": "root",
            "user_id": "forged",
            "tenant": "other",
            "device": "forged",
            "owner_kind": "tenant",
            "agent": "forged",
        });
        for (sender, tenant) in [
            (Sender::Bound("alice-agent"), Some("acme")),
            (Sender::Bound("unknown"), None),
            (Sender::Legacy, Some("acme")),
            (Sender::Legacy, None),
        ] {
            let origin = stamped(
                envelope("alice-agent", forged.clone()),
                sender,
                tenant,
                &manifest,
            );
            let text = serde_json::to_string(&origin).unwrap();
            for value in ["root", "forged", "other"] {
                assert!(!text.contains(value), "{value} survived in {text}");
            }
            assert_ne!(origin.owner_kind.as_deref(), Some("tenant"));
            assert_eq!(origin.tenant.as_deref(), tenant);
        }
    }

    #[tokio::test]
    async fn legacy_never_looks_up_meta_from() {
        let manifest = manifest_with(&[("alice-agent", "alice")]).await;
        let origin = stamped(
            envelope("alice-agent", json!(null)),
            Sender::Legacy,
            Some("acme"),
            &manifest,
        );
        assert_eq!(
            origin,
            Origin {
                tenant: Some("acme".into()),
                ..Origin::default()
            }
        );
    }

    #[tokio::test]
    async fn reported_passes_through_for_both_senders() {
        let manifest = IdentityManifest::default();
        let sent = json!({"reported": {"model": "m1", "session": "s1", "client_version": "2"}});
        for sender in [Sender::Bound("a"), Sender::Legacy] {
            let origin = stamped(envelope("a", sent.clone()), sender, None, &manifest);
            assert_eq!(origin.reported.model.as_deref(), Some("m1"));
            assert_eq!(origin.reported.session.as_deref(), Some("s1"));
            assert_eq!(origin.reported.extra["client_version"], "2");
        }
    }

    #[tokio::test]
    async fn on_behalf_of_is_kept_only_from_the_orchestrator() {
        let manifest = manifest_with(&[("bob-agent", "bob")]).await;
        let sent = json!({"on_behalf_of": {"identity": "bob-agent", "user": "forged"}});

        let origin = stamped(
            envelope("x", sent.clone()),
            Sender::Bound(ORCHESTRATOR_IDENTITY),
            None,
            &manifest,
        );
        assert_eq!(
            origin.on_behalf_of,
            Some(OnBehalfOf {
                identity: "bob-agent".into(),
                user: Some("bob".into()),
            }),
            "user comes from the manifest, not the client"
        );

        for sender in [Sender::Bound("mallory"), Sender::Legacy] {
            let origin = stamped(envelope("x", sent.clone()), sender, None, &manifest);
            assert_eq!(origin.on_behalf_of, None);
        }
    }

    #[tokio::test]
    async fn on_behalf_of_needs_a_valid_live_identity_for_a_user() {
        let manifest = manifest_with(&[("gone", "alice")]).await;
        manifest.remove(None, "gone", 2).await.unwrap();

        let orchestrator = |sent: serde_json::Value| {
            stamped(
                envelope("x", sent),
                Sender::Bound(ORCHESTRATOR_IDENTITY),
                None,
                &manifest,
            )
        };
        let bad = orchestrator(json!({"on_behalf_of": {"identity": "not valid"}}));
        assert_eq!(bad.on_behalf_of, None);

        let removed = orchestrator(json!({"on_behalf_of": {"identity": "gone"}}));
        assert_eq!(
            removed.on_behalf_of,
            Some(OnBehalfOf {
                identity: "gone".into(),
                user: None,
            })
        );
    }
}

//! The `meta.origin.reported` values a [`HubClient`](super::HubClient) sends.

use std::borrow::Cow;
use std::sync::{Arc, RwLock};

use crate::origin::{Origin, Reported};
use crate::protocol::Envelope;

/// Shared by a client's clones. The lock is never held across an `.await`.
#[derive(Clone)]
pub(super) struct ReportedSlot(Arc<RwLock<Reported>>);

impl ReportedSlot {
    pub(super) fn from_env() -> Self {
        Self::new(Reported::from_env())
    }

    fn new(reported: Reported) -> Self {
        Self(Arc::new(RwLock::new(reported)))
    }

    pub(super) fn set(&self, reported: Reported) {
        // A panic can't leave a half-written value: the write is one move.
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = reported;
    }

    /// `env`, with this slot's values filled in where the caller left them out.
    pub(super) fn stamp<'a>(&self, env: &'a Envelope) -> Cow<'a, Envelope> {
        let reported = self.0.read().unwrap_or_else(|e| e.into_inner());
        match stamped_origin(env.meta.origin.as_ref(), &reported) {
            Some(origin) => {
                let mut env = env.clone();
                env.meta.origin = Some(origin);
                Cow::Owned(env)
            }
            None => Cow::Borrowed(env),
        }
    }
}

/// The origin to send, or `None` when the envelope's own stays as it is: there
/// is nothing to report, or the caller already reported something. The
/// caller's other fields are kept.
fn stamped_origin(current: Option<&Origin>, reported: &Reported) -> Option<Origin> {
    if reported.is_empty() || current.is_some_and(|origin| !origin.reported.is_empty()) {
        return None;
    }
    Some(Origin {
        reported: reported.clone(),
        ..current.cloned().unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::origin::OnBehalfOf;
    use crate::protocol::MessageKind;
    use serde_json::json;

    fn model(name: &str) -> Reported {
        Reported {
            model: Some(name.into()),
            ..Reported::default()
        }
    }

    fn envelope(origin: Option<Origin>) -> Envelope {
        let mut env = Envelope::new("a", "chat", MessageKind::Message, json!({}));
        env.meta.origin = origin;
        env
    }

    #[test]
    fn fills_an_envelope_without_origin() {
        let slot = ReportedSlot::new(model("m1"));
        let env = envelope(None);
        let sent = slot.stamp(&env);
        assert_eq!(sent.meta.origin.as_ref().unwrap().reported, model("m1"));
        assert_eq!(sent.meta.id, env.meta.id);
    }

    #[test]
    fn keeps_the_callers_fields_and_fills_empty_reported() {
        let on_behalf_of = Some(OnBehalfOf {
            identity: "bob".into(),
            user: None,
        });
        let env = envelope(Some(Origin {
            on_behalf_of: on_behalf_of.clone(),
            ..Origin::default()
        }));
        let sent = ReportedSlot::new(model("m1")).stamp(&env).into_owned();
        let origin = sent.meta.origin.unwrap();
        assert_eq!(origin.on_behalf_of, on_behalf_of);
        assert_eq!(origin.reported, model("m1"));
    }

    #[test]
    fn the_callers_reported_wins() {
        let env = envelope(Some(Origin {
            reported: model("caller"),
            ..Origin::default()
        }));
        let sent = ReportedSlot::new(model("m1")).stamp(&env);
        assert!(matches!(sent, Cow::Borrowed(_)));
        assert_eq!(sent.meta.origin.as_ref().unwrap().reported, model("caller"));
    }

    #[test]
    fn nothing_to_report_leaves_the_envelope_alone() {
        let env = envelope(None);
        let sent = ReportedSlot::new(Reported::default()).stamp(&env);
        assert!(matches!(sent, Cow::Borrowed(_)));
        assert!(sent.meta.origin.is_none());
    }

    #[test]
    fn set_applies_to_clones() {
        let slot = ReportedSlot::new(Reported::default());
        let clone = slot.clone();
        slot.set(model("m2"));
        let env = envelope(None);
        let sent = clone.stamp(&env);
        assert_eq!(sent.meta.origin.as_ref().unwrap().reported, model("m2"));
    }
}

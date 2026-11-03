//! Message provenance: the `meta.origin` block on an envelope.
//!
//! The router builds `origin` for every envelope it routes. Everything except
//! `reported` is **trusted**: the router sets it from the bound subject, the
//! identity manifest and the hub's tenant label, and discards whatever a
//! client sent. `reported` is **self-reported**: the client sets it, the
//! router sanitizes it and passes it through, and consumers must treat it as
//! unverified.
//!
//! Absent and `null` fields mean the same thing; the hub omits them to keep
//! envelopes small.
//!
//! Decoding never fails. A malformed `origin` must not make
//! [`Envelope::from_json_bytes`](crate::protocol::Envelope::from_json_bytes)
//! fail, or the router would drop the whole message over a provenance block
//! it is about to replace anyway. See [`Origin::from_value_lossy`].

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Most keys a `reported` block keeps (known keys first, then extras in key
/// order).
pub const REPORTED_MAX_KEYS: usize = 8;
/// Longest `reported` value, in bytes. Longer values are dropped, not
/// truncated: a truncated session id would point at the wrong session.
pub const REPORTED_VALUE_MAX_BYTES: usize = 256;
/// Longest extra `reported` key, in bytes.
pub const REPORTED_KEY_MAX_BYTES: usize = 32;

/// Provenance of an envelope (see the module docs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Origin {
    /// The hub's tenant label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// The bound sender. Absent on a legacy (unbound) send.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Handle of the user who owns the sending identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Id of the user who owns the sending identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// `"user"` or `"tenant"`. A string, not an enum, so a reader never fails
    /// on a value a newer hub adds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_kind: Option<String>,
    /// Device the sending identity runs on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Agent client, e.g. `claude-code`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The identity the wave orchestrator acts for. Router-set, and only when
    /// the bound sender is the orchestrator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<OnBehalfOf>,
    /// Self-reported by the client; unverified.
    #[serde(skip_serializing_if = "Reported::is_empty")]
    pub reported: Reported,
}

/// The identity an orchestrator's message acts for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OnBehalfOf {
    pub identity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// Self-reported, unverified client details.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Reported {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Other self-reported string keys (for example a client version).
    #[serde(flatten)]
    pub extra: BTreeMap<String, String>,
}

impl Origin {
    /// Decode an `origin` value, keeping only what is well formed. A
    /// non-object gives [`Origin::default`]. A field survives only when it is
    /// a string; `on_behalf_of` only when it is an object with a string
    /// `identity`; `reported` goes through [`Reported::sanitize`].
    pub fn from_value_lossy(value: &Value) -> Self {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
        Self {
            tenant: text("tenant"),
            identity: text("identity"),
            user: text("user"),
            user_id: text("user_id"),
            owner_kind: text("owner_kind"),
            device: text("device"),
            agent: text("agent"),
            on_behalf_of: value.get("on_behalf_of").and_then(OnBehalfOf::from_value),
            reported: value
                .get("reported")
                .map(Reported::sanitize)
                .unwrap_or_default(),
        }
    }
}

impl<'de> Deserialize<'de> for Origin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(Self::from_value_lossy(&value))
    }
}

impl OnBehalfOf {
    fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            identity: value.get("identity")?.as_str()?.to_string(),
            user: value
                .get("user")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

impl Reported {
    /// Keep what passes the caps and drop the rest, key by key, without
    /// error. A value must be a string of 1..=[`REPORTED_VALUE_MAX_BYTES`]
    /// bytes with no control characters; an extra key must match
    /// `^[a-z0-9_]{1,32}$`; at most [`REPORTED_MAX_KEYS`] keys survive.
    pub fn sanitize(value: &Value) -> Self {
        let Some(map) = value.as_object() else {
            return Self::default();
        };
        let known = |key: &str| map.get(key).and_then(clean_value);
        let mut out = Self {
            session: known("session"),
            model_provider: known("model_provider"),
            model: known("model"),
            extra: BTreeMap::new(),
        };
        let kept = [&out.session, &out.model_provider, &out.model]
            .iter()
            .filter(|v| v.is_some())
            .count();

        let mut extras: Vec<(&String, String)> = map
            .iter()
            .filter(|(key, _)| valid_extra_key(key))
            .filter_map(|(key, v)| clean_value(v).map(|v| (key, v)))
            .collect();
        extras.sort_by(|a, b| a.0.cmp(b.0));
        out.extra = extras
            .into_iter()
            .take(REPORTED_MAX_KEYS - kept)
            .map(|(key, v)| (key.clone(), v))
            .collect();
        out
    }

    /// True when nothing was reported.
    pub fn is_empty(&self) -> bool {
        self.session.is_none()
            && self.model_provider.is_none()
            && self.model.is_none()
            && self.extra.is_empty()
    }
}

fn clean_value(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let fits = !text.is_empty() && text.len() <= REPORTED_VALUE_MAX_BYTES;
    (fits && !text.chars().any(char::is_control)).then(|| text.to_string())
}

fn valid_extra_key(key: &str) -> bool {
    !matches!(key, "session" | "model_provider" | "model")
        && !key.is_empty()
        && key.len() <= REPORTED_KEY_MAX_BYTES
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Envelope, MessageKind};
    use serde_json::json;

    fn envelope_with_origin(origin: Option<Value>) -> Vec<u8> {
        let env = Envelope::new("a", "chat", MessageKind::Message, json!({"keep": 1}));
        let mut raw = serde_json::to_value(&env).unwrap();
        match origin {
            Some(origin) => raw["meta"]["origin"] = origin,
            None => {
                raw["meta"].as_object_mut().unwrap().remove("origin");
            }
        }
        serde_json::to_vec(&raw).unwrap()
    }

    #[test]
    fn round_trip_keeps_every_field() {
        let origin = Origin {
            tenant: Some("acme".into()),
            identity: Some("alice-agent".into()),
            user: Some("alice".into()),
            user_id: Some("0b6b7e0e-5b0c-4f4e-9d55-0a4b0ce1c111".into()),
            owner_kind: Some("user".into()),
            device: Some("laptop".into()),
            agent: Some("claude-code".into()),
            on_behalf_of: Some(OnBehalfOf {
                identity: "bob".into(),
                user: Some("bob".into()),
            }),
            reported: Reported {
                session: Some("s1".into()),
                model_provider: Some("anthropic".into()),
                model: Some("m1".into()),
                extra: BTreeMap::from([("client_version".to_string(), "1.2".to_string())]),
            },
        };
        let json = serde_json::to_value(&origin).unwrap();
        assert_eq!(json["reported"]["client_version"], "1.2");
        let back: Origin = serde_json::from_value(json).unwrap();
        assert_eq!(back, origin);
    }

    #[test]
    fn envelope_without_origin_decodes() {
        let env = Envelope::from_json_bytes(&envelope_with_origin(None)).unwrap();
        assert!(env.meta.origin.is_none());
    }

    #[test]
    fn null_origin_decodes_to_none() {
        let env = Envelope::from_json_bytes(&envelope_with_origin(Some(Value::Null))).unwrap();
        assert!(env.meta.origin.is_none());
    }

    #[test]
    fn malformed_origin_leaves_the_envelope_intact() {
        for bad in [json!(5), json!("x"), json!([]), json!({"user": 7})] {
            let env = Envelope::from_json_bytes(&envelope_with_origin(Some(bad.clone())))
                .unwrap_or_else(|e| panic!("origin {bad} failed the envelope: {e}"));
            assert_eq!(env.meta.origin, Some(Origin::default()), "origin {bad}");
            assert_eq!(env.payload, json!({"keep": 1}));
        }
    }

    #[test]
    fn on_behalf_of_needs_a_string_identity() {
        let parse = |v: Value| Origin::from_value_lossy(&json!({ "on_behalf_of": v }));
        assert_eq!(parse(json!({"user": "bob"})).on_behalf_of, None);
        assert_eq!(parse(json!({"identity": 5})).on_behalf_of, None);
        assert_eq!(parse(json!("bob")).on_behalf_of, None);
        let kept = parse(json!({"identity": "bob", "user": 5}))
            .on_behalf_of
            .unwrap();
        assert_eq!(kept.identity, "bob");
        assert_eq!(kept.user, None);
    }

    #[test]
    fn reported_keeps_known_keys_first_then_extras_in_order() {
        let r = Reported::sanitize(&json!({
            "z9": "z", "a1": "a", "a2": "a", "a3": "a", "a4": "a", "a5": "a", "a6": "a",
            "session": "s", "model": "m", "model_provider": "p",
        }));
        assert_eq!(r.session.as_deref(), Some("s"));
        assert_eq!(r.model.as_deref(), Some("m"));
        assert_eq!(r.model_provider.as_deref(), Some("p"));
        let keys: Vec<&str> = r.extra.keys().map(String::as_str).collect();
        assert_eq!(keys, ["a1", "a2", "a3", "a4", "a5"]);
    }

    #[test]
    fn reported_nine_extras_keep_eight() {
        let map: serde_json::Map<String, Value> =
            (0..9).map(|i| (format!("k{i}"), json!("v"))).collect();
        let r = Reported::sanitize(&Value::Object(map));
        assert_eq!(r.extra.len(), REPORTED_MAX_KEYS);
        assert!(!r.extra.contains_key("k8"));
    }

    #[test]
    fn reported_drops_bad_values_and_keeps_the_rest() {
        let long = "x".repeat(REPORTED_VALUE_MAX_BYTES + 1);
        let r = Reported::sanitize(&json!({
            "session": long,
            "model": "line\nbreak",
            "model_provider": 7,
            "empty": "",
            "good": "yes",
        }));
        assert_eq!(r.session, None, "over the byte cap");
        assert_eq!(r.model, None, "control character");
        assert_eq!(r.model_provider, None, "not a string");
        assert_eq!(
            r.extra,
            BTreeMap::from([("good".to_string(), "yes".to_string())])
        );
    }

    #[test]
    fn reported_drops_bad_extra_keys() {
        let long_key = "k".repeat(REPORTED_KEY_MAX_BYTES + 1);
        let ok_key = "k".repeat(REPORTED_KEY_MAX_BYTES);
        let r = Reported::sanitize(&json!({
            "Upper": "v", "dash-key": "v", "": "v", long_key: "v", ok_key.clone(): "v",
        }));
        assert_eq!(r.extra, BTreeMap::from([(ok_key, "v".to_string())]));
    }

    #[test]
    fn reported_unicode_cap_counts_bytes() {
        // "é" is two bytes: 128 of them fit exactly, one more does not.
        let at_cap = "é".repeat(REPORTED_VALUE_MAX_BYTES / 2);
        assert_eq!(at_cap.len(), REPORTED_VALUE_MAX_BYTES);
        let over = format!("{at_cap}x");
        let r = Reported::sanitize(&json!({"session": at_cap.clone(), "model": over}));
        assert_eq!(r.session, Some(at_cap));
        assert_eq!(r.model, None);
    }

    #[test]
    fn reported_non_object_is_empty() {
        for bad in [json!(5), json!("x"), json!([]), Value::Null] {
            assert!(Reported::sanitize(&bad).is_empty());
        }
    }

    #[test]
    fn empty_blocks_are_not_serialized() {
        assert_eq!(serde_json::to_value(Origin::default()).unwrap(), json!({}));
        let env = Envelope::new("a", "chat", MessageKind::Message, json!({}));
        let raw = serde_json::to_value(&env).unwrap();
        assert!(raw["meta"].get("origin").is_none());
    }
}

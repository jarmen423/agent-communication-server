//! One-line label for message provenance (`meta.origin`).
//!
//! Form: `alice · claude-code · ~claude-opus-5-5`. The human and the agent
//! client are trusted (router-stamped). A `~` prefixes a self-reported,
//! unverified value: the model always, and the session in verbose labels.
//! Missing parts are omitted; an origin with nothing to show gives an empty
//! string, so old hubs add nothing to the output.
//!
//! Values are untrusted even when they came from a stored row (an old hub
//! may not have sanitized `reported`), so control characters are stripped
//! again here and each part is capped before printing.

use crate::protocol::Origin;

/// Longest part shown in a label, in characters.
const MAX_PART_CHARS: usize = 64;
/// Characters of a session id shown in a verbose label.
const SESSION_CHARS: usize = 8;

/// Build the provenance label for an origin. Verbose labels also show the
/// self-reported session (shortened) and who a wave task acts for.
pub fn origin_label(origin: &Origin, verbose: bool) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(user) = clean(origin.user.as_deref()) {
        parts.push(user);
    } else if origin.owner_kind.as_deref() == Some("tenant") {
        if let Some(tenant) = clean(origin.tenant.as_deref()) {
            parts.push(format!("tenant:{tenant}"));
        }
    }
    if let Some(agent) = clean(origin.agent.as_deref()) {
        parts.push(agent);
    }
    if let Some(model) = clean(origin.reported.model.as_deref()) {
        parts.push(format!("~{model}"));
    }

    if verbose {
        if let Some(session) = clean(origin.reported.session.as_deref()) {
            let short: String = session.chars().take(SESSION_CHARS).collect();
            parts.push(format!("~session:{short}"));
        }
        if let Some(on_behalf_of) = &origin.on_behalf_of {
            let behalf =
                clean(on_behalf_of.user.as_deref()).or_else(|| clean(Some(&on_behalf_of.identity)));
            if let Some(behalf) = behalf {
                parts.push(format!("on behalf of {behalf}"));
            }
        }
    }

    parts.join(" · ")
}

/// Strip control characters and cap a value for display. An empty result
/// counts as absent.
fn clean(value: Option<&str>) -> Option<String> {
    let cleaned: String = value?
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_PART_CHARS)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{OnBehalfOf, Reported};

    fn full_origin() -> Origin {
        Origin {
            user: Some("alice".into()),
            agent: Some("claude-code".into()),
            reported: Reported {
                model: Some("claude-opus-5-5".into()),
                session: Some("9f2c1a2b3c4d".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn full_label_shows_user_agent_and_unverified_model() {
        assert_eq!(
            origin_label(&full_origin(), false),
            "alice · claude-code · ~claude-opus-5-5"
        );
    }

    #[test]
    fn verbose_adds_shortened_session() {
        assert_eq!(
            origin_label(&full_origin(), true),
            "alice · claude-code · ~claude-opus-5-5 · ~session:9f2c1a2b"
        );
    }

    #[test]
    fn empty_origin_shows_nothing() {
        assert_eq!(origin_label(&Origin::default(), false), "");
        assert_eq!(origin_label(&Origin::default(), true), "");
    }

    #[test]
    fn tenant_owner_without_user_shows_tenant() {
        let origin = Origin {
            tenant: Some("acme".into()),
            owner_kind: Some("tenant".into()),
            agent: Some("claude-code".into()),
            ..Default::default()
        };
        assert_eq!(origin_label(&origin, false), "tenant:acme · claude-code");
    }

    #[test]
    fn verbose_on_behalf_of_prefers_user() {
        let mut origin = full_origin();
        origin.on_behalf_of = Some(OnBehalfOf {
            identity: "bob-orchestrator".into(),
            user: Some("bob".into()),
        });
        assert!(origin_label(&origin, true).ends_with("on behalf of bob"));
        assert!(!origin_label(&origin, false).contains("on behalf of"));
        origin.on_behalf_of.as_mut().unwrap().user = None;
        assert!(origin_label(&origin, true).ends_with("on behalf of bob-orchestrator"));
    }

    #[test]
    fn control_characters_are_stripped_and_parts_capped() {
        let origin = Origin {
            user: Some("al\x1b[31mice\n".into()),
            reported: Reported {
                model: Some(format!("m\u{7}{}", "x".repeat(100))),
                ..Default::default()
            },
            ..Default::default()
        };
        let label = origin_label(&origin, false);
        assert!(!label.chars().any(char::is_control), "{label:?}");
        let model_part = label.rsplit(" · ").next().unwrap();
        assert_eq!(model_part.chars().count(), 1 + MAX_PART_CHARS, "{label}");
    }
}

//! One-line label for message provenance (`meta.origin`).
//!
//! Form: `alice · claude-code · ~claude-opus-5-5`. The human and the agent
//! client are trusted (router-stamped). A `~` prefixes a self-reported,
//! unverified value: the model always, and the session in verbose labels.
//! Missing parts are omitted; an origin with nothing to show gives an empty
//! string, so old hubs add nothing to the output.
//!
//! Values are untrusted even when they came from a stored row (an old hub
//! may not have sanitized `reported`), so control and format characters
//! are stripped again here and each part is capped before printing. The
//! separator (`·`) and the self-reported marker (`~`) are also replaced
//! inside values, so a value can never forge extra label parts or a fake
//! marker.

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

/// True for Unicode format characters (general category `Cf`): bidi
/// overrides/isolates, zero-width characters, the BOM, and friends.
/// Explicit ranges (Unicode 16) so no category-table crate is needed.
fn is_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}'
        | '\u{0600}'..='\u{0605}'
        | '\u{061C}'
        | '\u{06DD}'
        | '\u{070F}'
        | '\u{0890}'..='\u{0891}'
        | '\u{08E2}'
        | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{110BD}'
        | '\u{110CD}'
        | '\u{13430}'..='\u{1343F}'
        | '\u{1BCA0}'..='\u{1BCA3}'
        | '\u{1D173}'..='\u{1D17A}'
        | '\u{E0001}'
        | '\u{E0020}'..='\u{E007F}')
}

/// Clean a value for display: drop control (`Cc`) and format (`Cf`)
/// characters plus the line/paragraph separators U+2028/U+2029, replace
/// the label separator `·` and the self-reported marker `~` with `-`,
/// and cap the result. An empty result counts as absent. The JS and
/// Python label helpers implement exactly this.
fn clean(value: Option<&str>) -> Option<String> {
    let cleaned: String = value?
        .chars()
        .filter_map(|c| {
            if c.is_control() || is_format_char(c) || matches!(c, '\u{2028}' | '\u{2029}') {
                None
            } else if matches!(c, '·' | '~') {
                Some('-')
            } else {
                Some(c)
            }
        })
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
    fn values_cannot_forge_label_parts_or_the_unverified_marker() {
        // A self-reported model carrying the separator must stay one
        // self-reported part, never trusted-looking extra parts.
        let origin = Origin {
            reported: Reported {
                model: Some("x · alice · claude-code".into()),
                session: Some("ab·~cdefghij".into()),
                ..Default::default()
            },
            on_behalf_of: Some(OnBehalfOf {
                identity: "id·~x".into(),
                user: Some("bob · ~mallory".into()),
            }),
            ..Default::default()
        };
        let label = origin_label(&origin, true);
        assert_eq!(
            label,
            "~x - alice - claude-code · ~session:ab--cdef · on behalf of bob - -mallory"
        );
        // Only the real separators and real `~` prefixes remain.
        assert_eq!(label.matches(" · ").count(), 2, "{label}");
        assert_eq!(label.matches('~').count(), 2, "{label}");

        // A trusted-looking user starting with `~` must not look
        // self-reported.
        let origin = Origin {
            user: Some("~alice".into()),
            ..Default::default()
        };
        assert_eq!(origin_label(&origin, false), "-alice");
    }

    #[test]
    fn bidi_and_invisible_characters_are_stripped() {
        // Bidi override/isolate, zero-width, BOM, word joiner and the
        // line/paragraph separators must not survive into a printed row.
        let origin = Origin {
            user: Some(
                "al\u{202E}ice\u{200B}\u{2066}\u{FEFF}\u{2060}\u{2028}\u{2029}\u{200F}".into(),
            ),
            reported: Reported {
                model: Some("mo\u{202A}del\u{00AD}\u{061C}\u{E0061}".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let label = origin_label(&origin, false);
        assert_eq!(label, "alice · ~model");
        assert!(
            !label.chars().any(|c| is_format_char(c)
                || c.is_control()
                || matches!(c, '\u{2028}' | '\u{2029}')),
            "{label:?}"
        );
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

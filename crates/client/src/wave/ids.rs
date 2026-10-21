//! Wave and task identifiers.
//!
//! Both ids end up inside NATS subjects (`channel.wave.<wave_id>`,
//! `channel.wave.<wave_id>.task.<task_id>`), and together they form the
//! task's session id `<wave_id>--<task_id>` (contract C4), which workers treat
//! as one opaque NATS token. So each id must be a single token without the
//! `--` separator, or two different (wave, task) pairs could share a session.

use anyhow::{bail, Result};

/// Separator between the wave id and the task id in a wave task session id.
pub const SESSION_ID_SEPARATOR: &str = "--";

/// Longest wave or task id accepted.
pub const MAX_ID_LEN: usize = 128;

/// The session id a wave task runs under: `<wave_id>--<task_id>`. Unique per
/// (wave, task), so worker session state never carries over between waves,
/// and stable across re-dispatches of the same task.
pub fn wave_task_session_id(wave_id: &str, task_id: &str) -> String {
    format!("{wave_id}{SESSION_ID_SEPARATOR}{task_id}")
}

/// Check that `id` (a wave id or task id, named by `what`) is a single NATS
/// token: non-empty, at most [`MAX_ID_LEN`] bytes, printable ASCII without
/// whitespace, `.`, `*`, `>`, and without the `--` session separator.
pub fn validate_id(what: &str, id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("{what} must not be empty");
    }
    if id.len() > MAX_ID_LEN {
        bail!("{what} '{id}' is longer than {MAX_ID_LEN} bytes");
    }
    if let Some(c) = id
        .chars()
        .find(|c| !c.is_ascii_graphic() || matches!(c, '.' | '*' | '>'))
    {
        bail!("{what} '{id}' contains {c:?}; ids must be one NATS token (no '.', '*', '>', whitespace)");
    }
    if id.contains(SESSION_ID_SEPARATOR) {
        bail!("{what} '{id}' must not contain '{SESSION_ID_SEPARATOR}' (it separates wave and task in session ids)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_joins_wave_and_task() {
        assert_eq!(wave_task_session_id("w1", "t1"), "w1--t1");
    }

    #[test]
    fn accepts_ordinary_ids() {
        for id in ["w1", "a1b2c3d4", "w-happy", "task_7", "T-1-2"] {
            validate_id("id", id).unwrap();
        }
    }

    #[test]
    fn rejects_ids_that_break_subjects_or_session_ids() {
        let long = "x".repeat(MAX_ID_LEN + 1);
        for id in [
            "",
            "a.b",
            "a*",
            "a>",
            "a b",
            "tab\t",
            "a--b",
            "--",
            "é",
            long.as_str(),
        ] {
            assert!(validate_id("id", id).is_err(), "{id:?} should be rejected");
        }
    }
}

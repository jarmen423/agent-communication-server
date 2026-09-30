//! Datetime helpers for the SurrealDB backend.
//!
//! Through the SurrealDB SDK, chrono's `DateTime<Utc>` serializes as a
//! *string*, which a `TYPE datetime` field rejects (and which compares as a
//! string, not a time). Every datetime written or bound as a query parameter
//! must go through [`db_time`] so it lands as a native SurrealDB datetime.
//!
//! Rows written before schema v2 hold RFC 3339 strings. `migrate()` converts
//! them, and [`StoredTime`] keeps read paths tolerant of any row it could not
//! convert: a bad value is logged and reported, never silently replaced with
//! "now".

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tracing::warn;

/// Native SurrealDB datetime (serializes as a SurrealDB `datetime`).
pub(crate) type DbTime = surrealdb::sql::Datetime;

/// Convert a chrono timestamp into a native SurrealDB datetime.
pub(crate) fn db_time(t: DateTime<Utc>) -> DbTime {
    t.into()
}

/// Current time as a native SurrealDB datetime.
pub(crate) fn db_now() -> DbTime {
    Utc::now().into()
}

/// A datetime column as read back from the DB: native, or a legacy string.
///
/// A valid RFC 3339 string deserializes as `Native` too, so `Text` only
/// carries empty strings (legacy "no value") and corrupt values.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum StoredTime {
    Native(DbTime),
    Text(String),
}

impl StoredTime {
    /// Resolve an optional column. Empty legacy strings mean "no value";
    /// anything else unparseable is logged as corruption and treated as
    /// missing.
    pub(crate) fn optional(
        value: Option<StoredTime>,
        record: &str,
        field: &str,
    ) -> Option<DateTime<Utc>> {
        match value? {
            StoredTime::Native(d) => Some(d.into()),
            StoredTime::Text(s) if s.trim().is_empty() => None,
            StoredTime::Text(s) => match parse_legacy(&s) {
                Some(t) => Some(t),
                None => {
                    warn!(%record, %field, value = %s, "corrupt datetime in storage; treating as unset");
                    None
                }
            },
        }
    }

    /// Resolve a required column. A missing or corrupt value is logged and
    /// returned as an error naming the record, so callers can skip the row
    /// (lists) or surface it (single gets) instead of inventing a time.
    pub(crate) fn required(self, record: &str, field: &str) -> Result<DateTime<Utc>> {
        match self {
            StoredTime::Native(d) => Ok(d.into()),
            StoredTime::Text(s) => parse_legacy(&s).ok_or_else(|| {
                warn!(%record, %field, value = %s, "corrupt datetime in storage");
                anyhow!("{record}: corrupt `{field}` value {s:?} (expected an RFC 3339 datetime)")
            }),
        }
    }
}

fn parse_legacy(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Metadata columns are `TYPE object`; JSON `null` (the serde default for
/// `serde_json::Value`) is stored as `{}`.
pub(crate) fn metadata_object(v: serde_json::Value) -> serde_json::Value {
    if v.is_object() {
        v
    } else if v.is_null() {
        serde_json::json!({})
    } else {
        serde_json::json!({ "value": v })
    }
}

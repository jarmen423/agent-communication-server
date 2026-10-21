// Query-shape assertions: every statement the analytics backend
// emits must aggregate inside SurrealQL and must never project
// envelope columns (the row-loading shape this file replaced).
// `tests/analytics.rs` exercises the same statements end to end.
use super::*;

// Query-shape assertions: every statement the analytics backend
// emits must aggregate inside SurrealQL and must never project
// envelope columns (the row-loading shape this file replaced).
// `tests/analytics.rs` exercises the same statements end to end.
use super::*;

#[test]
fn aggregates_are_pushed_down() {
    let range = TimeRange::bounded(Utc::now() - chrono::Duration::hours(1), Utc::now());

    for stmt in [
        SurrealAnalytics::bucket_sql(&range, ""),
        SurrealAnalytics::bucket_sql(&range, " AND kind = 'event'"),
        SurrealAnalytics::latency_sql(&range, Some("agents.tasks")),
        SurrealAnalytics::latency_sql(&range, None),
        SurrealAnalytics::activity_sent_sql(&range),
        SurrealAnalytics::activity_recv_sql(&range),
        SurrealAnalytics::activity_events_sql(&range),
        SurrealAnalytics::hotspots_sql(&range),
        SurrealAnalytics::PENDING_SQL.to_string(),
    ] {
        assert!(
            stmt.contains("count(") || stmt.contains("math::") || stmt.contains("GROUP"),
            "must aggregate in the DB: {stmt}"
        );
        // Selecting payload/from_identity/... per envelope is the
        // row-loading shape this file replaced.
        for row_col in ["payload,", "from_identity,", "reply_to,"] {
            assert!(
                !stmt.contains(row_col),
                "looks like a row load ({row_col}): {stmt}"
            );
        }
    }
}

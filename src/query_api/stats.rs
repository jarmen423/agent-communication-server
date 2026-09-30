//! Query API handlers for analytics (`stats.*`). Needs the
//! `storage-surreal` feature (for `SurrealAnalytics`).

use std::sync::Arc;

use serde_json::{json, Value};

use super::ApiResponse;
use crate::analytics::{Analytics, Interval, SurrealAnalytics, TimeRange};
use crate::storage::Storage;

fn parse_interval(p: &Value) -> Interval {
    match p.get("interval").and_then(|v| v.as_str()).unwrap_or("hour") {
        "minute" => Interval::Minute,
        "day" => Interval::Day,
        _ => Interval::Hour,
    }
}

pub(super) async fn stats_message_rate(s: &Arc<dyn Storage>, p: &Value) -> ApiResponse {
    let secs = p.get("secs").and_then(|v| v.as_i64()).unwrap_or(3600);
    let interval = parse_interval(p);
    let analytics = SurrealAnalytics::new(s.clone());
    match analytics
        .message_rate(&TimeRange::last(secs), interval)
        .await
    {
        Ok(data) => ApiResponse::ok(json!({"data": data})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn stats_latency(s: &Arc<dyn Storage>, p: &Value) -> ApiResponse {
    let secs = p.get("secs").and_then(|v| v.as_i64()).unwrap_or(3600);
    let channel = p.get("channel").and_then(|v| v.as_str());
    let analytics = SurrealAnalytics::new(s.clone());
    match analytics
        .latency_stats(channel, &TimeRange::last(secs))
        .await
    {
        Ok(stats) => ApiResponse::ok(json!({"stats": stats})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn stats_agent_activity(s: &Arc<dyn Storage>, p: &Value) -> ApiResponse {
    let identity = match p.get("identity").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing identity"),
    };
    let secs = p.get("secs").and_then(|v| v.as_i64()).unwrap_or(86400);
    let analytics = SurrealAnalytics::new(s.clone());
    match analytics
        .agent_activity(identity, &TimeRange::last(secs))
        .await
    {
        Ok(activity) => ApiResponse::ok(json!({"activity": activity})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn stats_channel_hotspots(s: &Arc<dyn Storage>, p: &Value) -> ApiResponse {
    let secs = p.get("secs").and_then(|v| v.as_i64()).unwrap_or(3600);
    let limit = p.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let analytics = SurrealAnalytics::new(s.clone());
    match analytics
        .channel_hotspots(&TimeRange::last(secs), limit)
        .await
    {
        Ok(hotspots) => ApiResponse::ok(json!({"hotspots": hotspots})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn stats_error_rate(s: &Arc<dyn Storage>, p: &Value) -> ApiResponse {
    let secs = p.get("secs").and_then(|v| v.as_i64()).unwrap_or(3600);
    let interval = parse_interval(p);
    let analytics = SurrealAnalytics::new(s.clone());
    match analytics.error_rate(&TimeRange::last(secs), interval).await {
        Ok(data) => ApiResponse::ok(json!({"data": data})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

//! Wave record handlers (`wave.create`, `wave.get`, task CRUD, status
//! updates). Orchestration ops live in [`super::wave_orch`].
//!
//! Terminal states are final: a `completed`/`failed`/`cancelled` wave and a
//! `done`/`failed`/`cancelled` task never change again, and while a wave is
//! `running` its task statuses change only through the orchestrator's state
//! machine.

use chrono::Utc;
use serde_json::{json, Value};

use super::wave_orch::ask_orchestrator;
use crate::orchestrator::OrchCommand;
use crate::query_api::ApiResponse;
use crate::storage::{Storage, WaveRecord, WaveTaskRecord};
use crate::wave::{validate_id, validate_tasks, WaveTaskInput};

fn is_wave_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

fn is_task_terminal(status: &str) -> bool {
    matches!(status, "done" | "failed" | "cancelled")
}

/// A wave may be (re)created only while it doesn't exist or is still
/// pending: overwriting a running or finished wave would reset its tasks
/// behind the orchestrator's back.
async fn check_recreate(s: &dyn Storage, wave_id: &str) -> Result<(), ApiResponse> {
    validate_id("wave_id", wave_id).map_err(|e| ApiResponse::err(format!("{e:#}")))?;
    match s.get_wave(wave_id).await {
        Ok(Some(w)) if w.status == "running" || is_wave_terminal(&w.status) => {
            Err(ApiResponse::err(format!(
                "wave '{wave_id}' already exists and is '{}'",
                w.status
            )))
        }
        Ok(_) => Ok(()),
        Err(e) => Err(ApiResponse::err(e.to_string())),
    }
}

/// `wave.create`.
///
/// Atomic form (preferred): `{"wave": WaveRecord, "tasks": [WaveTaskInput]}`
/// — validates every task (write-scope overlap, unknown deps, dependency
/// cycles) before persisting anything, then writes the wave + all tasks.
///
/// Legacy form: a bare `WaveRecord` creates the wave only.
pub(in crate::query_api) async fn wave_create(s: &dyn Storage, p: &Value) -> ApiResponse {
    if let Some(wave_v) = p.get("wave") {
        let wave: WaveRecord = match serde_json::from_value(wave_v.clone()) {
            Ok(w) => w,
            Err(e) => return ApiResponse::err(format!("bad wave record: {e}")),
        };
        let inputs: Vec<WaveTaskInput> = match p.get("tasks") {
            None | Some(Value::Null) => Vec::new(),
            Some(v) => match serde_json::from_value(v.clone()) {
                Ok(t) => t,
                Err(e) => return ApiResponse::err(format!("bad tasks list: {e}")),
            },
        };
        if let Err(resp) = check_recreate(s, &wave.wave_id).await {
            return resp;
        }
        if !inputs.is_empty() {
            if let Err(e) = validate_tasks(&inputs) {
                return ApiResponse::err(format!("{e:#}"));
            }
        }
        let wave_id = wave.wave_id.clone();
        if let Err(e) = s.create_wave(wave).await {
            return ApiResponse::err(e.to_string());
        }
        let now = Utc::now();
        let mut created = Vec::with_capacity(inputs.len());
        for input in inputs {
            let task = WaveTaskRecord {
                wave_id: wave_id.clone(),
                task_id: input.task_id.clone(),
                worker: input.worker.clone(),
                goal: input.goal.clone(),
                status: "pending".to_string(),
                write_scope: input.write_scope,
                dependencies: input.dependencies,
                handoff_path: input.handoff_path,
                verify_cmd: input.verify_cmd,
                created_at: now,
                started_at: None,
                completed_at: None,
                result: None,
                verify_result: None,
            };
            let task_id = task.task_id.clone();
            if let Err(e) = s.create_wave_task(task).await {
                return ApiResponse::err(format!(
                    "wave created but task '{task_id}' failed to store: {e}"
                ));
            }
            created.push(task_id);
        }
        return ApiResponse::ok(json!({"wave_id": wave_id, "tasks": created}));
    }

    let wave: WaveRecord = match serde_json::from_value(p.clone()) {
        Ok(w) => w,
        Err(e) => return ApiResponse::err(format!("bad wave record: {e}")),
    };
    if let Err(resp) = check_recreate(s, &wave.wave_id).await {
        return resp;
    }
    match s.create_wave(wave).await {
        Ok(()) => ApiResponse::ok(json!({"created": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(in crate::query_api) async fn wave_create_task(s: &dyn Storage, p: &Value) -> ApiResponse {
    let task: WaveTaskRecord = match serde_json::from_value(p.clone()) {
        Ok(t) => t,
        Err(e) => return ApiResponse::err(format!("bad task record: {e}")),
    };
    if let Err(e) = validate_id("task_id", &task.task_id) {
        return ApiResponse::err(format!("{e:#}"));
    }
    match s.get_wave(&task.wave_id).await {
        Ok(Some(w)) if w.status == "running" || is_wave_terminal(&w.status) => {
            return ApiResponse::err(format!(
                "wave '{}' is '{}' — tasks can only be added to a pending wave",
                task.wave_id, w.status
            ))
        }
        Ok(_) => {}
        Err(e) => return ApiResponse::err(e.to_string()),
    }
    match s.get_wave_task(&task.wave_id, &task.task_id).await {
        Ok(Some(t)) if is_task_terminal(&t.status) || t.status == "running" => {
            return ApiResponse::err(format!(
                "task '{}' already exists and is '{}'",
                task.task_id, t.status
            ))
        }
        Ok(_) => {}
        Err(e) => return ApiResponse::err(e.to_string()),
    }
    match s.create_wave_task(task).await {
        Ok(()) => ApiResponse::ok(json!({"created": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(in crate::query_api) async fn wave_update_status(s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    let status = match p.get("status").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ApiResponse::err("missing status"),
    };
    let current = match s.get_wave(wave_id).await {
        Ok(Some(w)) => w.status,
        Ok(None) => return ApiResponse::err(format!("wave '{wave_id}' not found")),
        Err(e) => return ApiResponse::err(e.to_string()),
    };
    if current == status {
        return ApiResponse::ok(json!({"updated": false, "status": current}));
    }
    if is_wave_terminal(&current) {
        return ApiResponse::err(format!(
            "wave '{wave_id}' is already {current} (terminal states are final)"
        ));
    }
    if current == "running" {
        return ApiResponse::err(format!(
            "wave '{wave_id}' is running — the orchestrator owns its status; use wave.cancel"
        ));
    }
    if status == "running" {
        return ApiResponse::err("use wave.spawn to start a wave");
    }
    match s.update_wave_status(wave_id, status).await {
        Ok(()) => ApiResponse::ok(json!({"updated": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(in crate::query_api) async fn wave_get(s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    match s.get_wave(wave_id).await {
        Ok(wave) => ApiResponse::ok(json!({"wave": wave})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(in crate::query_api) async fn wave_list(s: &dyn Storage, p: &Value) -> ApiResponse {
    let status = p.get("status").and_then(|v| v.as_str());
    match s.list_waves(status).await {
        Ok(waves) => ApiResponse::ok(json!({"waves": waves})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(in crate::query_api) async fn wave_list_tasks(s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    match s.list_wave_tasks(wave_id).await {
        Ok(tasks) => ApiResponse::ok(json!({"tasks": tasks})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(in crate::query_api) async fn wave_update_task_status(
    s: &dyn Storage,
    p: &Value,
) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return ApiResponse::err("missing wave_id"),
    };
    let task_id = match p.get("task_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return ApiResponse::err("missing task_id"),
    };
    let status = match p.get("status").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return ApiResponse::err("missing status"),
    };
    let result = p
        .get("result")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let task = match s.get_wave_task(&wave_id, &task_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return ApiResponse::err(format!("task '{task_id}' not found in wave '{wave_id}'"))
        }
        Err(e) => return ApiResponse::err(e.to_string()),
    };
    if task.status == status {
        return ApiResponse::ok(json!({"updated": false, "status": status}));
    }
    if is_task_terminal(&task.status) {
        return ApiResponse::err(format!(
            "task '{task_id}' is already {} (terminal states are final)",
            task.status
        ));
    }
    let wave_status = match s.get_wave(&wave_id).await {
        Ok(Some(w)) => w.status,
        Ok(None) => return ApiResponse::err(format!("wave '{wave_id}' not found")),
        Err(e) => return ApiResponse::err(e.to_string()),
    };
    if is_wave_terminal(&wave_status) {
        return ApiResponse::err(format!("wave '{wave_id}' is already {wave_status}"));
    }
    if wave_status == "running" {
        // A running wave's tasks move only through the orchestrator's state
        // machine (dependents dispatch, fail-fast, cancel DMs). Without an
        // orchestrator in this process (embedded use), fall through to storage.
        let forwarded = ask_orchestrator(|reply| OrchCommand::UpdateTask {
            wave_id: wave_id.clone(),
            task_id: task_id.clone(),
            status: status.clone(),
            result: result.clone(),
            reply,
        })
        .await;
        if let Some(resp) = forwarded {
            return resp;
        }
    }
    match s
        .update_wave_task_status(&wave_id, &task_id, &status, result.as_deref())
        .await
    {
        Ok(()) => ApiResponse::ok(json!({"updated": true, "status": status})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(in crate::query_api) async fn wave_get_task(s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    let task_id = match p.get("task_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing task_id"),
    };
    match s.get_wave_task(wave_id, task_id).await {
        Ok(task) => ApiResponse::ok(json!({"task": task})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

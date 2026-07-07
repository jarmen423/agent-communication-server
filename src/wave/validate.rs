//! Wave task validation — disjoint write scopes and dependency checks.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Task definition from a `tasks.json` file passed to `hub-wave create`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WaveTaskInput {
    pub task_id: String,
    pub worker: String,
    pub goal: String,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub handoff_path: Option<String>,
    #[serde(default)]
    pub verify_cmd: Option<String>,
}

/// Return true when two write-scope paths overlap (same path or parent/child).
fn scopes_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

/// Validate task definitions before persisting a wave.
pub fn validate_tasks(tasks: &[WaveTaskInput]) -> Result<()> {
    if tasks.is_empty() {
        bail!("tasks list must not be empty");
    }

    let ids: std::collections::HashSet<&str> = tasks.iter().map(|t| t.task_id.as_str()).collect();
    if ids.len() != tasks.len() {
        bail!("duplicate task_id values in tasks list");
    }

    for task in tasks {
        for dep in &task.dependencies {
            if !ids.contains(dep.as_str()) {
                bail!(
                    "task '{}' depends on unknown task_id '{}'",
                    task.task_id,
                    dep
                );
            }
            if dep == &task.task_id {
                bail!("task '{}' cannot depend on itself", task.task_id);
            }
        }
    }

    for i in 0..tasks.len() {
        for j in (i + 1)..tasks.len() {
            for scope_a in &tasks[i].write_scope {
                for scope_b in &tasks[j].write_scope {
                    if scopes_overlap(scope_a, scope_b) {
                        bail!(
                            "overlapping write scopes: task '{}' ({scope_a}) vs task '{}' ({scope_b})",
                            tasks[i].task_id,
                            tasks[j].task_id
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, scopes: &[&str], deps: &[&str]) -> WaveTaskInput {
        WaveTaskInput {
            task_id: id.to_string(),
            worker: "worker-1".to_string(),
            goal: "do work".to_string(),
            write_scope: scopes.iter().map(|s| s.to_string()).collect(),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
            handoff_path: None,
            verify_cmd: None,
        }
    }

    #[test]
    fn accepts_disjoint_scopes() {
        let tasks = vec![task("a", &["src/foo"], &[]), task("b", &["src/bar"], &[])];
        assert!(validate_tasks(&tasks).is_ok());
    }

    #[test]
    fn rejects_overlapping_scopes() {
        let tasks = vec![task("a", &["src"], &[]), task("b", &["src/lib"], &[])];
        assert!(validate_tasks(&tasks).is_err());
    }

    #[test]
    fn rejects_unknown_dependency() {
        let tasks = vec![task("a", &["src/a"], &["missing"])];
        assert!(validate_tasks(&tasks).is_err());
    }
}

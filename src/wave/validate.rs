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

    check_dependency_cycles(tasks)?;

    Ok(())
}

/// Reject dependency cycles. A cycle means tasks wait on each other forever —
/// the old client-side orchestrator hung silently in that case.
fn check_dependency_cycles(tasks: &[WaveTaskInput]) -> Result<()> {
    use std::collections::{HashMap, HashSet};

    // Kahn's algorithm: repeatedly resolve tasks with no unsatisfied deps.
    let mut unresolved: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for task in tasks {
        unresolved.insert(
            task.task_id.as_str(),
            task.dependencies.iter().map(|d| d.as_str()).collect(),
        );
        for dep in &task.dependencies {
            dependents
                .entry(dep.as_str())
                .or_default()
                .push(task.task_id.as_str());
        }
    }

    let mut queue: Vec<&str> = unresolved
        .iter()
        .filter(|(_, deps)| deps.is_empty())
        .map(|(id, _)| *id)
        .collect();
    let mut resolved = 0usize;
    while let Some(id) = queue.pop() {
        resolved += 1;
        for dependent in dependents.get(id).into_iter().flatten() {
            if let Some(deps) = unresolved.get_mut(dependent) {
                deps.remove(id);
                if deps.is_empty() {
                    queue.push(dependent);
                }
            }
        }
    }

    if resolved != tasks.len() {
        let mut stuck: Vec<&str> = unresolved
            .iter()
            .filter(|(_, deps)| !deps.is_empty())
            .map(|(id, _)| *id)
            .collect();
        stuck.sort_unstable();
        bail!(
            "dependency cycle detected among tasks: {}",
            stuck.join(", ")
        );
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

    #[test]
    fn rejects_direct_cycle() {
        // A → B and B → A: the old orchestrator hung forever on this.
        let tasks = vec![task("a", &["src/a"], &["b"]), task("b", &["src/b"], &["a"])];
        let err = validate_tasks(&tasks).unwrap_err();
        assert!(format!("{err}").contains("cycle"), "{err}");
    }

    #[test]
    fn rejects_indirect_cycle() {
        let tasks = vec![
            task("a", &["src/a"], &["b"]),
            task("b", &["src/b"], &["c"]),
            task("c", &["src/c"], &["a"]),
        ];
        assert!(validate_tasks(&tasks).is_err());
    }

    #[test]
    fn accepts_dag_with_partial_cycle_free_branch() {
        // A resolvable branch plus a cyclic pair still rejects, and the error
        // names only the stuck tasks.
        let tasks = vec![
            task("root", &["src/r"], &[]),
            task("a", &["src/a"], &["b", "root"]),
            task("b", &["src/b"], &["a"]),
        ];
        let err = validate_tasks(&tasks).unwrap_err();
        assert!(format!("{err}").contains("a"), "{err}");
        assert!(format!("{err}").contains("b"), "{err}");
        assert!(!format!("{err}").contains("root"), "{err}");
    }
}

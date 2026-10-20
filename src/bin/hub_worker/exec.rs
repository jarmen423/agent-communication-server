//! Running the `--execute` command for one task (split out of hub_worker.rs).
//!
//! The command runs in its own process group. The prompt is streamed to its
//! stdin while stdout/stderr are drained concurrently (no pipe deadlock).
//! A timeout or a cancel (refocus-iteration-2.md §4.2) SIGKILLs the whole
//! group and reaps the child before returning. If the future is dropped
//! mid-run (the worker is stopping: SIGTERM / Ctrl-C), [`GroupGuard`] kills
//! the group too, so no grandchild outlives the worker.

use anyhow::{Context, Result};
use std::future::Future;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tracing::debug;

/// Max chars of stderr quoted in an error result.
const STDERR_TAIL: usize = 2000;

/// How one execution ended.
#[derive(Debug, PartialEq, Eq)]
pub enum ExecOutcome {
    /// Exit status 0; carries stdout.
    Done(String),
    /// Spawn failure, non-zero exit or timeout; carries the error message.
    Error(String),
    /// `cancel` resolved first; the process group was killed.
    Cancelled,
}

/// Execute `command`, streaming `prompt` to stdin. Kills the process group
/// when `timeout` elapses or `cancel` resolves.
pub async fn execute_command(
    command: &str,
    prompt: &str,
    timeout: Option<Duration>,
    cancel: impl Future<Output = ()>,
) -> ExecOutcome {
    match run(command, prompt, timeout, cancel).await {
        Ok(outcome) => outcome,
        Err(e) => ExecOutcome::Error(format!("{e:#}")),
    }
}

async fn run(
    command: &str,
    prompt: &str,
    timeout: Option<Duration>,
    cancel: impl Future<Output = ()>,
) -> Result<ExecOutcome> {
    debug!(%command, prompt_len = prompt.len(), "executing command");

    // Parse the command into program + args (simple split on whitespace)
    let parts: Vec<&str> = command.split_whitespace().collect();
    let Some((program, rest)) = parts.split_first() else {
        anyhow::bail!("empty execute command");
    };

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(rest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Own process group, so a timeout or cancel can kill the whole tree.
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn command: {command}"))?;
    // Declared after `child`, so on drop it runs first: the leader is not
    // reaped yet and its pid still names the group.
    let mut guard = GroupGuard::new(child.id());

    enum Ended {
        Finished(Result<(ExitStatus, Vec<u8>, Vec<u8>)>),
        TimedOut(Duration),
        Cancelled,
    }
    let ended = {
        let collect = collect_output(&mut child, prompt.as_bytes().to_vec());
        let limited = async {
            match timeout {
                None => Ended::Finished(collect.await),
                Some(limit) => match tokio::time::timeout(limit, collect).await {
                    Ok(res) => Ended::Finished(res),
                    Err(_) => Ended::TimedOut(limit),
                },
            }
        };
        tokio::select! {
            ended = limited => ended,
            () = cancel => Ended::Cancelled,
        }
    };

    let (status, stdout, stderr) = match ended {
        Ended::Finished(res) => {
            guard.disarm(); // the leader is reaped: its pid may be reused
            res?
        }
        Ended::TimedOut(limit) => {
            kill_tree(&mut child).await;
            anyhow::bail!("command timed out after {}s: {command}", limit.as_secs());
        }
        Ended::Cancelled => {
            kill_tree(&mut child).await;
            return Ok(ExecOutcome::Cancelled);
        }
    };

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let stderr = stderr.trim();
        let tail_start = stderr
            .char_indices()
            .rev()
            .nth(STDERR_TAIL)
            .map_or(0, |(i, _)| i);
        anyhow::bail!("command exited with {status}: {}", &stderr[tail_start..]);
    }
    Ok(ExecOutcome::Done(
        String::from_utf8_lossy(&stdout).into_owned(),
    ))
}

/// Write stdin, read stdout + stderr, and wait — all concurrently.
async fn collect_output(
    child: &mut Child,
    input: Vec<u8>,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut stdin = child.stdin.take().context("child stdin not piped")?;
    let mut stdout = child.stdout.take().context("child stdout not piped")?;
    let mut stderr = child.stderr.take().context("child stderr not piped")?;

    let write = async move {
        let res = stdin.write_all(&input).await;
        drop(stdin); // close → EOF for the child
        match res {
            // The child may exit without consuming all input; not our error.
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            other => other,
        }
    };
    let read_out = async {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).await.map(|_| buf)
    };
    let read_err = async {
        let mut buf = Vec::new();
        stderr.read_to_end(&mut buf).await.map(|_| buf)
    };

    let (w, out, err) = tokio::join!(write, read_out, read_err);
    w.context("writing prompt to stdin")?;
    let out = out.context("reading stdout")?;
    let err = err.context("reading stderr")?;
    let status = child.wait().await.context("waiting for command")?;
    Ok((status, out, err))
}

/// SIGKILL the child's process group (unix), then the child itself, and reap it.
async fn kill_tree(child: &mut Child) {
    if let Some(pid) = child.id() {
        kill_group(pid);
    }
    let _ = child.kill().await;
}

/// SIGKILL process group `pgid` (the child was spawned with
/// `process_group(0)`, so its pid is the group id). No-op off unix.
fn kill_group(pgid: u32) {
    #[cfg(unix)]
    if let Ok(pgid) = libc::pid_t::try_from(pgid) {
        if pgid > 1 {
            // SAFETY: killpg only sends a signal; any pgid value is memory-safe.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = pgid;
}

/// Kills the command's process group when dropped while armed — i.e. when
/// the run is abandoned (worker shutdown) rather than finished.
struct GroupGuard(Option<u32>);

impl GroupGuard {
    fn new(pgid: Option<u32>) -> Self {
        Self(pgid)
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pgid) = self.0.take() {
            kill_group(pgid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn done_timeout_and_cancel() {
        let never = std::future::pending::<()>;
        assert_eq!(
            execute_command("cat", "hi", None, never()).await,
            ExecOutcome::Done("hi".into())
        );
        let timed = execute_command("sleep 30", "", Some(Duration::from_millis(200)), never());
        assert!(matches!(timed.await, ExecOutcome::Error(e) if e.contains("timed out")));
        let started = std::time::Instant::now();
        let cancel = tokio::time::sleep(Duration::from_millis(200));
        assert_eq!(
            execute_command("sleep 30", "", None, cancel).await,
            ExecOutcome::Cancelled
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Dropping a running execution (worker shutdown) kills the whole group.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_the_run_kills_the_group() {
        let dir = std::env::temp_dir().join(format!("hub-worker-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("grandchild.pid");
        let script = dir.join("spawn.sh");
        std::fs::write(
            &script,
            format!("sleep 60 &\necho $! > {}\nwait\n", pidfile.display()),
        )
        .unwrap();
        let cmd = format!("sh {}", script.display());
        let never = std::future::pending::<()>();
        let run = execute_command(&cmd, "", None, never);
        let pid: i32 = tokio::select! {
            _ = run => panic!("command finished early"),
            pid = async {
                loop {
                    if let Ok(t) = std::fs::read_to_string(&pidfile) {
                        if let Ok(pid) = t.trim().parse() {
                            break pid;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            } => pid,
        }; // `run` is dropped here
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        // SAFETY: kill(pid, 0) only probes for existence.
        while unsafe { libc::kill(pid, 0) } == 0 && !is_zombie(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "grandchild {pid} survived"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn is_zombie(pid: i32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| {
                s.rsplit(')')
                    .next()
                    .unwrap_or("")
                    .trim_start()
                    .starts_with('Z')
            })
            .unwrap_or(false)
    }
}

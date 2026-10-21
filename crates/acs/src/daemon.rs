//! Credential refresh daemon and platform user-service installation.
use crate::{agents, local};
use anyhow::{bail, Context, Result};
use rand::Rng;
use std::{collections::HashMap, path::Path, time::Duration};

pub fn running(root: &Path) -> bool {
    local::lock(root, "daemon.lock").is_err()
}

async fn stopped() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! { _ = term.recv() => (), _ = tokio::signal::ctrl_c() => () }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub async fn foreground(root: &Path) -> Result<()> {
    let _daemon = local::lock(root, "daemon.lock").context("daemon already running")?;
    local::load(root)?;
    local::atomic(
        &root.join("daemon.pid"),
        std::process::id().to_string().as_bytes(),
    )?;
    eprintln!("acs credential daemon started");
    let mut retries: HashMap<String, (u64, u32)> = HashMap::new();
    let stop = stopped();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = tokio::time::sleep(Duration::from_secs(1)) => (),
        }
        let Ok(_operation) = local::lock(root, "operation.lock") else {
            continue;
        };
        let mut state = match local::load(root) {
            Ok(s) => s,
            Err(_) => {
                eprintln!("Login removed; credential daemon stopping");
                break;
            }
        };
        let ids: Vec<_> = state.agents.keys().cloned().collect();
        for id in ids {
            let now = local::now();
            if retries.get(&id).is_some_and(|(next, _)| *next > now) {
                continue;
            }
            let remaining = state.agents[&id].expires_at.saturating_sub(now);
            // 15–25 seconds of jitter avoids a herd and covers short test TTLs.
            if remaining > rand::thread_rng().gen_range(15..=25) {
                continue;
            }
            match agents::refresh(root, &mut state, &id).await {
                Ok(()) => {
                    retries.remove(&id);
                }
                Err(e) => {
                    let attempt = retries
                        .get(&id)
                        .map_or(0, |(_, n)| *n)
                        .saturating_add(1)
                        .min(8);
                    let delay = (1_u64 << attempt).min(60) + rand::thread_rng().gen_range(0..=3);
                    retries.insert(id.clone(), (now + delay, attempt));
                    eprintln!("Refresh {id} failed ({e}); retry in {delay}s");
                }
            }
        }
    }
    let _ = std::fs::remove_file(root.join("daemon.pid"));
    eprintln!("acs credential daemon stopped");
    Ok(())
}

pub async fn down(root: &Path) -> Result<()> {
    if running(root) {
        let pid: i32 = std::fs::read_to_string(root.join("daemon.pid"))?.parse()?;
        if pid <= 1 {
            bail!("invalid daemon pid");
        }
        #[cfg(unix)]
        {
            // Only the PID recorded by the process holding daemon.lock is used.
            if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
                bail!("cannot stop credential daemon");
            }
        }
        for _ in 0..100 {
            if !running(root) {
                println!("Daemon stopped");
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!("daemon did not stop within 10 seconds");
    }
    println!("Daemon is stopped");
    Ok(())
}

pub async fn up(root: &Path) -> Result<()> {
    local::load(root)?;
    let exe = std::env::current_exe()?;
    if cfg!(target_os = "linux") {
        let unit = local::home()?.join(".config/systemd/user/acs.service");
        // systemd expands percent specifiers even inside quoted arguments.
        let quote = |s: &str| {
            format!(
                "\"{}\"",
                s.replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('%', "%%")
            )
        };
        let contents = format!("[Unit]\nDescription=ACS device credential manager\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={} up --foreground\nEnvironment={}\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
            quote(&exe.to_string_lossy()), quote(&format!("ACS_HOME={}", root.display())));
        local::atomic(&unit, contents.as_bytes())?;
        for args in [
            vec!["--user", "daemon-reload"],
            vec!["--user", "enable", "--now", "acs.service"],
        ] {
            if !tokio::process::Command::new("systemctl")
                .args(args)
                .status()
                .await?
                .success()
            {
                bail!(
                    "systemd user service unavailable; installed {}; run acs up --foreground",
                    unit.display()
                );
            }
        }
        println!("Installed and started {}", unit.display());
    } else if cfg!(target_os = "macos") {
        let path = local::home()?.join("Library/LaunchAgents/com.agentmemorylabs.acs.plist");
        let xml = |s: &str| {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
        };
        let contents = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>Label</key><string>com.agentmemorylabs.acs</string><key>ProgramArguments</key><array><string>{}</string><string>up</string><string>--foreground</string></array><key>EnvironmentVariables</key><dict><key>ACS_HOME</key><string>{}</string></dict><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict></dict></plist>", xml(&exe.to_string_lossy()), xml(&root.to_string_lossy()));
        local::atomic(&path, contents.as_bytes())?;
        if !tokio::process::Command::new("launchctl")
            .arg("load")
            .arg(&path)
            .status()
            .await?
            .success()
        {
            bail!("launchd load failed");
        }
        println!("Installed {}", path.display());
    } else {
        bail!("user service installation supports Linux and macOS; use --foreground");
    }
    Ok(())
}

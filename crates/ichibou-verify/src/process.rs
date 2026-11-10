//! Shell commands run in their own process group, so a dev server and
//! everything it spawned can be stopped together.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use ichibou_core::sandbox::Guard;
use tokio::process::{Child, Command};

fn command(
    line: &str,
    cwd: &Path,
    env: &[(String, String)],
    log: &Path,
    guard: &Guard,
) -> Result<Command> {
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("cannot open {}", log.display()))?;
    let err = out.try_clone()?;
    // The command line runs under the guard: it and everything it starts
    // cannot write where the guard forbids.
    let mut command = match guard.prefix() {
        Some(prefix) => {
            let mut command = Command::new(&prefix[0]);
            command.args(&prefix[1..]).arg("sh");
            command
        }
        None => Command::new("sh"),
    };
    command
        .arg("-c")
        .arg(line)
        .current_dir(cwd)
        .envs(env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .kill_on_drop(true);
    Ok(command)
}

fn kill_group(child: &Child) {
    if let Some(pid) = child.id() {
        // The child leads its own group, so the group id equals its pid.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
}

/// Runs a command to completion. Returns its exit code, or `None` when it
/// was killed for exceeding `timeout`.
pub async fn run(
    line: &str,
    cwd: &Path,
    env: &[(String, String)],
    log: &Path,
    timeout: Duration,
    guard: &Guard,
) -> Result<Option<i32>> {
    let mut child = command(line, cwd, env, log, guard)?
        .spawn()
        .with_context(|| format!("cannot run `{line}`"))?;
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => Ok(Some(status?.code().unwrap_or(-1))),
        Err(_) => {
            kill_group(&child);
            let _ = child.kill().await;
            Ok(None)
        }
    }
}

/// A long-running process (an app under test).
pub struct Background {
    child: Child,
}

impl Background {
    pub fn spawn(
        line: &str,
        cwd: &Path,
        env: &[(String, String)],
        log: &Path,
        guard: &Guard,
    ) -> Result<Self> {
        let child = command(line, cwd, env, log, guard)?
            .spawn()
            .with_context(|| format!("cannot run `{line}`"))?;
        Ok(Self { child })
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Whether the process has already exited.
    pub fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    pub async fn stop(mut self) {
        kill_group(&self.child);
        if tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .is_err()
        {
            if let Some(pid) = self.child.id() {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = self.child.kill().await;
        }
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        kill_group(&self.child);
    }
}

/// Asks the OS for a free loopback port.
pub fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Waits until `url` answers at all (any HTTP status).
pub async fn wait_for_url(url: &str, timeout: Duration, process: &mut Background) -> bool {
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if process.exited() {
            return false;
        }
        if client
            .get(url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok()
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

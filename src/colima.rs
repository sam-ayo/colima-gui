//! Colima CLI calls. Colima has no API, so the app uses the CLI for VM control only.

use std::process::Command;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Status {
    docker_socket: Option<String>,
}

/// A GUI app gets a short PATH. Add the usual Homebrew locations so that
/// `colima` and the tools it calls (`limactl`, `docker`) are found.
fn colima() -> Command {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut cmd = Command::new("colima");
    cmd.env("PATH", format!("{path}:/opt/homebrew/bin:/usr/local/bin"));
    cmd
}

/// Returns the Docker socket that `colima status --json` reports.
pub fn docker_socket() -> Option<String> {
    let out = colima()
        .args(["status", "--json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let status: Status = serde_json::from_slice(&out.stdout).ok()?;
    status.docker_socket.filter(|s| !s.is_empty())
}

/// Runs `colima start` and waits until it finishes.
pub fn start() -> Result<()> {
    let out = colima()
        .arg("start")
        .output()
        .context("could not run `colima`. Make sure that it is on the PATH")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        bail!("colima start failed: {last}");
    }
    Ok(())
}

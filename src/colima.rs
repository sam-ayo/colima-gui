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

/// One Colima profile, from `colima list --json`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Machine {
    pub name: String,
    pub status: String,
}

impl Machine {
    pub fn is_running(&self) -> bool {
        self.status == "Running"
    }
}

/// Reads the profiles. `colima list --json` writes one JSON object on each
/// line. An empty list means that the command failed.
pub fn list() -> Vec<Machine> {
    let Ok(out) = colima().args(["list", "--json"]).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_list(&String::from_utf8_lossy(&out.stdout))
}

fn parse_list(text: &str) -> Vec<Machine> {
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Runs `colima start` for the default profile and waits until it finishes.
pub fn start() -> Result<()> {
    run(&["start"])
}

/// Starts or stops one profile and waits until the command finishes.
pub fn set_running(profile: &str, running: bool) -> Result<()> {
    run(&[if running { "start" } else { "stop" }, "--profile", profile])
}

fn run(args: &[&str]) -> Result<()> {
    let out = colima()
        .args(args)
        .output()
        .context("could not run `colima`. Make sure that it is on the PATH")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        bail!("colima {} failed: {last}", args[0]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_has_one_machine_on_each_line() {
        let text = concat!(
            r#"{"name":"default","status":"Running","arch":"aarch64","cpus":4}"#,
            "\n",
            r#"{"name":"k8s-lab","status":"Stopped","arch":"aarch64","cpus":2}"#,
            "\n",
        );
        let list = parse_list(text);
        assert_eq!(list.len(), 2);
        assert!(list[0].is_running());
        assert_eq!(list[1].name, "k8s-lab");
        assert!(!list[1].is_running());
    }
}

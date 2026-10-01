//! Colima CLI calls. Colima has no API, so the app uses the CLI for VM control only.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Status {
    docker_socket: Option<String>,
    #[serde(default)]
    kubernetes: bool,
}

/// A GUI app gets a short PATH. Add the usual Homebrew locations so that
/// `colima` and the tools it calls (`limactl`, `docker`) are found. The
/// tools in the app bundle come last, so an installed Colima has priority.
fn colima() -> Command {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut path = format!("{path}:/opt/homebrew/bin:/usr/local/bin");
    if let Some(tools) = bundled_tools() {
        path = format!("{path}:{}", tools.display());
    }
    let mut cmd = Command::new("colima");
    cmd.env("PATH", path);
    cmd
}

/// The `bin` folder of the tools in the app bundle
/// (`Colima GUI.app/Contents/Resources/tools/bin`). `scripts/package.sh` puts
/// the tools there. A `cargo run` build has no bundle.
fn bundled_tools() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?.join("Resources/tools/bin");
    dir.is_dir().then_some(dir)
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
    /// True if Kubernetes runs in the machine. `colima list` does not tell
    /// this, so `list` gets it from `colima status`.
    #[serde(default)]
    pub kubernetes: bool,
}

impl Machine {
    pub fn is_running(&self) -> bool {
        self.status == "Running"
    }

    /// The configuration file of the profile. `colima start --edit`
    /// changes the same file.
    pub fn config_file(&self) -> Option<PathBuf> {
        let home = std::env::var_os("COLIMA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".colima")))?;
        let file = home.join(&self.name).join("colima.yaml");
        file.is_file().then_some(file)
    }
}

/// An action on a profile, as the `colima` commands give them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineAction {
    Start,
    Stop,
    /// A stop without a graceful shutdown.
    ForceStop,
    Restart,
    /// Updates the container runtime in the machine.
    Update,
    StartKubernetes,
    StopKubernetes,
    /// Deletes the Kubernetes cluster and makes a new one.
    ResetKubernetes,
    /// Deletes the machine. The container data stays, as with
    /// `colima delete` without `--data`.
    Delete,
}

impl MachineAction {
    fn args(self) -> &'static [&'static str] {
        match self {
            MachineAction::Start => &["start"],
            MachineAction::Stop => &["stop"],
            MachineAction::ForceStop => &["stop", "--force"],
            MachineAction::Restart => &["restart"],
            MachineAction::Update => &["update"],
            MachineAction::StartKubernetes => &["kubernetes", "start"],
            MachineAction::StopKubernetes => &["kubernetes", "stop"],
            MachineAction::ResetKubernetes => &["kubernetes", "reset"],
            MachineAction::Delete => &["delete", "--force"],
        }
    }

    /// True if the engine runs when the action ends.
    pub fn leaves_engine_running(self) -> bool {
        !matches!(self, MachineAction::Stop | MachineAction::ForceStop | MachineAction::Delete)
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
    let mut list = parse_list(&String::from_utf8_lossy(&out.stdout));
    for m in list.iter_mut().filter(|m| m.is_running()) {
        m.kubernetes = status(&m.name).is_some_and(|s| s.kubernetes);
    }
    list
}

fn status(profile: &str) -> Option<Status> {
    let out = colima().args(["status", "--json", "--profile", profile]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
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

/// Runs an action on one profile and waits until the command finishes.
pub fn run_action(profile: &str, action: MachineAction) -> Result<()> {
    let mut args = action.args().to_vec();
    args.extend(["--profile", profile]);
    run(&args)
}

/// Opens a Terminal window with `colima ssh` for a profile. A `.command`
/// file opens in Terminal without the automation permission that an
/// AppleScript needs.
pub fn ssh_in_terminal(profile: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let file = std::env::temp_dir().join(format!("colima-ssh-{profile}.command"));
    let quoted = profile.replace('\'', r"'\''");
    std::fs::write(&file, format!("#!/bin/sh\nclear\nexec colima ssh --profile '{quoted}'\n"))?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))?;
    let status = Command::new("open").arg(&file).status()?;
    if !status.success() {
        bail!("could not open Terminal");
    }
    Ok(())
}

fn run(args: &[&str]) -> Result<()> {
    let out = colima()
        .args(args)
        .output()
        .context("could not run `colima`. Make sure that it is on the PATH")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        let command = args.iter().take_while(|a| !a.starts_with('-')).copied().collect::<Vec<_>>();
        bail!("colima {} failed: {last}", command.join(" "));
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

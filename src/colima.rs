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
        let file = home()?.join(&self.name).join("colima.yaml");
        file.is_file().then_some(file)
    }
}

/// The folder of the Colima profiles: `$COLIMA_HOME`, or `~/.colima`.
fn home() -> Option<PathBuf> {
    std::env::var_os("COLIMA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".colima")))
}

/// The folder of the Lima machines that Colima makes. Each machine has a
/// folder in it.
pub fn lima_home() -> Option<PathBuf> {
    Some(home()?.join("_lima"))
}

/// True if a Colima machine runs. This check does not start the CLI, so it
/// is fast. The Lima host agent of a running machine writes its process ID
/// to `ha.pid`, and removes the file when the machine stops. The Dock tile
/// plug-in (`macos/DockTile`) does the same check.
pub fn any_running() -> bool {
    let Some(Ok(dirs)) = lima_home().map(std::fs::read_dir) else {
        return false;
    };
    dirs.flatten().any(|dir| {
        std::fs::read_to_string(dir.path().join("ha.pid"))
            .ok()
            .and_then(|pid| pid.trim().parse::<libc::pid_t>().ok())
            // SAFETY: signal 0 sends no signal. It only checks that the
            // process exists.
            .is_some_and(|pid| pid > 0 && unsafe { libc::kill(pid, 0) } == 0)
    })
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

/// Stops all the running profiles at the same time, and waits until all
/// the commands finish. If `colima list` fails, Colima does not run, and
/// there is nothing to stop.
pub fn stop_running(force: bool) -> Result<()> {
    let Ok(out) = colima().args(["list", "--json"]).output() else {
        return Ok(());
    };
    let running: Vec<Machine> = parse_list(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .filter(Machine::is_running)
        .collect();
    let action = if force { MachineAction::ForceStop } else { MachineAction::Stop };
    std::thread::scope(|s| {
        let handles: Vec<_> = running
            .iter()
            .map(|m| s.spawn(move || run_action(&m.name, action)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("colima stop failed"))))
            .collect::<Result<Vec<()>>>()
            .map(drop)
    })
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

/// The kind of an entry in a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Folder,
    File,
    Link,
    Other,
}

/// One entry of a folder in the VM.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// The time of the last change, in seconds since the Unix epoch.
    pub modified: i64,
}

/// The most entries that `list_folder` returns. A larger folder shows the
/// first entries only.
pub const MAX_ENTRIES: usize = 2000;

/// Lists a folder in the VM of a profile. Root owns the Docker data, so
/// the call uses `sudo`, which needs no password in a Colima VM. The folders
/// come first, then the files, each sorted by name.
///
/// `colima ssh` quotes each argument for the shell in the VM, so a path
/// with spaces or quotes is safe.
pub fn list_folder(profile: &str, path: &str) -> Result<Vec<Entry>> {
    let out = colima()
        .args(["ssh", "--profile", profile, "--", "sudo", "find", path])
        .args(["-mindepth", "1", "-maxdepth", "1", "-printf", r"%y\t%s\t%T@\t%f\0"])
        .output()
        .context("could not run `colima`. Make sure that it is on the PATH")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        bail!("could not read the folder: {}", last.trim_start_matches("find: "));
    }
    Ok(parse_entries(&out.stdout))
}

/// The largest file that `copy_file` copies. A larger file goes through
/// SSH slowly, and the user can use `colima ssh` for it.
pub const MAX_COPY_SIZE: u64 = 100 * 1024 * 1024;

/// Copies a file from the VM of a profile to `dest` on the Mac. Root owns
/// the Docker data, so the call uses `sudo cat`.
pub fn copy_file(profile: &str, path: &str, dest: &std::path::Path) -> Result<()> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::File::create(dest)?;
    let out = colima()
        .args(["ssh", "--profile", profile, "--", "sudo", "cat", path])
        .stdout(file)
        .output()
        .context("could not run `colima`. Make sure that it is on the PATH")?;
    if !out.status.success() {
        let _ = std::fs::remove_file(dest);
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        bail!("could not copy the file: {}", last.trim_start_matches("cat: "));
    }
    Ok(())
}

/// Opens a file on the Mac in its default app. Returns `false` if no app
/// can open the file type.
pub fn open_file(path: &std::path::Path) -> Result<bool> {
    let out = Command::new("open").arg(path).output().context("could not run `open`")?;
    if out.status.success() {
        return Ok(true);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("kLSApplicationNotFoundErr") || err.contains("No application knows how to open") {
        return Ok(false);
    }
    bail!("could not open the file: {}", err.trim());
}

/// Reads the output of `find -printf '%y\t%s\t%T@\t%f\0'`. The name is the
/// last field, so a tab in a name does not break the line.
fn parse_entries(out: &[u8]) -> Vec<Entry> {
    let mut entries: Vec<Entry> = out
        .split(|&b| b == 0)
        .filter_map(|line| {
            let line = String::from_utf8_lossy(line);
            let mut parts = line.splitn(4, '\t');
            let kind = match parts.next()? {
                "d" => EntryKind::Folder,
                "f" => EntryKind::File,
                "l" => EntryKind::Link,
                _ => EntryKind::Other,
            };
            let size = parts.next()?.parse().ok()?;
            let modified = parts.next()?.split('.').next()?.parse().ok()?;
            let name = parts.next()?.to_string();
            Some(Entry { name, kind, size, modified })
        })
        .collect();
    entries.sort_by(|a, b| {
        (a.kind != EntryKind::Folder, &a.name).cmp(&(b.kind != EntryKind::Folder, &b.name))
    });
    entries.truncate(MAX_ENTRIES);
    entries
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
    fn folder_entries_put_folders_first() {
        let out = b"f\t416\t1788353434.0000000000\tkeyfile\0d\t4096\t1788353000.5\tjournal\0f\t3\t1\ta\tb\0";
        let entries = parse_entries(out);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["journal", "a\tb", "keyfile"]);
        assert_eq!(entries[0].kind, EntryKind::Folder);
        assert_eq!(entries[2].size, 416);
        assert_eq!(entries[2].modified, 1_788_353_434);
    }

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

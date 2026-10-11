//! Colima CLI calls. Colima has no API, so the app uses the CLI for VM control only.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Status {
    docker_socket: Option<String>,
}

/// A GUI app gets a short PATH. Add the usual Homebrew locations so that
/// `colima` and the tools it calls (`limactl`, `docker`) are found. The
/// tools in the app bundle come last, so an installed Colima has priority.
fn colima() -> Command {
    let mut cmd = Command::new("colima");
    cmd.env("PATH", search_path());
    cmd
}

/// The PATH for the CLI tools that the app runs. See `colima`.
pub fn search_path() -> String {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut path = format!("{path}:/opt/homebrew/bin:/usr/local/bin");
    if let Some(tools) = bundled_tools() {
        path = format!("{path}:{}", tools.display());
    }
    path
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
    /// Deletes the machine. The container data stays, as with
    /// `colima delete` without `--data`.
    Delete,
    /// Deletes the machine and its container data: the images, the
    /// containers, and the volumes.
    DeleteWithData,
}

impl MachineAction {
    fn args(self) -> &'static [&'static str] {
        match self {
            MachineAction::Start => &["start"],
            MachineAction::Stop => &["stop"],
            MachineAction::ForceStop => &["stop", "--force"],
            MachineAction::Restart => &["restart"],
            MachineAction::Update => &["update"],
            MachineAction::Delete => &["delete", "--force"],
            MachineAction::DeleteWithData => &["delete", "--force", "--data"],
        }
    }

    /// True if the engine runs when the action ends.
    pub fn leaves_engine_running(self) -> bool {
        !matches!(
            self,
            MachineAction::Stop | MachineAction::ForceStop | MachineAction::Delete | MachineAction::DeleteWithData
        )
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

/// The container runtime of a machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Runtime {
    #[default]
    Docker,
    Containerd,
}

impl Runtime {
    pub const ALL: [Runtime; 2] = [Runtime::Docker, Runtime::Containerd];

    /// The value of `--runtime` and of `runtime:` in `colima.yaml`.
    pub fn arg(self) -> &'static str {
        match self {
            Runtime::Docker => "docker",
            Runtime::Containerd => "containerd",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Runtime::Docker => "Docker",
            Runtime::Containerd => "containerd",
        }
    }
}

/// The virtualization of a machine. `vz` is the Apple Virtualization
/// framework. `qemu` is slower, but it can emulate other architectures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VmType {
    #[default]
    Vz,
    Qemu,
}

impl VmType {
    pub const ALL: [VmType; 2] = [VmType::Vz, VmType::Qemu];

    /// The value of `--vm-type` and of `vmType:` in `colima.yaml`.
    pub fn arg(self) -> &'static str {
        match self {
            VmType::Vz => "vz",
            VmType::Qemu => "qemu",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            VmType::Vz => "Apple Virtualization (vz)",
            VmType::Qemu => "QEMU",
        }
    }
}

/// A folder of the Mac that the machine can read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub location: String,
    pub writable: bool,
}

/// The settings of a machine that the app can change. The other settings
/// in `colima.yaml` keep their values.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub cpus: u32,
    /// The memory in GiB.
    pub memory: f32,
    /// The disk size in GiB.
    pub disk: u32,
    pub runtime: Runtime,
    pub vm_type: VmType,
    /// Rosetta runs x86_64 programs in the machine. It needs `vz`.
    pub rosetta: bool,
    /// An empty list uses the default mounts of Colima.
    pub mounts: Vec<Mount>,
}

impl Default for Settings {
    /// The defaults of `colima start`.
    fn default() -> Self {
        Settings {
            cpus: 2,
            memory: 2.,
            disk: 100,
            runtime: Runtime::Docker,
            vm_type: VmType::Vz,
            rosetta: false,
            mounts: Vec::new(),
        }
    }
}

impl Settings {
    /// The `colima start` flags for these settings. `--mount` replaces all
    /// the mounts, so the flags have no mount for an empty list.
    fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--cpus".to_string(),
            self.cpus.to_string(),
            "--memory".into(),
            format_gib(self.memory),
            "--disk".into(),
            self.disk.to_string(),
            "--runtime".into(),
            self.runtime.arg().into(),
            "--vm-type".into(),
            self.vm_type.arg().into(),
            format!("--vz-rosetta={}", self.rosetta && self.vm_type == VmType::Vz),
        ];
        for m in &self.mounts {
            args.push("--mount".into());
            args.push(if m.writable { format!("{}:w", m.location) } else { m.location.clone() });
        }
        args
    }
}

/// A size in GiB, with no fraction if it is a whole number.
pub fn format_gib(gib: f32) -> String {
    if gib.fract() == 0. { format!("{gib:.0}") } else { format!("{gib}") }
}

impl Machine {
    /// Reads the settings from `colima.yaml`. A missing value keeps the
    /// default.
    pub fn settings(&self) -> Option<Settings> {
        let text = std::fs::read_to_string(self.config_file()?).ok()?;
        Some(parse_settings(&text))
    }
}

/// Reads the top-level keys of `colima.yaml` that `Settings` has. Colima
/// writes the file, so a full YAML parser is not necessary.
fn parse_settings(text: &str) -> Settings {
    let mut s = Settings::default();
    let mut in_mounts = false;
    for line in text.lines() {
        let line = line.split(" #").next().unwrap_or(line).trim_end();
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let top = !line.starts_with(' ') && !line.starts_with('-');
        if top {
            in_mounts = false;
            let Some((key, value)) = line.split_once(':') else { continue };
            let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
            match key {
                "cpu" => s.cpus = value.parse().unwrap_or(s.cpus),
                "memory" => s.memory = value.parse().unwrap_or(s.memory),
                "disk" => s.disk = value.parse().unwrap_or(s.disk),
                "runtime" => {
                    s.runtime = Runtime::ALL.into_iter().find(|r| r.arg() == value).unwrap_or(s.runtime)
                }
                "vmType" => s.vm_type = VmType::ALL.into_iter().find(|v| v.arg() == value).unwrap_or(s.vm_type),
                "rosetta" => s.rosetta = value == "true",
                "mounts" => in_mounts = value.is_empty(),
                _ => {}
            }
            continue;
        }
        if !in_mounts {
            continue;
        }
        let item = line.trim_start();
        let (new, item) = match item.strip_prefix("- ") {
            Some(rest) => (true, rest.trim_start()),
            None => (false, item),
        };
        if new {
            s.mounts.push(Mount { location: String::new(), writable: false });
        }
        let Some(mount) = s.mounts.last_mut() else { continue };
        let Some((key, value)) = item.split_once(':') else { continue };
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        match key.trim() {
            "location" => mount.location = value.to_string(),
            "writable" => mount.writable = value == "true",
            _ => {}
        }
    }
    s.mounts.retain(|m| !m.location.is_empty());
    s
}

/// Replaces the `mounts:` block of `colima.yaml` with an empty list. The
/// machine then uses the default mounts of Colima. `colima start` cannot do
/// this, because a start without `--mount` keeps the saved mounts.
fn clear_mounts(text: &str) -> String {
    let mut out = Vec::new();
    let mut skip = false;
    for line in text.lines() {
        let top = !line.starts_with(' ') && !line.starts_with('-') && !line.trim().is_empty();
        if skip && !top {
            continue;
        }
        skip = false;
        if top && line.starts_with("mounts:") {
            out.push("mounts: []".to_string());
            skip = true;
            continue;
        }
        out.push(line.to_string());
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// True if `name` can be the name of a new profile. Colima uses the name
/// in file and folder names.
pub fn valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '.', '_'])
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Makes and starts a new profile, and waits until the command finishes.
pub fn create(profile: &str, settings: &Settings) -> Result<()> {
    let mut args = vec!["start".to_string(), "--profile".into(), profile.into()];
    args.extend(settings.args());
    run(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

/// Changes the settings of a profile. Colima reads the settings when the
/// machine starts, so a running machine stops first. The runtime and the
/// VM type of a machine cannot change, so the call keeps them.
pub fn configure(profile: &str, old: &Settings, new: &Settings, running: bool) -> Result<()> {
    if running {
        run_action(profile, MachineAction::Stop)?;
    }
    if new.mounts.is_empty()
        && !old.mounts.is_empty()
        && let Some(file) = home().map(|h| h.join(profile).join("colima.yaml"))
    {
        let text = std::fs::read_to_string(&file).context("could not read colima.yaml")?;
        std::fs::write(&file, clear_mounts(&text)).context("could not change colima.yaml")?;
    }
    let settings = Settings { runtime: old.runtime, vm_type: old.vm_type, ..new.clone() };
    create(profile, &settings)
}

/// The processor count and the memory of the Mac, in GiB. These are the
/// limits for a machine.
pub fn host_resources() -> (u32, f32) {
    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get() as u32);
    let mut memsize: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: `hw.memsize` is a 64-bit value, and `len` gives the size of
    // the buffer.
    let ok = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&raw mut memsize).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    let memory = if ok { memsize as f32 / (1u64 << 30) as f32 } else { 8. };
    (cpus, memory)
}

/// The counters of the resource use in a machine, from one read.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UsageCounters {
    /// The CPU time of all the processors, in clock ticks.
    pub cpu_total: u64,
    pub cpu_idle: u64,
    pub cpus: u32,
    pub mem_total: u64,
    pub mem_used: u64,
    /// The disk that has the container data.
    pub disk_total: u64,
    pub disk_used: u64,
}

/// The script that reads the counters in the VM. The disk is the one that
/// keeps the container data.
const USAGE_SCRIPT: &str = r#"head -n1 /proc/stat; grep -E '^(MemTotal|MemAvailable):' /proc/meminfo; for d in /var/lib/docker /var/lib/containerd /; do if [ -d "$d" ]; then df -Pk "$d"; break; fi; done; nproc"#;

/// Reads the resource counters of a running profile.
pub fn usage(profile: &str) -> Result<UsageCounters> {
    let out = colima()
        .args(["ssh", "--profile", profile, "--", "sh", "-c", USAGE_SCRIPT])
        .output()
        .context("could not run `colima`. Make sure that it is on the PATH")?;
    if !out.status.success() {
        bail!("could not read the resource use of “{profile}”");
    }
    parse_usage(&String::from_utf8_lossy(&out.stdout)).context("could not read the resource use")
}

fn parse_usage(text: &str) -> Option<UsageCounters> {
    let mut u = UsageCounters::default();
    let mut lines = text.lines();
    // cpu  user nice system idle iowait irq softirq steal ...
    let cpu: Vec<u64> = lines.next()?.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
    if cpu.len() < 5 {
        return None;
    }
    // The guest time is in the user time already, so the first eight
    // fields are the total.
    u.cpu_total = cpu.iter().take(8).sum();
    u.cpu_idle = cpu[3] + cpu[4];
    let mut available = 0;
    for line in lines {
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields.as_slice() {
            ["MemTotal:", kb, ..] => u.mem_total = kb.parse::<u64>().ok()? * 1024,
            ["MemAvailable:", kb, ..] => available = kb.parse::<u64>().ok()? * 1024,
            [dev, total, used, _, _, _] if dev.starts_with('/') => {
                u.disk_total = total.parse::<u64>().ok()? * 1024;
                u.disk_used = used.parse::<u64>().ok()? * 1024;
            }
            [n] => u.cpus = n.parse().unwrap_or(0),
            _ => {}
        }
    }
    u.mem_used = u.mem_total.saturating_sub(available);
    Some(u)
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
    fn settings_come_from_colima_yaml() {
        let text = "# Number of CPUs\ncpu: 4\ndisk: 60\nmemory: 2.5\narch: aarch64\nruntime: containerd\n\
            kubernetes:\n  enabled: false\nvmType: qemu\nrosetta: true\nmounts:\n  - location: ~/src\n    \
            writable: true\n  - location: /tmp/x\nmountType: virtiofs\n";
        let s = parse_settings(text);
        assert_eq!(s.cpus, 4);
        assert_eq!(s.disk, 60);
        assert_eq!(s.memory, 2.5);
        assert_eq!(s.runtime, Runtime::Containerd);
        assert_eq!(s.vm_type, VmType::Qemu);
        assert!(s.rosetta);
        assert_eq!(
            s.mounts,
            [
                Mount { location: "~/src".into(), writable: true },
                Mount { location: "/tmp/x".into(), writable: false },
            ]
        );
        assert!(parse_settings("mounts: []\n").mounts.is_empty());
    }

    #[test]
    fn settings_become_start_flags() {
        let s = Settings {
            memory: 1.5,
            mounts: vec![Mount { location: "/a b".into(), writable: true }],
            ..Settings::default()
        };
        let args = s.args();
        assert_eq!(args[..6], ["--cpus", "2", "--memory", "1.5", "--disk", "100"]);
        assert!(args.contains(&"--vz-rosetta=false".to_string()));
        assert_eq!(args[args.len() - 2..], ["--mount", "/a b:w"]);
    }

    #[test]
    fn clearing_mounts_keeps_the_other_keys() {
        let text = "cpu: 2\nmounts:\n  - location: /a\n    writable: true\nmountType: sshfs\n";
        assert_eq!(clear_mounts(text), "cpu: 2\nmounts: []\nmountType: sshfs\n");
    }

    #[test]
    fn profile_names_are_safe() {
        assert!(valid_profile_name("dev-2.x_y"));
        assert!(!valid_profile_name(""));
        assert!(!valid_profile_name("-x"));
        assert!(!valid_profile_name("a b"));
        assert!(!valid_profile_name("a/b"));
    }

    #[test]
    fn usage_comes_from_proc_and_df() {
        let text = "cpu  758 0 646 128472 139 0 58 0 0 0\nMemTotal:        3999652 kB\n\
            MemAvailable:    3575804 kB\nFilesystem     1024-blocks    Used Available Capacity Mounted on\n\
            /dev/vdb1         40971488 1619560  37238496       5% /var/lib/docker\n2\n";
        let u = parse_usage(text).unwrap();
        assert_eq!(u.cpu_total, 758 + 646 + 128472 + 139 + 58);
        assert_eq!(u.cpu_idle, 128472 + 139);
        assert_eq!(u.cpus, 2);
        assert_eq!(u.mem_total, 3999652 * 1024);
        assert_eq!(u.mem_used, (3999652 - 3575804) * 1024);
        assert_eq!(u.disk_total, 40971488 * 1024);
        assert_eq!(u.disk_used, 1619560 * 1024);
    }

    #[test]
    fn list_has_one_machine_on_each_line() {
        let text = concat!(
            r#"{"name":"default","status":"Running","arch":"aarch64","cpus":4}"#,
            "\n",
            r#"{"name":"dev","status":"Stopped","arch":"aarch64","cpus":2}"#,
            "\n",
        );
        let list = parse_list(text);
        assert_eq!(list.len(), 2);
        assert!(list[0].is_running());
        assert_eq!(list[1].name, "dev");
        assert!(!list[1].is_running());
    }
}

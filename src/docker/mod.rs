//! Docker Engine API access through the Unix socket (bollard).

pub mod ansi;
pub mod compose;
pub mod containers;
pub mod events;
pub mod image_logo;
pub mod images;
pub mod logs;
pub mod networks;
pub mod stats;
pub mod volumes;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bollard::{API_DEFAULT_VERSION, Docker};

pub use containers::ContainerRow;

#[derive(Clone, Debug, PartialEq)]
pub enum EngineStatus {
    Connecting,
    Connected,
    /// The socket is missing or does not answer. Colima is probably stopped.
    NotRunning,
    Error(String),
}

/// Finds the Docker socket: `DOCKER_HOST`, then the default Colima socket,
/// then `colima status --json`. The CLI call takes about 0.5 s, so the app
/// uses it only when the default socket does not exist (another profile).
pub fn discover_socket() -> String {
    if let Ok(host) = std::env::var("DOCKER_HOST")
        && host.starts_with("unix://") {
            return host;
        }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let default = home.join(".colima/default/docker.sock");
    if default.exists() {
        return format!("unix://{}", default.display());
    }
    if let Some(sock) = crate::colima::docker_socket() {
        return sock;
    }
    format!("unix://{}", default.display())
}

/// Reads "dev" from a socket such as `unix:///Users/me/.colima/dev/docker.sock`.
/// A socket that is not in a Colima profile gives `None`.
pub fn profile_of(socket: &str) -> Option<&str> {
    let rest = &socket[socket.find("/.colima/")? + "/.colima/".len()..];
    rest.split('/').next().filter(|p| !p.is_empty())
}

/// Connects to the engine at `socket` and checks it with `ping`.
pub async fn connect(socket: &str) -> (Option<Docker>, EngineStatus) {
    tracing::debug!(%socket, "connect");
    let path = socket.trim_start_matches("unix://");
    if !std::path::Path::new(path).exists() {
        return (None, EngineStatus::NotRunning);
    }
    let docker = match Docker::connect_with_unix(socket, 30, API_DEFAULT_VERSION) {
        Ok(d) => d,
        Err(e) => return (None, EngineStatus::Error(e.to_string())),
    };
    match tokio::time::timeout(std::time::Duration::from_secs(3), docker.ping()).await {
        Ok(Ok(_)) => (Some(docker), EngineStatus::Connected),
        // A socket file that does not answer stays after the VM stops.
        Ok(Err(_)) | Err(_) => (None, EngineStatus::NotRunning),
    }
}

/// Checks the name of a volume or a network with the rule of the engine:
/// a letter or a digit first, then letters, digits, `_`, `.`, or `-`. The
/// engine needs at least two characters.
pub fn check_name(name: &str) -> std::result::Result<(), &'static str> {
    let mut chars = name.chars();
    match chars.next() {
        None => return Err("Type a name."),
        Some(c) if !c.is_ascii_alphanumeric() => return Err("The name must start with a letter or a digit."),
        _ => {}
    }
    if name.len() < 2 {
        return Err("The name must have at least two characters.");
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
        return Err("Use only letters, digits, “_”, “.”, and “-”.");
    }
    Ok(())
}

/// The Docker socket of one Colima profile.
pub fn profile_socket(profile: &str) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    format!("unix://{}", home.join(".colima").join(profile).join("docker.sock").display())
}

const READY_POLL: Duration = Duration::from_millis(500);

/// Pings `socket` until the engine answers or until `done` is true.
/// `colima start` finishes about 5 s after the engine answers, so the app
/// can show the engine before the command ends. Returns true when the
/// engine answers.
pub async fn wait_until_ready(socket: String, done: Arc<AtomicBool>) -> bool {
    let path = socket.trim_start_matches("unix://").to_string();
    while !done.load(Ordering::Relaxed) {
        if Path::new(&path).exists()
            && let Ok(docker) = Docker::connect_with_unix(&socket, 2, API_DEFAULT_VERSION)
            && let Ok(Ok(_)) = tokio::time::timeout(Duration::from_secs(2), docker.ping()).await
        {
            return true;
        }
        tokio::time::sleep(READY_POLL).await;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{check_name, profile_of};

    #[test]
    fn names_follow_the_engine_rule() {
        assert!(check_name("db_data").is_ok());
        assert!(check_name("a1.b-c").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name("a").is_err());
        assert!(check_name("_db").is_err());
        assert!(check_name("my data").is_err());
        assert!(check_name("dät").is_err());
    }

    #[test]
    fn profile_comes_from_the_socket_path() {
        assert_eq!(profile_of("unix:///Users/me/.colima/dev/docker.sock"), Some("dev"));
        assert_eq!(profile_of("unix:///var/run/docker.sock"), None);
    }
}

/// Tests against the real engine. They create their own containers.
/// Run them with `cargo test -- --ignored`.
#[cfg(test)]
mod live_tests {
    use std::process::Command;
    use std::time::Duration;

    use futures::StreamExt;

    use super::*;
    use crate::state::log_buffer::{LogBuffer, MAX_LINES};

    fn docker_cli(args: &[&str]) {
        let _ = Command::new("docker").args(args).output();
    }

    async fn client() -> Docker {
        let (docker, status) = connect(&discover_socket()).await;
        assert_eq!(status, EngineStatus::Connected);
        docker.unwrap()
    }

    #[tokio::test]
    #[ignore = "needs a running Docker engine"]
    async fn log_stream_fills_buffer_to_limit() {
        docker_cli(&["rm", "-f", "cgui-spam"]);
        docker_cli(&["run", "--name", "cgui-spam", "alpine", "sh", "-c", "seq 1 200000"]);
        let docker = client().await;
        // `tail=1000` limits the history, so read all lines for this test.
        let opts = bollard::query_parameters::LogsOptions {
            stdout: true,
            stderr: true,
            timestamps: true,
            tail: "all".into(),
            ..Default::default()
        };
        let mut splitter = logs::LineSplitter::default();
        let mut buf = LogBuffer::new();
        let mut s = docker.logs("cgui-spam", Some(opts));
        while let Some(Ok(out)) = s.next().await {
            buf.extend(splitter.push(logs::LogStream::Stdout, &out.into_bytes()));
        }
        docker_cli(&["rm", "-f", "cgui-spam"]);
        assert_eq!(buf.len(), MAX_LINES);
        assert_eq!(buf.evicted, 150_000);
        assert_eq!(buf.visible_line(MAX_LINES - 1).unwrap().text, "200000");
        assert!(buf.visible_line(0).unwrap().time.is_some());
    }

    #[tokio::test]
    #[ignore = "needs a running Docker engine"]
    async fn follow_gets_tail_then_ends_when_container_exits() {
        docker_cli(&["rm", "-f", "cgui-tail"]);
        docker_cli(&["run", "--name", "cgui-tail", "alpine", "sh", "-c", "seq 1 5000"]);
        let docker = client().await;
        let lines: Vec<_> = tokio::time::timeout(
            Duration::from_secs(10),
            logs::follow(&docker, "cgui-tail").collect::<Vec<_>>(),
        )
        .await
        .expect("the stream must end for a stopped container");
        docker_cli(&["rm", "-f", "cgui-tail"]);
        assert_eq!(lines.len(), logs::TAIL_LINES);
        assert_eq!(lines.last().unwrap().text, "5000");
    }

    #[tokio::test]
    #[ignore = "needs a running Docker engine and the network"]
    async fn pull_then_read_an_image() {
        let docker = client().await;
        let mut progress = images::PullProgress::default();
        let mut s = std::pin::pin!(images::pull(&docker, "hello-world:latest"));
        while let Some(item) = s.next().await {
            progress.apply(&item.expect("the pull must not fail"));
        }
        assert!(progress.status.is_some_and(|s| s.starts_with("Status:")));
        let rows = images::list_all(&docker).await.unwrap();
        let row = rows.iter().find(|r| r.tags.iter().any(|t| t == "hello-world:latest")).unwrap();
        assert!(row.size > 0);
        assert!(!images::history(&docker, &row.id).await.unwrap().is_empty());
        let info = images::inspect(&docker, &row.id).await.unwrap();
        assert_eq!(info.command.as_deref(), Some("/hello"));
        assert!(info.platform.is_some_and(|p| p.starts_with("linux/")));
        // A pull of a name that does not exist ends with the engine message.
        let errors: Vec<String> = images::pull(&docker, "cgui-missing/nothing:none")
            .filter_map(|r| async move { r.err() })
            .collect()
            .await;
        assert_eq!(errors.len(), 1);
        assert!(!errors[0].starts_with("Docker stream error"));
    }

    #[tokio::test]
    #[ignore = "needs a running Docker engine"]
    async fn stats_and_events_for_a_running_container() {
        docker_cli(&["rm", "-f", "cgui-stats"]);
        let docker = client().await;
        let (tx, rx) = async_channel::unbounded();
        let pump = tokio::spawn(events::pump(docker.clone(), tx));
        assert_eq!(rx.recv().await.unwrap(), events::EngineEvent::Resync);
        docker_cli(&["run", "-d", "--name", "cgui-stats", "nginx:alpine"]);
        let started = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let events::EngineEvent::Container { action, .. } = rx.recv().await.unwrap() {
                    if action == "start" {
                        break;
                    }
                }
            }
        })
        .await;
        assert!(started.is_ok(), "no start event");

        let samples: Vec<_> = stats::stream(&docker, "cgui-stats").take(2).collect().await;
        assert_eq!(samples.len(), 2);
        assert!(samples[1].mem_bytes > 0);
        assert!(samples[1].mem_limit > 0);

        let row = containers::refresh_one(&docker, &containers::list_all(&docker)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.name == "cgui-stats")
            .unwrap()
            .id)
        .await
        .unwrap()
        .unwrap();
        assert!(row.state.is_running());
        containers::run_action(&docker, &row.id, containers::Action::Remove)
            .await
            .unwrap();
        assert!(containers::refresh_one(&docker, &row.id).await.unwrap().is_none());
        pump.abort();
    }
}

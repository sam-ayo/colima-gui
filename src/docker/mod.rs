//! Docker Engine API access through the Unix socket (bollard).

pub mod containers;
pub mod events;
pub mod image_logo;
pub mod logs;
pub mod stats;

use std::path::PathBuf;

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

/// The Colima profile that owns the socket that the app uses. The sidebar
/// marks this machine.
pub fn current_profile() -> String {
    std::env::var("DOCKER_HOST")
        .ok()
        .and_then(|host| profile_of(&host).map(str::to_string))
        .unwrap_or_else(|| "default".into())
}

/// Reads "dev" from a socket such as `unix:///Users/me/.colima/dev/docker.sock`.
fn profile_of(socket: &str) -> Option<&str> {
    let rest = &socket[socket.find("/.colima/")? + "/.colima/".len()..];
    rest.split('/').next().filter(|p| !p.is_empty())
}

/// Connects to the engine and checks it with `ping`.
pub async fn connect() -> (Option<Docker>, EngineStatus) {
    let socket = discover_socket();
    tracing::debug!(%socket, "socket found");
    let path = socket.trim_start_matches("unix://");
    if !std::path::Path::new(path).exists() {
        return (None, EngineStatus::NotRunning);
    }
    let docker = match Docker::connect_with_unix(&socket, 30, API_DEFAULT_VERSION) {
        Ok(d) => d,
        Err(e) => return (None, EngineStatus::Error(e.to_string())),
    };
    match tokio::time::timeout(std::time::Duration::from_secs(3), docker.ping()).await {
        Ok(Ok(_)) => (Some(docker), EngineStatus::Connected),
        // A socket file that does not answer stays after the VM stops.
        Ok(Err(_)) | Err(_) => (None, EngineStatus::NotRunning),
    }
}

#[cfg(test)]
mod tests {
    use super::profile_of;

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
        let (docker, status) = connect().await;
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
        assert!(!buf.visible_line(0).unwrap().timestamp.is_empty());
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

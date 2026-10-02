//! Container list and actions.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use bollard::Docker;
use bollard::models::{ContainerSummary, ContainerSummaryStateEnum, PortSummary};
use bollard::query_parameters::{
    ListContainersOptions, RemoveContainerOptions, RestartContainerOptions, StartContainerOptions,
    StopContainerOptions,
};

pub const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";
pub const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";
/// The Compose files of the project, separated by commas.
pub const COMPOSE_FILES_LABEL: &str = "com.docker.compose.project.config_files";
/// An icon URL for the container. OrbStack reads the same label.
pub const ICON_LABEL: &str = "dev.orbstack.icon";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Created,
    Running,
    Paused,
    Restarting,
    Exited,
    Removing,
    Dead,
    Unknown,
}

impl State {
    pub fn is_running(self) -> bool {
        matches!(self, State::Running | State::Restarting | State::Paused)
    }
}

impl From<Option<ContainerSummaryStateEnum>> for State {
    fn from(s: Option<ContainerSummaryStateEnum>) -> Self {
        use ContainerSummaryStateEnum as S;
        match s {
            Some(S::CREATED) => State::Created,
            Some(S::RUNNING) => State::Running,
            Some(S::PAUSED) => State::Paused,
            Some(S::RESTARTING) => State::Restarting,
            Some(S::EXITED) | Some(S::STOPPING) => State::Exited,
            Some(S::REMOVING) => State::Removing,
            Some(S::DEAD) => State::Dead,
            _ => State::Unknown,
        }
    }
}

/// The part of a container that the list shows.
#[derive(Clone, Debug, PartialEq)]
pub struct ContainerRow {
    pub id: String,
    pub name: String,
    pub image: String,
    /// The full ID of the image, for example "sha256:858f009f…".
    pub image_id: String,
    pub state: State,
    pub status: String,
    pub ports: Vec<Port>,
    pub compose_project: Option<String>,
    pub compose_service: Option<String>,
    /// The main Compose file of the project: the first file in the label.
    pub compose_file: Option<String>,
    pub icon: Option<String>,
    pub created: i64,
    /// The time when the container exited, in milliseconds since the Unix
    /// epoch. The list does not include it, so only exited Compose members
    /// get it, from the inspect data.
    pub finished_at: Option<i64>,
    /// The names of the volumes that the container mounts.
    pub volumes: Vec<String>,
}

impl ContainerRow {
    pub fn short_id(&self) -> &str {
        &self.id[..self.id.len().min(12)]
    }

    /// True for a dead container, or one that exited with an error code.
    /// Docker writes the code in the status, for example "Exited (1) 4
    /// minutes ago". A stop signal is not an error: `docker stop` sends
    /// SIGTERM (143), then SIGKILL (137) after the timeout.
    pub fn failed(&self) -> bool {
        match self.state {
            State::Dead => true,
            State::Exited => {
                exit_code(&self.status).is_some_and(|c| c != 0 && !is_stop_signal(c))
            }
            _ => false,
        }
    }

    /// True if `failed` is true and the Compose project did not stop at the
    /// same time. A stop sends a signal to all the members, but some
    /// programs exit with an error code on the signal. Thus an error exit
    /// is a stop if a different member stopped cleanly near the same time.
    /// `rows` can contain containers of other projects.
    pub fn failed_in<'a>(&self, rows: impl IntoIterator<Item = &'a ContainerRow>) -> bool {
        if !self.failed() {
            return false;
        }
        let (Some(project), Some(at)) = (&self.compose_project, self.finished_at) else {
            return true;
        };
        !rows.into_iter().any(|r| {
            r.id != self.id
                && r.compose_project.as_ref() == Some(project)
                && r.state == State::Exited
                && !r.failed()
                && r.finished_at.is_some_and(|t| (t - at).abs() <= STOP_WINDOW_MS)
        })
    }

    /// True for a row that needs `finished_at` for `failed_in`.
    fn needs_finish_time(&self) -> bool {
        self.state == State::Exited && self.compose_project.is_some()
    }

    /// The status in two parts: the state and the time. "Up 2 hours" gives
    /// ("Running", "2 hours"), and "Exited (1) 4 minutes ago" gives
    /// ("Exited (1)", "4 minutes ago").
    pub fn status_parts(&self) -> (String, Option<String>) {
        let s = self.status.as_str();
        if let Some(rest) = s.strip_prefix("Up ") {
            let head = if self.state == State::Paused { "Paused" } else { "Running" };
            return (head.into(), Some(rest.trim_end_matches(" (Paused)").into()));
        }
        if s.starts_with("Exited (")
            && let Some(end) = s.find(')')
        {
            let tail = s[end + 1..].trim();
            return (s[..=end].into(), (!tail.is_empty()).then(|| tail.into()));
        }
        (s.into(), None)
    }

    /// The time since the container exited, from a status such as "Exited
    /// (0) 2 hours ago". Docker writes the time in words, so the value is
    /// approximate. It is `None` for a container that did not exit.
    pub fn exited_ago(&self) -> Option<Duration> {
        let (head, time) = self.status_parts();
        if !head.starts_with("Exited") {
            return None;
        }
        parse_ago(&time?)
    }
}

/// Reads a Docker time such as "2 hours ago", "About a minute ago", or
/// "Less than a second ago".
fn parse_ago(s: &str) -> Option<Duration> {
    let s = s.strip_suffix(" ago")?;
    if s.starts_with("Less than") {
        return Some(Duration::ZERO);
    }
    let (count, unit) = s.split_once(' ')?;
    let count = match count {
        "About" | "a" | "an" => 1,
        n => n.parse().ok()?,
    };
    let unit = unit.trim_start_matches("a ").trim_start_matches("an ");
    let secs = match unit.trim_end_matches('s') {
        "second" => 1,
        "minute" => 60,
        "hour" => 3_600,
        "day" => 86_400,
        "week" => 604_800,
        "month" => 2_592_000,
        "year" => 31_536_000,
        _ => return None,
    };
    Some(Duration::from_secs(count * secs))
}

/// Reads the code from an "Exited (N) …" status.
fn exit_code(status: &str) -> Option<i64> {
    let rest = status.strip_prefix("Exited (")?;
    rest[..rest.find(')')?].parse().ok()
}

/// The longest time between the exits of two members in one stop. A member
/// that does not stop on SIGTERM gets SIGKILL after the stop timeout, which
/// is 10 seconds by default.
const STOP_WINDOW_MS: i64 = 15_000;

/// True for the codes of a process that a stop signal ended: 128 + SIGINT
/// (130), SIGKILL (137), or SIGTERM (143).
fn is_stop_signal(code: i64) -> bool {
    matches!(code, 130 | 137 | 143)
}

/// A container port that Docker publishes on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Port {
    pub public: u16,
    pub private: u16,
    pub proto: String,
}

impl Port {
    /// The address on the host, for example "localhost:3000".
    pub fn host(&self) -> String {
        format!("localhost:{}", self.public)
    }

    /// The URL that opens the port in a browser. Only TCP ports have one.
    pub fn url(&self) -> Option<String> {
        (self.proto == "tcp").then(|| format!("http://localhost:{}", self.public))
    }
}

impl From<ContainerSummary> for ContainerRow {
    fn from(c: ContainerSummary) -> Self {
        let labels = c.labels.unwrap_or_default();
        let name = c
            .names
            .and_then(|n| n.into_iter().next())
            .map(|n| n.trim_start_matches('/').to_string())
            .unwrap_or_default();
        ContainerRow {
            id: c.id.unwrap_or_default(),
            name,
            image: c.image.unwrap_or_default(),
            image_id: c.image_id.unwrap_or_default(),
            state: c.state.into(),
            status: c.status.unwrap_or_default(),
            ports: published_ports(c.ports.as_deref().unwrap_or_default()),
            compose_project: labels.get(COMPOSE_PROJECT_LABEL).cloned(),
            compose_service: labels.get(COMPOSE_SERVICE_LABEL).cloned(),
            compose_file: labels
                .get(COMPOSE_FILES_LABEL)
                .and_then(|files| files.split(',').next())
                .map(|f| f.trim().to_string())
                .filter(|f| !f.is_empty()),
            icon: labels.get(ICON_LABEL).cloned(),
            created: c.created.unwrap_or_default(),
            finished_at: None,
            volumes: c
                .mounts
                .iter()
                .flatten()
                .filter(|m| m.typ.as_deref() == Some("volume"))
                .filter_map(|m| m.name.clone())
                .collect(),
        }
    }
}

/// Returns the published ports, sorted by host port. Docker reports each
/// port one time for IPv4 and one time for IPv6, so duplicates are removed.
pub fn published_ports(ports: &[PortSummary]) -> Vec<Port> {
    let mut out: Vec<Port> = Vec::new();
    for p in ports {
        let Some(public) = p.public_port else { continue };
        let port = Port {
            public,
            private: p.private_port,
            proto: p.typ.map(|t| t.to_string()).unwrap_or_else(|| "tcp".into()),
        };
        if !out.contains(&port) {
            out.push(port);
        }
    }
    out.sort_by_key(|p| p.public);
    out
}

pub async fn list_all(docker: &Docker) -> Result<Vec<ContainerRow>> {
    let opts = ListContainersOptions {
        all: true,
        ..Default::default()
    };
    let list = docker.list_containers(Some(opts)).await?;
    let mut rows: Vec<ContainerRow> = list.into_iter().map(ContainerRow::from).collect();
    add_finish_times(docker, &mut rows).await;
    Ok(rows)
}

/// Reads one container. Returns `None` if the container does not exist.
pub async fn refresh_one(docker: &Docker, id: &str) -> Result<Option<ContainerRow>> {
    let opts = ListContainersOptions {
        all: true,
        filters: Some(HashMap::from([("id".to_string(), vec![id.to_string()])])),
        ..Default::default()
    };
    let list = docker.list_containers(Some(opts)).await?;
    let mut row = list
        .into_iter()
        .map(ContainerRow::from)
        .find(|row| row.id == id);
    add_finish_times(docker, row.as_mut_slice()).await;
    Ok(row)
}

/// Sets `finished_at` from the inspect data, for the rows that need it. An
/// inspect that fails leaves the value `None`, so the row can show as
/// failed, but the list still loads.
async fn add_finish_times(docker: &Docker, rows: &mut [ContainerRow]) {
    let rows: Vec<&mut ContainerRow> = rows.iter_mut().filter(|r| r.needs_finish_time()).collect();
    let times = futures::future::join_all(rows.iter().map(|r| async {
        let info = docker.inspect_container(&r.id, None).await.ok()?;
        parse_time(info.state?.finished_at.as_deref()?)
    }))
    .await;
    for (row, time) in rows.into_iter().zip(times) {
        row.finished_at = time;
    }
}

/// Reads a Docker time such as "2026-10-01T01:52:47.948022754Z" as
/// milliseconds since the Unix epoch.
fn parse_time(s: &str) -> Option<i64> {
    let t = chrono::DateTime::parse_from_rfc3339(s).ok()?.timestamp_millis();
    // Docker writes "0001-01-01T00:00:00Z" for a container that did not exit.
    (t > 0).then_some(t)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
    Remove,
}

impl Action {
    pub fn pending_label(self) -> &'static str {
        match self {
            Action::Start => "Starting…",
            Action::Stop => "Stopping…",
            Action::Restart => "Restarting…",
            Action::Remove => "Removing…",
        }
    }

    pub fn verb(self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
            Action::Remove => "remove",
        }
    }
}

pub async fn run_action(docker: &Docker, id: &str, action: Action) -> Result<()> {
    match action {
        Action::Start => {
            docker
                .start_container(id, None::<StartContainerOptions>)
                .await?
        }
        Action::Stop => {
            docker
                .stop_container(id, None::<StopContainerOptions>)
                .await?
        }
        Action::Restart => {
            docker
                .restart_container(id, None::<RestartContainerOptions>)
                .await?
        }
        Action::Remove => {
            let opts = RemoveContainerOptions {
                force: true,
                ..Default::default()
            };
            docker.remove_container(id, Some(opts)).await?
        }
    }
    Ok(())
}

/// A mount of a container, from the inspect data.
#[derive(Clone, Debug, PartialEq)]
pub struct Mount {
    /// A host path for a bind mount, or the name of a volume.
    pub source: String,
    pub destination: String,
    pub volume: bool,
}

/// The part of the inspect data that the list does not have.
pub struct Details {
    pub ip: Option<String>,
    pub mounts: Vec<Mount>,
    /// The full inspect data as pretty JSON.
    pub json: String,
}

pub async fn inspect(docker: &Docker, id: &str) -> Result<Details> {
    let info = docker.inspect_container(id, None).await?;
    // A container can be on more than one network. Show the first address.
    let ip = info
        .network_settings
        .as_ref()
        .and_then(|n| n.networks.as_ref())
        .and_then(|nets| {
            let mut ips: Vec<&String> = nets
                .values()
                .filter_map(|e| e.ip_address.as_ref())
                .filter(|ip| !ip.is_empty())
                .collect();
            ips.sort();
            ips.first().map(|ip| ip.to_string())
        });
    let mounts = info
        .mounts
        .iter()
        .flatten()
        .map(|m| {
            let volume = m.typ.as_deref() == Some("volume");
            let source = if volume { m.name.clone() } else { m.source.clone() };
            Mount {
                source: source.unwrap_or_default(),
                destination: m.destination.clone().unwrap_or_default(),
                volume,
            }
        })
        .collect();
    Ok(Details {
        ip,
        mounts,
        json: serde_json::to_string_pretty(&info)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::PortSummaryTypeEnum;

    #[test]
    fn status_splits_into_state_and_time() {
        let mut row = ContainerRow {
            id: String::new(),
            name: String::new(),
            image: String::new(),
            image_id: String::new(),
            state: State::Running,
            status: "Up 2 hours".into(),
            ports: Vec::new(),
            compose_project: None,
            compose_service: None,
            compose_file: None,
            icon: None,
            created: 0,
            finished_at: None,
            volumes: Vec::new(),
        };
        assert_eq!(row.status_parts(), ("Running".into(), Some("2 hours".into())));
        row.state = State::Exited;
        row.status = "Exited (1) 4 minutes ago".into();
        assert_eq!(row.status_parts(), ("Exited (1)".into(), Some("4 minutes ago".into())));
        assert!(row.failed());
        row.status = "Exited (137) 6 minutes ago".into();
        assert!(!row.failed());
        row.status = "Exited (143) 6 minutes ago".into();
        assert!(!row.failed());
        row.state = State::Created;
        row.status = "Created".into();
        assert_eq!(row.status_parts(), ("Created".into(), None));
        assert_eq!(row.exited_ago(), None);
    }

    #[test]
    fn exit_time_reads_docker_words() {
        let ago = |s: &str| parse_ago(s).map(|d| d.as_secs());
        assert_eq!(ago("Less than a second ago"), Some(0));
        assert_eq!(ago("45 seconds ago"), Some(45));
        assert_eq!(ago("About a minute ago"), Some(60));
        assert_eq!(ago("4 minutes ago"), Some(240));
        assert_eq!(ago("About an hour ago"), Some(3_600));
        assert_eq!(ago("2 hours ago"), Some(7_200));
        assert_eq!(ago("3 days ago"), Some(259_200));
        assert_eq!(ago("2 weeks ago"), Some(1_209_600));
        assert_eq!(ago("soon"), None);
    }

    #[test]
    fn an_error_exit_in_a_project_stop_is_not_a_failure() {
        let member = |id: &str, status: &str, at: i64| ContainerRow {
            id: id.into(),
            name: id.into(),
            image: String::new(),
            image_id: String::new(),
            state: State::Exited,
            status: status.into(),
            ports: Vec::new(),
            compose_project: Some("api".into()),
            compose_service: Some(id.into()),
            compose_file: None,
            icon: None,
            created: 0,
            finished_at: Some(at),
            volumes: Vec::new(),
        };
        let mongo = member("mongo", "Exited (2) 6 minutes ago", 47_948);
        let redis = member("redis", "Exited (0) 6 minutes ago", 48_009);
        let server = member("server", "Exited (137) 6 minutes ago", 57_970);
        assert!(!mongo.failed_in([&mongo, &redis, &server]));
        assert!(!mongo.failed_in([&mongo, &server]));
        // A crash alone, or long before the stop of the others.
        assert!(mongo.failed_in([&mongo]));
        let late = member("late", "Exited (0) 1 hour ago", 47_948 + 3_600_000);
        assert!(mongo.failed_in([&mongo, &late]));
        // A member that also failed does not make the exit a stop.
        let worker = member("worker", "Exited (1) 6 minutes ago", 48_000);
        assert!(mongo.failed_in([&mongo, &worker]));
        // A member of a different project does not count.
        let other = ContainerRow { compose_project: Some("shop".into()), ..redis.clone() };
        assert!(mongo.failed_in([&mongo, &other]));
    }

    #[test]
    fn docker_times_read_as_milliseconds() {
        assert_eq!(parse_time("1970-01-01T00:00:01.5Z"), Some(1_500));
        assert_eq!(parse_time("2026-10-01T01:52:47.948022754Z"), Some(1_790_819_567_948));
        assert_eq!(parse_time("0001-01-01T00:00:00Z"), None);
        assert_eq!(parse_time("soon"), None);
    }

    #[test]
    fn exit_codes_come_from_the_status() {
        assert_eq!(exit_code("Exited (1) 4 minutes ago"), Some(1));
        assert_eq!(exit_code("Exited (0) 2 hours ago"), Some(0));
        assert_eq!(exit_code("Up 2 hours"), None);
    }

    #[test]
    fn ports_are_deduplicated_and_sorted() {
        let p = |public: Option<u16>, private: u16, typ| PortSummary {
            ip: None,
            private_port: private,
            public_port: public,
            typ: Some(typ),
        };
        let ports = vec![
            p(Some(8080), 80, PortSummaryTypeEnum::TCP),
            p(Some(8080), 80, PortSummaryTypeEnum::TCP),
            p(None, 443, PortSummaryTypeEnum::TCP),
            p(Some(53), 53, PortSummaryTypeEnum::UDP),
        ];
        let port = |public, private, proto: &str| Port {
            public,
            private,
            proto: proto.into(),
        };
        assert_eq!(
            published_ports(&ports),
            vec![port(53, 53, "udp"), port(8080, 80, "tcp")]
        );
        assert_eq!(port(53, 53, "udp").url(), None);
        assert_eq!(port(8080, 80, "tcp").url().as_deref(), Some("http://localhost:8080"));
    }
}

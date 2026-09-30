//! Container list and actions.

use std::collections::HashMap;

use anyhow::Result;
use bollard::Docker;
use bollard::models::{ContainerSummary, ContainerSummaryStateEnum, PortSummary};
use bollard::query_parameters::{
    ListContainersOptions, RemoveContainerOptions, RestartContainerOptions, StartContainerOptions,
    StopContainerOptions,
};

pub const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";
pub const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";
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
    pub state: State,
    pub status: String,
    pub ports: String,
    pub compose_project: Option<String>,
    pub compose_service: Option<String>,
    pub icon: Option<String>,
    pub created: i64,
}

impl ContainerRow {
    pub fn short_id(&self) -> &str {
        &self.id[..self.id.len().min(12)]
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
            state: c.state.into(),
            status: c.status.unwrap_or_default(),
            ports: format_ports(c.ports.as_deref().unwrap_or_default()),
            compose_project: labels.get(COMPOSE_PROJECT_LABEL).cloned(),
            compose_service: labels.get(COMPOSE_SERVICE_LABEL).cloned(),
            icon: labels.get(ICON_LABEL).cloned(),
            created: c.created.unwrap_or_default(),
        }
    }
}

/// Formats published ports as `host->container/proto`. Docker reports each
/// port one time for IPv4 and one time for IPv6, so duplicates are removed.
pub fn format_ports(ports: &[PortSummary]) -> String {
    let mut out: Vec<String> = Vec::new();
    for p in ports {
        let Some(public) = p.public_port else { continue };
        let proto = p.typ.map(|t| t.to_string()).unwrap_or_else(|| "tcp".into());
        let s = if proto == "tcp" {
            format!("{public}->{}", p.private_port)
        } else {
            format!("{public}->{}/{proto}", p.private_port)
        };
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out.join(", ")
}

pub async fn list_all(docker: &Docker) -> Result<Vec<ContainerRow>> {
    let opts = ListContainersOptions {
        all: true,
        ..Default::default()
    };
    let list = docker.list_containers(Some(opts)).await?;
    Ok(list.into_iter().map(ContainerRow::from).collect())
}

/// Reads one container. Returns `None` if the container does not exist.
pub async fn refresh_one(docker: &Docker, id: &str) -> Result<Option<ContainerRow>> {
    let opts = ListContainersOptions {
        all: true,
        filters: Some(HashMap::from([("id".to_string(), vec![id.to_string()])])),
        ..Default::default()
    };
    let list = docker.list_containers(Some(opts)).await?;
    Ok(list
        .into_iter()
        .map(ContainerRow::from)
        .find(|row| row.id == id))
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

pub async fn inspect_json(docker: &Docker, id: &str) -> Result<String> {
    let info = docker.inspect_container(id, None).await?;
    Ok(serde_json::to_string_pretty(&info)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::PortSummaryTypeEnum;

    #[test]
    fn ports_are_deduplicated_and_formatted() {
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
        assert_eq!(format_ports(&ports), "8080->80, 53->53/udp");
    }
}

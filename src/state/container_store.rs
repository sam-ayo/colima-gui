//! The list of containers and the engine status.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use bollard::Docker;
use futures::channel::oneshot;
use gpui::{Context, EventEmitter, Global, Task};
use indexmap::IndexMap;

use crate::docker::containers::{self, Action};
use crate::docker::events::{self, EngineEvent};
use crate::docker::{self as engine, ContainerRow, EngineStatus};
use crate::runtime::{Backend, recv_batch};

/// Container rows, pending actions, and a generation number per id.
/// This part has no GPUI types, so unit tests can use it directly.
#[derive(Default)]
pub struct ContainerSet {
    pub rows: IndexMap<String, ContainerRow>,
    pub pending: HashMap<String, Action>,
    /// Each event for an id increments its generation. A refresh result that
    /// has an old generation is out of date, so the set ignores it.
    gens: HashMap<String, u64>,
}

impl ContainerSet {
    pub fn bump(&mut self, id: &str) -> u64 {
        let g = self.gens.entry(id.to_string()).or_default();
        *g += 1;
        *g
    }

    pub fn generation(&self, id: &str) -> u64 {
        self.gens.get(id).copied().unwrap_or(0)
    }

    /// Applies the result of `refresh_one`. `None` means that the container
    /// does not exist. Returns `true` if the set changed.
    pub fn apply_refresh(&mut self, id: &str, generation: u64, row: Option<ContainerRow>) -> bool {
        if self.generation(id) != generation {
            return false;
        }
        match row {
            Some(row) => {
                if self.rows.get(id) == Some(&row) {
                    return false;
                }
                self.rows.insert(id.to_string(), row);
                true
            }
            None => self.remove(id),
        }
    }

    pub fn remove(&mut self, id: &str) -> bool {
        self.gens.remove(id);
        self.pending.remove(id);
        self.rows.shift_remove(id).is_some()
    }

    /// Replaces all rows. Returns `true` if a row is different.
    pub fn replace_all(&mut self, rows: Vec<ContainerRow>) -> bool {
        let rows: IndexMap<String, ContainerRow> =
            rows.into_iter().map(|r| (r.id.clone(), r)).collect();
        let changed = rows != self.rows;
        self.rows = rows;
        let rows = &self.rows;
        self.pending.retain(|id, _| rows.contains_key(id));
        self.gens.retain(|id, _| rows.contains_key(id));
        changed
    }
}

/// What one batch of engine events asks the store to do.
#[derive(Debug, Default, PartialEq)]
pub struct EventPlan {
    /// `Some(true)` if the engine came back, `Some(false)` if it went away.
    pub link: Option<bool>,
    pub resync: bool,
    pub refresh: Vec<String>,
    pub remove: Vec<String>,
}

pub fn plan_events(batch: &[EngineEvent]) -> EventPlan {
    let mut plan = EventPlan::default();
    let mut refresh = Vec::new();
    let mut seen = HashSet::new();
    for ev in batch {
        match ev {
            EngineEvent::Disconnected => {
                plan = EventPlan {
                    link: Some(false),
                    ..Default::default()
                };
                refresh.clear();
                seen.clear();
            }
            EngineEvent::Resync => {
                plan.link = Some(true);
                plan.resync = true;
            }
            EngineEvent::Container { id, action } => {
                if action == "destroy" {
                    plan.remove.push(id.clone());
                    refresh.retain(|r| r != id);
                    seen.remove(id);
                } else if seen.insert(id.clone()) {
                    plan.remove.retain(|r| r != id);
                    refresh.push(id.clone());
                }
            }
        }
    }
    // A full resync reads every row, so single refreshes are not necessary.
    if !plan.resync {
        plan.refresh = refresh;
    }
    plan
}

/// One line of the grouped list.
#[derive(Clone, Debug, PartialEq)]
pub enum ListItem {
    Group {
        project: String,
        total: usize,
        running: usize,
        collapsed: bool,
    },
    Row {
        id: String,
        grouped: bool,
    },
}

/// Groups rows by Compose project. Groups come first, sorted by name. Then
/// the containers that have no project. `filter` matches the name, image,
/// or project, and does not care about case.
pub fn build_list<'a>(
    rows: impl Iterator<Item = &'a ContainerRow>,
    filter: &str,
    collapsed: &HashSet<String>,
) -> Vec<ListItem> {
    let filter = filter.trim().to_lowercase();
    let matches = |r: &ContainerRow| {
        filter.is_empty()
            || r.name.to_lowercase().contains(&filter)
            || r.image.to_lowercase().contains(&filter)
            || r
                .compose_project
                .as_deref()
                .is_some_and(|p| p.to_lowercase().contains(&filter))
    };
    let mut groups: BTreeMap<&str, Vec<&ContainerRow>> = BTreeMap::new();
    let mut single: Vec<&ContainerRow> = Vec::new();
    for r in rows.filter(|r| matches(r)) {
        match r.compose_project.as_deref() {
            Some(p) => groups.entry(p).or_default().push(r),
            None => single.push(r),
        }
    }
    let mut out = Vec::new();
    for (project, mut members) in groups {
        members.sort_by(|a, b| a.name.cmp(&b.name));
        let is_collapsed = collapsed.contains(project);
        out.push(ListItem::Group {
            project: project.to_string(),
            total: members.len(),
            running: members.iter().filter(|r| r.state.is_running()).count(),
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            out.extend(members.into_iter().map(|r| ListItem::Row {
                id: r.id.clone(),
                grouped: true,
            }));
        }
    }
    single.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(single.into_iter().map(|r| ListItem::Row {
        id: r.id.clone(),
        grouped: false,
    }));
    out
}

type ConnectResult = (Option<Docker>, EngineStatus, Option<Vec<ContainerRow>>);

/// Connects, and also loads the first list, so the rows are ready early.
async fn connect_and_list() -> ConnectResult {
    let (docker, status) = engine::connect().await;
    let rows = match &docker {
        Some(d) => containers::list_all(d).await.ok(),
        None => None,
    };
    (docker, status, rows)
}

/// The first connection, started in `main` before GPUI initializes.
pub struct EarlyConnect(Option<oneshot::Receiver<ConnectResult>>);

impl Global for EarlyConnect {}

impl EarlyConnect {
    pub fn start(backend: &Backend) -> Self {
        EarlyConnect(Some(backend.spawn(connect_and_list())))
    }
}

const STATUS_REFRESH: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub enum StoreEvent {
    Changed,
    Error(String),
}

pub struct ContainerStore {
    docker: Option<Docker>,
    pub status: EngineStatus,
    pub set: ContainerSet,
    pub starting_colima: bool,
    events_task: Option<Task<()>>,
    retry_task: Option<Task<()>>,
    /// Docker sends the status text ("Up 5 minutes") as a fixed string, and
    /// no event tells when it changes. A slow full reload keeps it current.
    status_task: Option<Task<()>>,
    /// True from the connection until the first full list arrives.
    loading: bool,
}

impl EventEmitter<StoreEvent> for ContainerStore {}

impl ContainerStore {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut this = ContainerStore {
            docker: None,
            status: EngineStatus::Connecting,
            set: ContainerSet::default(),
            starting_colima: false,
            events_task: None,
            retry_task: None,
            status_task: None,
            loading: false,
        };
        this.connect(cx);
        this
    }

    pub fn docker(&self) -> Option<&Docker> {
        self.docker.as_ref()
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(StoreEvent::Changed);
        cx.notify();
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let early = cx
            .try_global::<EarlyConnect>()
            .is_some_and(|e| e.0.is_some());
        let rx = if early {
            cx.global_mut::<EarlyConnect>().0.take().unwrap()
        } else {
            Backend::global(cx).spawn(connect_and_list())
        };
        cx.spawn(async move |this, cx| {
            let Ok((docker, status, rows)) = rx.await else { return };
            let _ = this.update(cx, |this, cx| match docker {
                Some(docker) => {
                    if let Some(rows) = rows {
                        let _ = this.set.replace_all(rows);
                        this.loading = false;
                    }
                    this.on_connected(docker, cx)
                }
                None => this.on_down(status, cx),
            });
        })
        .detach();
    }

    fn on_connected(&mut self, docker: Docker, cx: &mut Context<Self>) {
        tracing::debug!("store connected");
        self.retry_task = None;
        self.docker = Some(docker.clone());
        self.status = EngineStatus::Connected;
        self.loading = self.set.rows.is_empty();
        let (tx, rx) = async_channel::unbounded();
        let backend = Backend::global(cx);
        // The pump stops when `rx` closes, which happens when this task drops.
        drop(backend.spawn(events::pump(docker, tx)));
        self.events_task = Some(cx.spawn(async move |this, cx| {
            while let Some(batch) = recv_batch(&rx, 1024).await {
                if this.update(cx, |this, cx| this.handle_events(batch, cx)).is_err() {
                    break;
                }
            }
        }));
        self.status_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(STATUS_REFRESH).await;
                let ok = this.update(cx, |this, cx| {
                    if this.status == EngineStatus::Connected {
                        this.resync(cx);
                    }
                });
                if ok.is_err() {
                    break;
                }
            }
        }));
        self.changed(cx);
    }

    /// The engine is not available and no client exists. Try again slowly,
    /// so a `colima start` in a terminal also brings the list back.
    fn on_down(&mut self, status: EngineStatus, cx: &mut Context<Self>) {
        self.status = status;
        self.loading = false;
        self.retry_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            let _ = this.update(cx, |this, cx| {
                if !this.starting_colima {
                    this.connect(cx)
                }
            });
        }));
        self.changed(cx);
    }

    fn handle_events(&mut self, batch: Vec<EngineEvent>, cx: &mut Context<Self>) {
        let plan = plan_events(&batch);
        tracing::debug!(?batch, "engine events");
        match plan.link {
            Some(false) => {
                self.status = EngineStatus::NotRunning;
                self.set.pending.clear();
                self.changed(cx);
                return;
            }
            Some(true) => self.status = EngineStatus::Connected,
            None => {}
        }
        let mut changed = plan.link.is_some();
        for id in &plan.remove {
            changed |= self.set.remove(id);
        }
        if plan.resync {
            self.resync(cx);
        } else if plan.refresh.len() > 32 {
            // Many changes at one time: one full list is faster.
            self.resync(cx);
        } else {
            for id in plan.refresh {
                self.refresh(id, cx);
            }
        }
        if changed {
            self.changed(cx);
        }
    }

    fn resync(&mut self, cx: &mut Context<Self>) {
        let Some(docker) = self.docker.clone() else { return };
        let rx = Backend::global(cx).spawn(async move { containers::list_all(&docker).await });
        cx.spawn(async move |this, cx| {
            let Ok(result) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
                let mut changed = std::mem::take(&mut this.loading);
                match result {
                    Ok(rows) => {
                        tracing::debug!(count = rows.len(), "full list loaded");
                        changed |= this.set.replace_all(rows);
                    }
                    Err(e) => {
                        cx.emit(StoreEvent::Error(format!("Could not list containers: {e}")));
                        changed = true;
                    }
                }
                if changed {
                    this.changed(cx);
                }
            });
        })
        .detach();
    }

    fn refresh(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(docker) = self.docker.clone() else { return };
        let generation = self.set.bump(&id);
        let id2 = id.clone();
        let rx = Backend::global(cx).spawn(async move { containers::refresh_one(&docker, &id2).await });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(row)) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.set.apply_refresh(&id, generation, row) {
                    tracing::debug!(%id, "row updated");
                    this.changed(cx);
                }
            });
        })
        .detach();
    }

    /// Runs an action. The row shows a pending state until the call ends.
    /// An error resets the row and emits `StoreEvent::Error`.
    pub fn run_action(&mut self, id: &str, action: Action, cx: &mut Context<Self>) {
        let Some(docker) = self.docker.clone() else { return };
        if self.set.pending.contains_key(id) {
            return;
        }
        self.set.pending.insert(id.to_string(), action);
        self.changed(cx);
        let id = id.to_string();
        let id2 = id.clone();
        let rx = Backend::global(cx)
            .spawn(async move { containers::run_action(&docker, &id2, action).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("cancelled")));
            let _ = this.update(cx, |this, cx| {
                this.set.pending.remove(&id);
                if let Err(e) = result {
                    let name = this
                        .set
                        .rows
                        .get(&id)
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| id.clone());
                    cx.emit(StoreEvent::Error(format!(
                        "Could not {} {name}: {}",
                        action.verb(),
                        clean_error(&e)
                    )));
                }
                // Read the row again. Some calls (for example a stop of a
                // stopped container) do not send an event.
                this.refresh(id.clone(), cx);
                this.changed(cx);
            });
        })
        .detach();
    }

    /// Runs an action on each container of a Compose project.
    pub fn run_group_action(&mut self, project: &str, action: Action, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .set
            .rows
            .values()
            .filter(|r| r.compose_project.as_deref() == Some(project))
            .filter(|r| match action {
                Action::Start => !r.state.is_running(),
                Action::Stop => r.state.is_running(),
                Action::Restart | Action::Remove => true,
            })
            .map(|r| r.id.clone())
            .collect();
        for id in ids {
            self.run_action(&id, action, cx);
        }
    }

    pub fn start_colima(&mut self, cx: &mut Context<Self>) {
        if self.starting_colima {
            return;
        }
        self.starting_colima = true;
        self.retry_task = None;
        self.changed(cx);
        let rx = Backend::global(cx).spawn(async move {
            tokio::task::spawn_blocking(crate::colima::start)
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r)
        });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("cancelled")));
            let _ = this.update(cx, |this, cx| {
                this.starting_colima = false;
                if let Err(e) = result {
                    cx.emit(StoreEvent::Error(e.to_string()));
                }
                if this.status != EngineStatus::Connected {
                    // An events pump that exists reconnects by itself.
                    if this.events_task.is_none() {
                        this.connect(cx);
                    }
                }
                this.changed(cx);
            });
        })
        .detach();
    }
}

/// Docker errors contain the HTTP status. Keep only the message.
fn clean_error(e: &anyhow::Error) -> String {
    let s = e.to_string();
    let s = match s.split_once("status code ") {
        Some((_, rest)) => rest.split_once(": ").map_or(rest, |(_, msg)| msg),
        None => &s,
    };
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::containers::State;

    fn row(id: &str, name: &str, project: Option<&str>, state: State) -> ContainerRow {
        ContainerRow {
            id: id.into(),
            name: name.into(),
            image: "nginx".into(),
            state,
            status: String::new(),
            ports: String::new(),
            compose_project: project.map(Into::into),
            compose_service: None,
            created: 0,
        }
    }

    fn ev(id: &str, action: &str) -> EngineEvent {
        EngineEvent::Container {
            id: id.into(),
            action: action.into(),
        }
    }

    #[test]
    fn destroy_removes_and_other_events_refresh_once() {
        let plan = plan_events(&[ev("a", "start"), ev("a", "health_status"), ev("b", "destroy")]);
        assert_eq!(plan.refresh, vec!["a".to_string()]);
        assert_eq!(plan.remove, vec!["b".to_string()]);
        assert_eq!(plan.link, None);
    }

    #[test]
    fn destroy_after_start_cancels_refresh() {
        let plan = plan_events(&[ev("a", "start"), ev("a", "destroy")]);
        assert!(plan.refresh.is_empty());
        assert_eq!(plan.remove, vec!["a".to_string()]);
        // A new container with a reused name gets a new id, but test that a
        // create after a destroy for the same id refreshes it.
        let plan = plan_events(&[ev("a", "destroy"), ev("a", "create")]);
        assert_eq!(plan.refresh, vec!["a".to_string()]);
        assert!(plan.remove.is_empty());
    }

    #[test]
    fn resync_replaces_single_refreshes() {
        let plan = plan_events(&[ev("a", "start"), EngineEvent::Resync]);
        assert!(plan.resync);
        assert!(plan.refresh.is_empty());
        assert_eq!(plan.link, Some(true));
    }

    #[test]
    fn disconnect_then_resync_ends_connected() {
        let plan = plan_events(&[EngineEvent::Disconnected, EngineEvent::Resync]);
        assert_eq!(plan.link, Some(true));
        let plan = plan_events(&[EngineEvent::Resync, ev("a", "stop"), EngineEvent::Disconnected]);
        assert_eq!(plan.link, Some(false));
        assert!(!plan.resync);
    }

    #[test]
    fn old_refresh_result_is_ignored() {
        let mut set = ContainerSet::default();
        let g1 = set.bump("a");
        let g2 = set.bump("a");
        assert!(!set.apply_refresh("a", g1, Some(row("a", "web", None, State::Running))));
        assert!(set.rows.is_empty());
        assert!(set.apply_refresh("a", g2, Some(row("a", "web", None, State::Running))));
        assert_eq!(set.rows["a"].state, State::Running);
        // The same row again is not a change.
        let g3 = set.bump("a");
        assert!(!set.apply_refresh("a", g3, Some(row("a", "web", None, State::Running))));
        // `None` removes the row.
        let g4 = set.bump("a");
        assert!(set.apply_refresh("a", g4, None));
        assert!(set.rows.is_empty());
    }

    #[test]
    fn replace_all_drops_pending_for_missing_rows() {
        let mut set = ContainerSet::default();
        set.pending.insert("gone".into(), Action::Stop);
        set.pending.insert("a".into(), Action::Stop);
        set.replace_all(vec![row("a", "web", None, State::Running)]);
        assert!(set.pending.contains_key("a"));
        assert!(!set.pending.contains_key("gone"));
    }

    #[test]
    fn error_text_drops_http_status() {
        let e = anyhow::anyhow!("Docker responded with status code 400: failed to start");
        assert_eq!(clean_error(&e), "failed to start");
        assert_eq!(clean_error(&anyhow::anyhow!("plain")), "plain");
    }

    #[test]
    fn replace_all_reports_changes() {
        let mut set = ContainerSet::default();
        assert!(set.replace_all(vec![row("a", "web", None, State::Running)]));
        assert!(!set.replace_all(vec![row("a", "web", None, State::Running)]));
        assert!(set.replace_all(vec![row("a", "web", None, State::Exited)]));
    }

    #[test]
    fn compose_grouping() {
        let rows = vec![
            row("1", "zeta", None, State::Running),
            row("2", "shop-web-1", Some("shop"), State::Running),
            row("3", "shop-db-1", Some("shop"), State::Exited),
            row("4", "alpha", None, State::Exited),
            row("5", "api-server-1", Some("api"), State::Running),
        ];
        let list = build_list(rows.iter(), "", &HashSet::new());
        let ids: Vec<String> = list
            .iter()
            .map(|i| match i {
                ListItem::Group { project, .. } => format!("[{project}]"),
                ListItem::Row { id, .. } => id.clone(),
            })
            .collect();
        assert_eq!(ids, ["[api]", "5", "[shop]", "3", "2", "4", "1"]);
        assert_eq!(
            list[2],
            ListItem::Group {
                project: "shop".into(),
                total: 2,
                running: 1,
                collapsed: false
            }
        );

        let collapsed = HashSet::from(["shop".to_string()]);
        let list = build_list(rows.iter(), "", &collapsed);
        assert_eq!(list.len(), 5);

        let list = build_list(rows.iter(), "SHOP-DB", &HashSet::new());
        assert_eq!(list.len(), 2);
        let list = build_list(rows.iter(), "api", &HashSet::new());
        assert_eq!(list.len(), 2);
    }
}

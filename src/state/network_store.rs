//! The list of networks and the containers on them.

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui::{Context, Entity, EventEmitter};
use indexmap::IndexMap;

use crate::docker::ContainerRow;
use crate::docker::EngineStatus;
use crate::docker::networks::{self, NetworkRow};
use crate::runtime::Backend;
use crate::state::container_store::{ContainerStore, StoreEvent, clean_error};

/// The group key of the built-in networks. A Compose project name cannot
/// contain a NUL character, so the key does not match a project.
pub const BUILT_IN_GROUP: &str = "\0built-in";

/// One line of the grouped list.
#[derive(Clone, Debug, PartialEq)]
pub enum NetworkItem {
    Group {
        /// The Compose project, or `BUILT_IN_GROUP`.
        key: String,
        total: usize,
        /// The members that a prune deletes.
        unused: usize,
        collapsed: bool,
    },
    Row {
        name: String,
        grouped: bool,
    },
}

/// The containers on each network, by network name. The list includes the
/// stopped containers.
pub fn network_users<'a>(
    containers: impl Iterator<Item = &'a ContainerRow>,
) -> HashMap<&'a str, Vec<&'a ContainerRow>> {
    let mut users: HashMap<&str, Vec<&ContainerRow>> = HashMap::new();
    for c in containers {
        for n in &c.networks {
            users.entry(n.as_str()).or_default().push(c);
        }
    }
    for list in users.values_mut() {
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list.dedup_by(|a, b| a.id == b.id);
    }
    users
}

/// True if a prune deletes the network: it is not built in, and no running
/// container is on it. A stopped container does not keep its network.
pub fn is_unused(row: &NetworkRow, users: &HashMap<&str, Vec<&ContainerRow>>) -> bool {
    !row.is_built_in()
        && !users
            .get(row.name.as_str())
            .is_some_and(|list| list.iter().any(|c| c.state.is_running()))
}

/// What the list shows.
pub struct ListOptions<'a> {
    /// Matches the name, the project, or the subnet, and does not care
    /// about case.
    pub filter: &'a str,
    /// Show only the networks that a prune deletes.
    pub unused_only: bool,
    pub collapsed: &'a HashSet<String>,
    pub users: &'a HashMap<&'a str, Vec<&'a ContainerRow>>,
}

/// Groups networks by Compose project. The projects come first. Then the
/// networks that have no project, and last the group of built-in networks.
/// Each part is in name order.
pub fn build_list<'a>(rows: impl Iterator<Item = &'a NetworkRow>, options: &ListOptions) -> Vec<NetworkItem> {
    let ListOptions { filter, unused_only, collapsed, users } = *options;
    let filter = filter.trim().to_lowercase();
    let matches = |n: &NetworkRow| {
        (!unused_only || is_unused(n, users))
            && (filter.is_empty()
                || n.name.to_lowercase().contains(&filter)
                || n.compose_project.as_deref().is_some_and(|p| p.to_lowercase().contains(&filter))
                || n.subnets.iter().any(|s| s.contains(&filter)))
    };
    let mut groups: BTreeMap<&str, Vec<&NetworkRow>> = BTreeMap::new();
    let mut single: Vec<&NetworkRow> = Vec::new();
    let mut built_in: Vec<&NetworkRow> = Vec::new();
    for n in rows.filter(|n| matches(n)) {
        match &n.compose_project {
            _ if n.is_built_in() => built_in.push(n),
            Some(p) => groups.entry(p).or_default().push(n),
            None => single.push(n),
        }
    }
    let mut out = Vec::new();
    let push_group = |out: &mut Vec<NetworkItem>, key: &str, mut members: Vec<&NetworkRow>| {
        members.sort_by(|a, b| a.name.cmp(&b.name));
        let is_collapsed = collapsed.contains(key);
        out.push(NetworkItem::Group {
            key: key.to_string(),
            total: members.len(),
            unused: members.iter().filter(|n| is_unused(n, users)).count(),
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            out.extend(members.into_iter().map(|n| NetworkItem::Row {
                name: n.name.clone(),
                grouped: true,
            }));
        }
    };
    for (project, members) in groups {
        push_group(&mut out, project, members);
    }
    single.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(single.into_iter().map(|n| NetworkItem::Row {
        name: n.name.clone(),
        grouped: false,
    }));
    if !built_in.is_empty() {
        push_group(&mut out, BUILT_IN_GROUP, built_in);
    }
    out
}

#[derive(Clone, Debug)]
pub enum NetworkEvent {
    Error(String),
    /// A prune ended. The value is the count of deleted networks.
    Pruned(usize),
    /// A network that the user made now exists.
    Created(String),
}

pub struct NetworkStore {
    containers: Entity<ContainerStore>,
    /// The socket of the engine that the rows come from.
    socket: Option<String>,
    pub rows: IndexMap<String, NetworkRow>,
    /// The networks that a remove runs on now.
    pub removing: HashSet<String>,
    pub pruning: bool,
    /// The store loads nothing until the networks section shows one time.
    active: bool,
    /// True when the rows are from the current engine.
    loaded: bool,
    /// Increments on each list that arrives. The detail pane reads the
    /// inspect data again when it changes, because a container that joins
    /// or leaves a network changes only the inspect data.
    pub revision: u64,
    /// Increments on each list load and on each engine change. A result
    /// with an old value is out of date, so the store ignores it.
    list_gen: u64,
    _subs: Vec<gpui::Subscription>,
}

impl EventEmitter<NetworkEvent> for NetworkStore {}

impl NetworkStore {
    pub fn new(containers: Entity<ContainerStore>, cx: &mut Context<Self>) -> Self {
        let subs = vec![cx.subscribe(&containers, |this, _, ev: &StoreEvent, cx| match ev {
            StoreEvent::Changed => this.sync_engine(cx),
            StoreEvent::NetworksChanged => {
                if this.active {
                    this.reload(cx);
                }
            }
            StoreEvent::VolumesChanged | StoreEvent::ImagesChanged | StoreEvent::Error(_) | StoreEvent::Dns { .. } | StoreEvent::Compose(_) => {}
        })];
        NetworkStore {
            containers,
            socket: None,
            rows: IndexMap::new(),
            removing: HashSet::new(),
            pruning: false,
            active: false,
            loaded: false,
            revision: 0,
            list_gen: 0,
            _subs: subs,
        }
    }

    /// True until the first list of the current engine arrives.
    pub fn is_loading(&self) -> bool {
        !self.loaded
    }

    /// Starts the loads when the networks section shows. After the first
    /// list, the engine events keep the list current.
    pub fn activate(&mut self, cx: &mut Context<Self>) {
        self.active = true;
        if !self.loaded {
            self.reload(cx);
        }
    }

    /// Drops the rows of an old engine, and loads the rows of a new one.
    fn sync_engine(&mut self, cx: &mut Context<Self>) {
        let store = self.containers.read(cx);
        let socket = (store.status == EngineStatus::Connected)
            .then(|| store.socket().map(str::to_string))
            .flatten();
        if socket == self.socket {
            return;
        }
        self.socket = socket;
        self.list_gen += 1;
        self.rows.clear();
        self.removing.clear();
        self.pruning = false;
        self.loaded = false;
        if self.active && self.socket.is_some() {
            self.reload(cx);
        }
        cx.notify();
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        self.list_gen += 1;
        let generation = self.list_gen;
        let rx = Backend::global(cx).spawn(async move { networks::list_all(&docker).await });
        cx.spawn(async move |this, cx| {
            let Ok(result) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.list_gen != generation {
                    return;
                }
                // Show the empty list on an error, not a spinner that never
                // ends.
                this.loaded = true;
                match result {
                    Ok(rows) => {
                        this.rows = rows.into_iter().map(|r| (r.name.clone(), r)).collect();
                        let rows = &this.rows;
                        this.removing.retain(|n| rows.contains_key(n));
                        this.revision += 1;
                    }
                    Err(e) => {
                        cx.emit(NetworkEvent::Error(format!("Could not list networks: {}", clean_error(&e))))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Deletes a network. Docker refuses to delete a network that a running
    /// container is on.
    pub fn remove(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        if !self.removing.insert(name.to_string()) {
            return;
        }
        cx.notify();
        let name = name.to_string();
        let name2 = name.clone();
        let rx = Backend::global(cx).spawn(async move { networks::remove(&docker, &name2).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("cancelled")));
            let _ = this.update(cx, |this, cx| {
                this.removing.remove(&name);
                match result {
                    // The destroy event also reloads the list. Remove the row
                    // now, so the list does not wait for the event.
                    Ok(()) => {
                        this.rows.shift_remove(&name);
                    }
                    Err(e) => cx.emit(NetworkEvent::Error(format!(
                        "Could not remove {name}: {}",
                        clean_error(&e)
                    ))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Deletes some networks, one call each. A network that fails gives its
    /// own error, and the others continue.
    pub fn remove_many(&mut self, names: &[String], cx: &mut Context<Self>) {
        for name in names {
            self.remove(name, cx);
        }
    }

    /// Makes a bridge network. The name must pass `docker::check_name`.
    pub fn create(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        let name = name.to_string();
        let name2 = name.clone();
        let rx = Backend::global(cx).spawn(async move {
            networks::create(&docker, &name2).await?;
            // The create call gives only the ID. Read the row, so the list
            // can show and select it before the event comes.
            networks::list_all(&docker).await
        });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("cancelled")));
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(rows) => {
                        if let Some(row) = rows.into_iter().find(|r| r.name == name) {
                            this.rows.insert(row.name.clone(), row);
                        }
                        cx.emit(NetworkEvent::Created(name));
                    }
                    Err(e) => cx.emit(NetworkEvent::Error(format!(
                        "Could not make the network {name}: {}",
                        clean_error(&e)
                    ))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Deletes all the networks that no running container is on.
    pub fn prune(&mut self, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        if self.pruning {
            return;
        }
        self.pruning = true;
        cx.notify();
        let rx = Backend::global(cx).spawn(async move { networks::prune(&docker).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("cancelled")));
            let _ = this.update(cx, |this, cx| {
                this.pruning = false;
                match result {
                    Ok(count) => cx.emit(NetworkEvent::Pruned(count)),
                    Err(e) => cx.emit(NetworkEvent::Error(format!(
                        "Could not remove the unused networks: {}",
                        clean_error(&e)
                    ))),
                }
                this.reload(cx);
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::containers::State;

    fn network(name: &str, project: Option<&str>) -> NetworkRow {
        NetworkRow {
            id: name.into(),
            name: name.into(),
            driver: "bridge".into(),
            subnets: vec![format!("172.{}.0.0/16", name.len())],
            gateways: Vec::new(),
            internal: false,
            ipv6: false,
            compose_project: project.map(Into::into),
            compose_network: None,
        }
    }

    fn container(id: &str, state: State, networks: &[&str]) -> ContainerRow {
        ContainerRow {
            id: id.into(),
            name: id.into(),
            image: String::new(),
            image_id: String::new(),
            state,
            status: String::new(),
            ports: Vec::new(),
            compose_project: None,
            compose_service: None,
            compose_file: None,
            icon: None,
            created: 0,
            finished_at: None,
            volumes: Vec::new(),
            networks: networks.iter().map(|n| n.to_string()).collect(),
        }
    }

    #[test]
    fn users_come_from_the_container_networks() {
        let containers = [
            container("web", State::Running, &["shop_default"]),
            container("db", State::Exited, &["shop_default", "backend"]),
        ];
        let users = network_users(containers.iter());
        let names = |n: &str| users[n].iter().map(|c| c.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names("shop_default"), ["db", "web"]);
        assert_eq!(names("backend"), ["db"]);
        // A stopped container does not keep its network from a prune.
        assert!(!is_unused(&network("shop_default", None), &users));
        assert!(is_unused(&network("backend", None), &users));
        assert!(is_unused(&network("cache", None), &users));
        assert!(!is_unused(&network("bridge", None), &users));
    }

    #[test]
    fn grouping_puts_projects_first_and_built_in_last() {
        let rows = [
            network("zeta", None),
            network("bridge", None),
            network("shop_default", Some("shop")),
            network("host", None),
            network("shop_backend", Some("shop")),
            network("alpha", None),
        ];
        let containers = [container("web", State::Running, &["shop_default"])];
        let users = network_users(containers.iter());
        let collapsed = HashSet::new();
        let options = |filter, unused_only| ListOptions {
            filter,
            unused_only,
            collapsed: &collapsed,
            users: &users,
        };
        let keys = |list: Vec<NetworkItem>| -> Vec<String> {
            list.into_iter()
                .map(|i| match i {
                    NetworkItem::Group { key, .. } => format!("[{key}]"),
                    NetworkItem::Row { name, .. } => name,
                })
                .collect()
        };
        let list = build_list(rows.iter(), &options("", false));
        assert_eq!(
            list[0],
            NetworkItem::Group { key: "shop".into(), total: 2, unused: 1, collapsed: false }
        );
        assert_eq!(
            keys(list),
            ["[shop]", "shop_backend", "shop_default", "alpha", "zeta", "[\0built-in]", "bridge", "host"]
        );
        assert_eq!(keys(build_list(rows.iter(), &options("SHOP", false))).len(), 3);
        // The search also matches a subnet.
        assert_eq!(keys(build_list(rows.iter(), &options("172.5.", false))), ["alpha"]);
        assert_eq!(
            keys(build_list(rows.iter(), &options("", true))),
            ["[shop]", "shop_backend", "alpha", "zeta"]
        );
    }
}

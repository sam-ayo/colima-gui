//! The list of volumes, their sizes, and the containers that use them.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use gpui::{Context, Entity, EventEmitter, Task};
use indexmap::IndexMap;

use crate::docker::ContainerRow;
use crate::docker::volumes::{self, VolumeRow};
use crate::docker::EngineStatus;
use crate::runtime::Backend;
use crate::state::container_store::{ContainerStore, StoreEvent, clean_error};

/// The group key of the anonymous volumes. A Compose project name cannot
/// contain a NUL character, so the key does not match a project.
pub const ANONYMOUS_GROUP: &str = "\0anonymous";

/// One line of the grouped list.
#[derive(Clone, Debug, PartialEq)]
pub enum VolumeItem {
    Group {
        /// The Compose project, or `ANONYMOUS_GROUP`.
        key: String,
        total: usize,
        /// The members that no container uses.
        unused: usize,
        collapsed: bool,
    },
    Row {
        name: String,
        grouped: bool,
    },
}

/// The containers that mount each volume, by volume name.
pub fn volume_users<'a>(
    containers: impl Iterator<Item = &'a ContainerRow>,
) -> HashMap<&'a str, Vec<&'a ContainerRow>> {
    let mut users: HashMap<&str, Vec<&ContainerRow>> = HashMap::new();
    for c in containers {
        for v in &c.volumes {
            users.entry(v.as_str()).or_default().push(c);
        }
    }
    for list in users.values_mut() {
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list.dedup_by(|a, b| a.id == b.id);
    }
    users
}

/// The order of the rows and the groups.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VolumeSort {
    #[default]
    Name,
    /// The largest first. A volume with no size yet comes last.
    Size,
}

/// What the list shows, and in which order.
pub struct ListOptions<'a> {
    /// Matches the name or the project, and does not care about case.
    pub filter: &'a str,
    /// Show only the volumes that no container uses.
    pub unused_only: bool,
    pub sort: VolumeSort,
    pub collapsed: &'a HashSet<String>,
    pub users: &'a HashMap<&'a str, Vec<&'a ContainerRow>>,
    pub sizes: &'a HashMap<String, u64>,
}

/// Groups volumes by Compose project. The projects come first. Then the
/// named volumes that have no project, and last the group of anonymous
/// volumes. In each part, `options.sort` gives the order.
pub fn build_list<'a>(rows: impl Iterator<Item = &'a VolumeRow>, options: &ListOptions) -> Vec<VolumeItem> {
    let ListOptions { filter, unused_only, sort, collapsed, users, sizes } = *options;
    let filter = filter.trim().to_lowercase();
    let matches = |v: &VolumeRow| {
        (!unused_only || !users.contains_key(v.name.as_str()))
            && (filter.is_empty()
                || v.name.to_lowercase().contains(&filter)
                || v.compose_project.as_deref().is_some_and(|p| p.to_lowercase().contains(&filter)))
    };
    // By size, the largest first, and by name when the sizes are equal.
    let order = |list: &mut Vec<&VolumeRow>| match sort {
        VolumeSort::Name => list.sort_by(|a, b| a.name.cmp(&b.name)),
        VolumeSort::Size => list.sort_by(|a, b| {
            let size = |v: &VolumeRow| sizes.get(&v.name).map_or(-1, |&s| s as i128);
            size(b).cmp(&size(a)).then_with(|| a.name.cmp(&b.name))
        }),
    };
    let mut groups: BTreeMap<&str, Vec<&VolumeRow>> = BTreeMap::new();
    let mut single: Vec<&VolumeRow> = Vec::new();
    let mut anonymous: Vec<&VolumeRow> = Vec::new();
    for v in rows.filter(|v| matches(v)) {
        match (&v.compose_project, v.anonymous) {
            (Some(p), _) => groups.entry(p).or_default().push(v),
            (None, true) => anonymous.push(v),
            (None, false) => single.push(v),
        }
    }
    let mut out = Vec::new();
    let push_group = |out: &mut Vec<VolumeItem>, key: &str, mut members: Vec<&VolumeRow>| {
        order(&mut members);
        let is_collapsed = collapsed.contains(key);
        out.push(VolumeItem::Group {
            key: key.to_string(),
            total: members.len(),
            unused: members.iter().filter(|v| !users.contains_key(v.name.as_str())).count(),
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            out.extend(members.into_iter().map(|v| VolumeItem::Row {
                name: v.name.clone(),
                grouped: true,
            }));
        }
    };
    let mut groups: Vec<(&str, Vec<&VolumeRow>)> = groups.into_iter().collect();
    if sort == VolumeSort::Size {
        let total = |members: &[&VolumeRow]| members.iter().filter_map(|v| sizes.get(&v.name)).sum::<u64>();
        // The sort is stable, so the groups of equal size keep the name order.
        groups.sort_by_key(|(_, members)| std::cmp::Reverse(total(members)));
    }
    for (project, members) in groups {
        push_group(&mut out, project, members);
    }
    order(&mut single);
    out.extend(single.into_iter().map(|v| VolumeItem::Row {
        name: v.name.clone(),
        grouped: false,
    }));
    if !anonymous.is_empty() {
        push_group(&mut out, ANONYMOUS_GROUP, anonymous);
    }
    out
}

#[derive(Clone, Debug)]
pub enum VolumeEvent {
    Error(String),
    /// A prune ended. The values are the count of deleted volumes and the
    /// bytes that came free.
    Pruned(usize, u64),
    /// A volume that the user made now exists.
    Created(String),
}

/// The time between two size loads while the volumes section shows. The
/// engine measures each volume, so do not load more frequently.
const SIZES_REFRESH: Duration = Duration::from_secs(60);

/// The rows and the sizes of one engine.
type SavedVolumes = (IndexMap<String, VolumeRow>, HashMap<String, u64>);

pub struct VolumeStore {
    containers: Entity<ContainerStore>,
    /// The socket of the engine that the rows come from.
    socket: Option<String>,
    pub rows: IndexMap<String, VolumeRow>,
    /// The disk use of each volume, in bytes. It arrives after the list,
    /// because the engine must measure each volume.
    pub sizes: HashMap<String, u64>,
    /// The volumes that a remove runs on now.
    pub removing: HashSet<String>,
    pub pruning: bool,
    /// The store loads nothing until the volumes section shows one time.
    active: bool,
    /// Reads the sizes again while the volumes section shows. No event
    /// tells when a size changes.
    sizes_task: Option<Task<()>>,
    /// True when the rows are from the current engine.
    loaded: bool,
    sizing: bool,
    /// A size load that is necessary after the current one ends.
    sizes_stale: bool,
    /// True after a list load from the current connection starts. Saved
    /// rows are not fresh.
    fresh: bool,
    /// The last rows and sizes of each engine. A switch shows them at once.
    saved: HashMap<String, SavedVolumes>,
    /// Increments on each list load and on each engine change. A result
    /// with an old value is out of date, so the store ignores it.
    list_gen: u64,
    _subs: Vec<gpui::Subscription>,
}

impl EventEmitter<VolumeEvent> for VolumeStore {}

impl VolumeStore {
    pub fn new(containers: Entity<ContainerStore>, cx: &mut Context<Self>) -> Self {
        let subs = vec![cx.subscribe(&containers, |this, _, ev: &StoreEvent, cx| match ev {
            StoreEvent::Changed => this.sync_engine(cx),
            StoreEvent::VolumesChanged => {
                if this.active {
                    this.reload(cx);
                }
            }
            StoreEvent::ImagesChanged | StoreEvent::NetworksChanged | StoreEvent::Error(_) | StoreEvent::Dns { .. } | StoreEvent::Compose(_) => {}
        })];
        VolumeStore {
            containers,
            socket: None,
            rows: IndexMap::new(),
            sizes: HashMap::new(),
            removing: HashSet::new(),
            pruning: false,
            active: false,
            sizes_task: None,
            loaded: false,
            sizing: false,
            sizes_stale: false,
            fresh: false,
            saved: HashMap::new(),
            list_gen: 0,
            _subs: subs,
        }
    }

    /// True until the first list of the current engine arrives.
    pub fn is_loading(&self) -> bool {
        !self.loaded
    }

    pub fn is_sizing(&self) -> bool {
        self.sizing
    }

    /// Starts the loads when the volumes section shows. Each show reads
    /// the sizes again, and they update each `SIZES_REFRESH` while the
    /// section shows.
    pub fn activate(&mut self, cx: &mut Context<Self>) {
        self.active = true;
        if self.loaded && self.fresh {
            self.load_sizes(cx);
        } else {
            self.reload(cx);
        }
        self.sizes_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SIZES_REFRESH).await;
                if this.update(cx, |this, cx| this.load_sizes(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Stops the size updates when the volumes section goes away. The list
    /// stays current from the engine events.
    pub fn deactivate(&mut self) {
        self.sizes_task = None;
    }

    /// Saves the rows of an old engine, and shows the saved rows of a new
    /// one. Loads the rows when the new engine connects.
    fn sync_engine(&mut self, cx: &mut Context<Self>) {
        let store = self.containers.read(cx);
        let connected = store.status == EngineStatus::Connected;
        let socket = (store.shown_status() == EngineStatus::Connected)
            .then(|| store.socket().map(str::to_string))
            .flatten();
        if socket != self.socket {
            let rows = std::mem::take(&mut self.rows);
            let sizes = std::mem::take(&mut self.sizes);
            if self.loaded
                && let Some(old) = self.socket.take()
            {
                self.saved.insert(old, (rows, sizes));
            }
            self.socket = socket;
            self.list_gen += 1;
            self.removing.clear();
            self.pruning = false;
            self.loaded = false;
            self.fresh = false;
            self.sizing = false;
            self.sizes_stale = false;
            if let Some((rows, sizes)) = self.socket.as_ref().and_then(|s| self.saved.get(s)) {
                self.rows = rows.clone();
                self.sizes = sizes.clone();
                self.loaded = true;
            }
            cx.notify();
        }
        if connected && self.active && !self.fresh && self.socket.is_some() {
            self.reload(cx);
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        self.fresh = true;
        self.list_gen += 1;
        let generation = self.list_gen;
        let rx = Backend::global(cx).spawn(async move { volumes::list_all(&docker).await });
        cx.spawn(async move |this, cx| {
            let Ok(result) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.list_gen != generation {
                    return;
                }
                match result {
                    Ok(rows) => {
                        this.rows = rows.into_iter().map(|r| (r.name.clone(), r)).collect();
                        let rows = &this.rows;
                        this.removing.retain(|n| rows.contains_key(n));
                        this.sizes.retain(|n, _| rows.contains_key(n));
                        this.loaded = true;
                        this.load_sizes(cx);
                    }
                    Err(e) => {
                        // Show the empty list, not a spinner that never ends.
                        this.loaded = true;
                        cx.emit(VolumeEvent::Error(format!("Could not list volumes: {}", clean_error(&e))));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads the sizes. Only one load runs at a time. A request during a
    /// load starts one more load after it.
    fn load_sizes(&mut self, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        if self.sizing {
            self.sizes_stale = true;
            return;
        }
        self.sizing = true;
        self.sizes_stale = false;
        let socket = self.socket.clone();
        let rx = Backend::global(cx).spawn(async move { volumes::sizes(&docker).await });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let Ok(result) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.socket != socket {
                    return;
                }
                this.sizing = false;
                match result {
                    Ok(sizes) => this.sizes = sizes,
                    // The sizes are extra data. The list works without them.
                    Err(e) => tracing::warn!("volume sizes: {e}"),
                }
                if this.sizes_stale {
                    this.load_sizes(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Deletes a volume. Docker refuses to delete a volume that a
    /// container uses, also a stopped container.
    pub fn remove(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        if !self.removing.insert(name.to_string()) {
            return;
        }
        cx.notify();
        let name = name.to_string();
        let name2 = name.clone();
        let rx = Backend::global(cx).spawn(async move { volumes::remove(&docker, &name2).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("cancelled")));
            let _ = this.update(cx, |this, cx| {
                this.removing.remove(&name);
                match result {
                    // The destroy event also reloads the list. Remove the row
                    // now, so the list does not wait for the event.
                    Ok(()) => {
                        this.rows.shift_remove(&name);
                        this.sizes.remove(&name);
                    }
                    Err(e) => cx.emit(VolumeEvent::Error(format!(
                        "Could not remove {name}: {}",
                        clean_error(&e)
                    ))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Makes a volume. The name must pass `docker::check_name`.
    pub fn create(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        let name = name.to_string();
        let name2 = name.clone();
        let rx = Backend::global(cx).spawn(async move { volumes::create(&docker, &name2).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("cancelled")));
            let _ = this.update(cx, |this, cx| {
                match result {
                    // The create event also reloads the list. Add the row
                    // now, so the list can select it.
                    Ok(row) => {
                        this.rows.insert(row.name.clone(), row);
                        this.sizes.insert(name.clone(), 0);
                        cx.emit(VolumeEvent::Created(name));
                    }
                    Err(e) => cx.emit(VolumeEvent::Error(format!(
                        "Could not make the volume {name}: {}",
                        clean_error(&e)
                    ))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Deletes some volumes, one call each. A volume that fails gives its
    /// own error, and the others continue.
    pub fn remove_many(&mut self, names: &[String], cx: &mut Context<Self>) {
        for name in names {
            self.remove(name, cx);
        }
    }

    /// Deletes all the volumes that no container uses.
    pub fn prune(&mut self, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        if self.pruning {
            return;
        }
        self.pruning = true;
        cx.notify();
        let rx = Backend::global(cx).spawn(async move { volumes::prune(&docker).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("cancelled")));
            let _ = this.update(cx, |this, cx| {
                this.pruning = false;
                match result {
                    Ok(p) => cx.emit(VolumeEvent::Pruned(p.count, p.bytes)),
                    Err(e) => cx.emit(VolumeEvent::Error(format!(
                        "Could not remove the unused volumes: {}",
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

    fn volume(name: &str, project: Option<&str>, anonymous: bool) -> VolumeRow {
        VolumeRow {
            name: name.into(),
            driver: "local".into(),
            mountpoint: String::new(),
            compose_project: project.map(Into::into),
            compose_volume: None,
            anonymous,
        }
    }

    fn container(id: &str, volumes: &[&str]) -> ContainerRow {
        ContainerRow {
            id: id.into(),
            name: id.into(),
            image: String::new(),
            image_id: String::new(),
            state: State::Running,
            status: String::new(),
            ports: Vec::new(),
            compose_project: None,
            compose_service: None,
            compose_file: None,
            icon: None,
            created: 0,
            finished_at: None,
            volumes: volumes.iter().map(|v| v.to_string()).collect(),
            networks: Vec::new(),
        }
    }

    #[test]
    fn users_come_from_the_container_mounts() {
        let containers = [container("web", &["data"]), container("db", &["data", "logs"])];
        let users = volume_users(containers.iter());
        let names = |v: &str| users[v].iter().map(|c| c.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names("data"), ["db", "web"]);
        assert_eq!(names("logs"), ["db"]);
        assert!(!users.contains_key("cache"));
    }

    #[test]
    fn grouping_puts_projects_first_and_anonymous_last() {
        let rows = [
            volume("zeta", None, false),
            volume("0ad985de", None, true),
            volume("shop_db", Some("shop"), false),
            volume("shop_cache", Some("shop"), false),
            volume("alpha", None, false),
        ];
        let containers = [container("db", &["shop_db"])];
        let users = volume_users(containers.iter());
        let collapsed = HashSet::from([ANONYMOUS_GROUP.to_string()]);
        let sizes = HashMap::new();
        let options = |filter| ListOptions {
            filter,
            unused_only: false,
            sort: VolumeSort::Name,
            collapsed: &collapsed,
            users: &users,
            sizes: &sizes,
        };
        let list = build_list(rows.iter(), &options(""));
        let keys: Vec<String> = list
            .iter()
            .map(|i| match i {
                VolumeItem::Group { key, .. } => format!("[{key}]"),
                VolumeItem::Row { name, .. } => name.clone(),
            })
            .collect();
        assert_eq!(keys, ["[shop]", "shop_cache", "shop_db", "alpha", "zeta", "[\0anonymous]"]);
        assert_eq!(
            list[0],
            VolumeItem::Group { key: "shop".into(), total: 2, unused: 1, collapsed: false }
        );
        assert_eq!(build_list(rows.iter(), &options("SHOP")).len(), 3);
        assert_eq!(build_list(rows.iter(), &options("alp")).len(), 1);
    }

    #[test]
    fn unused_filter_and_size_sort() {
        let rows = [
            volume("a_small", Some("a"), false),
            volume("b_big", Some("b"), false),
            volume("b_used", Some("b"), false),
            volume("cache", None, false),
            volume("logs", None, false),
            volume("new", None, false),
        ];
        let containers = [container("web", &["b_used"])];
        let users = volume_users(containers.iter());
        let sizes = HashMap::from([
            ("a_small".to_string(), 10),
            ("b_big".to_string(), 500),
            ("b_used".to_string(), 1),
            ("cache".to_string(), 5),
            ("logs".to_string(), 50),
        ]);
        let collapsed = HashSet::new();
        let options = ListOptions {
            filter: "",
            unused_only: false,
            sort: VolumeSort::Size,
            collapsed: &collapsed,
            users: &users,
            sizes: &sizes,
        };
        let names = |list: Vec<VolumeItem>| -> Vec<String> {
            list.into_iter()
                .map(|i| match i {
                    VolumeItem::Group { key, .. } => format!("[{key}]"),
                    VolumeItem::Row { name, .. } => name,
                })
                .collect()
        };
        // The larger group first. A volume with no size comes last.
        assert_eq!(
            names(build_list(rows.iter(), &options)),
            ["[b]", "b_big", "b_used", "[a]", "a_small", "logs", "cache", "new"]
        );
        let unused = ListOptions { unused_only: true, ..options };
        assert_eq!(
            names(build_list(rows.iter(), &unused)),
            ["[b]", "b_big", "[a]", "a_small", "logs", "cache", "new"]
        );
    }
}

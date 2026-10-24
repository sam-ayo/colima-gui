//! The list of images, the pulls, and the containers that use each image.

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui::{Context, Entity, EventEmitter, Task};
use indexmap::IndexMap;

use crate::docker::ContainerRow;
use crate::docker::EngineStatus;
use crate::docker::images::{self, ImageRow, PullProgress, split_reference};
use crate::runtime::{Backend, recv_batch};
use crate::state::container_store::{ContainerStore, StoreEvent, clean_error};

/// The group key of the untagged images. A repository name cannot contain
/// a NUL character, so the key does not match a repository.
pub const UNTAGGED_GROUP: &str = "\0untagged";

/// One line of the grouped list.
#[derive(Clone, Debug, PartialEq)]
pub enum ImageItem {
    /// A pull that runs now.
    Pull { reference: String },
    Group {
        /// The repository, or `UNTAGGED_GROUP`.
        key: String,
        total: usize,
        /// The members that no container uses.
        unused: usize,
        collapsed: bool,
    },
    Row {
        /// A reference such as "redis:7-alpine", or the ID of an untagged
        /// image.
        key: String,
        grouped: bool,
    },
}

/// The containers that use each image, by image ID.
pub fn image_users<'a>(
    containers: impl Iterator<Item = &'a ContainerRow>,
) -> HashMap<&'a str, Vec<&'a ContainerRow>> {
    let mut users: HashMap<&str, Vec<&ContainerRow>> = HashMap::new();
    for c in containers.filter(|c| !c.image_id.is_empty()) {
        users.entry(c.image_id.as_str()).or_default().push(c);
    }
    for list in users.values_mut() {
        list.sort_by(|a, b| a.name.cmp(&b.name));
    }
    users
}

/// The order of the rows and the groups.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImageSort {
    #[default]
    Name,
    /// The largest first.
    Size,
}

/// What the list shows, and in which order.
pub struct ListOptions<'a> {
    /// Matches the reference or the ID, and does not care about case.
    pub filter: &'a str,
    /// Show only the images that no container uses.
    pub unused_only: bool,
    pub sort: ImageSort,
    pub collapsed: &'a HashSet<String>,
    pub users: &'a HashMap<&'a str, Vec<&'a ContainerRow>>,
}

/// One tag of an image, or an untagged image.
struct Entry<'a> {
    key: &'a str,
    image: &'a ImageRow,
}

/// A part of the list: a repository with two or more tags, or one row.
struct Block<'a> {
    name: &'a str,
    members: Vec<Entry<'a>>,
}

impl Block<'_> {
    /// The disk use of the images of the block. Two tags of one image
    /// count one time.
    fn size(&self) -> u64 {
        let mut seen = HashSet::new();
        self.members.iter().filter(|e| seen.insert(&e.image.id)).map(|e| e.image.size).sum()
    }
}

/// Makes the list. The pulls come first. Then the repositories and the
/// single rows, by name or by size. A repository with two or more tags is a
/// group. The group of untagged images comes last.
pub fn build_list<'a>(
    rows: impl Iterator<Item = &'a ImageRow>,
    pulls: impl Iterator<Item = &'a String>,
    options: &ListOptions,
) -> Vec<ImageItem> {
    let ListOptions { filter, unused_only, sort, collapsed, users } = *options;
    let filter = filter.trim().to_lowercase();
    let mut repos: BTreeMap<&str, Vec<Entry>> = BTreeMap::new();
    let mut untagged: Vec<Entry> = Vec::new();
    for image in rows {
        if unused_only && users.contains_key(image.id.as_str()) {
            continue;
        }
        let id_match = !filter.is_empty() && image.short_id().starts_with(&filter);
        let matches = |key: &str| filter.is_empty() || id_match || key.to_lowercase().contains(&filter);
        if image.tags.is_empty() {
            if matches("") {
                untagged.push(Entry { key: &image.id, image });
            }
            continue;
        }
        for tag in image.tags.iter().filter(|t| matches(t)) {
            repos.entry(split_reference(tag).0).or_default().push(Entry { key: tag, image });
        }
    }
    let order = |list: &mut Vec<Entry>| match sort {
        ImageSort::Name => list.sort_by(|a, b| a.key.cmp(b.key)),
        ImageSort::Size => list.sort_by(|a, b| b.image.size.cmp(&a.image.size).then_with(|| a.key.cmp(b.key))),
    };
    let mut blocks: Vec<Block> = repos.into_iter().map(|(name, members)| Block { name, members }).collect();
    if sort == ImageSort::Size {
        // The sort is stable, so the blocks of equal size keep the name order.
        blocks.sort_by_key(|b| std::cmp::Reverse(b.size()));
    }
    let mut out: Vec<ImageItem> = pulls.map(|r| ImageItem::Pull { reference: r.clone() }).collect();
    let push_group = |out: &mut Vec<ImageItem>, key: &str, mut members: Vec<Entry>| {
        order(&mut members);
        let is_collapsed = collapsed.contains(key);
        out.push(ImageItem::Group {
            key: key.to_string(),
            total: members.len(),
            unused: members.iter().filter(|e| !users.contains_key(e.image.id.as_str())).count(),
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            out.extend(members.into_iter().map(|e| ImageItem::Row { key: e.key.to_string(), grouped: true }));
        }
    };
    for block in blocks {
        if block.members.len() > 1 {
            push_group(&mut out, block.name, block.members);
        } else {
            out.extend(block.members.into_iter().map(|e| ImageItem::Row { key: e.key.to_string(), grouped: false }));
        }
    }
    if !untagged.is_empty() {
        push_group(&mut out, UNTAGGED_GROUP, untagged);
    }
    out
}

#[derive(Clone, Debug)]
pub enum ImageEvent {
    Error(String),
    /// A prune ended. The values are the count of deleted images and the
    /// bytes that came free.
    Pruned(usize, u64),
    /// A pull ended, and the list has the image.
    Pulled(String),
}

/// A pull that runs now.
pub struct Pull {
    pub progress: PullProgress,
    /// The task reads the progress. A drop stops the pull.
    task: Task<()>,
}

/// The rows and the references of one engine.
type SavedImages = (IndexMap<String, ImageRow>, HashMap<String, String>);

pub struct ImageStore {
    containers: Entity<ContainerStore>,
    /// The socket of the engine that the rows come from.
    socket: Option<String>,
    /// The images, by ID.
    pub rows: IndexMap<String, ImageRow>,
    /// The ID of the image of each reference.
    refs: HashMap<String, String>,
    /// The keys that a remove runs on now.
    pub removing: HashSet<String>,
    pub pruning: bool,
    /// The pulls, by reference, in the order that they started.
    pub pulls: IndexMap<String, Pull>,
    /// The store loads nothing until the images section shows one time.
    active: bool,
    /// True when the rows are from the current engine.
    loaded: bool,
    /// True after a list load from the current connection starts. Saved
    /// rows are not fresh.
    fresh: bool,
    /// The last rows and references of each engine. A switch shows them
    /// at once.
    saved: HashMap<String, SavedImages>,
    /// The pulls that ended. The store tells about them when the next
    /// list arrives, so the list can select the image.
    pulled: Vec<String>,
    /// Increments on each list load and on each engine change. A result
    /// with an old value is out of date, so the store ignores it.
    list_gen: u64,
    _subs: Vec<gpui::Subscription>,
}

impl EventEmitter<ImageEvent> for ImageStore {}

impl ImageStore {
    pub fn new(containers: Entity<ContainerStore>, cx: &mut Context<Self>) -> Self {
        let subs = vec![cx.subscribe(&containers, |this, _, ev: &StoreEvent, cx| match ev {
            StoreEvent::Changed => this.sync_engine(cx),
            StoreEvent::ImagesChanged => {
                if this.active {
                    this.reload(cx);
                }
            }
            StoreEvent::VolumesChanged | StoreEvent::NetworksChanged | StoreEvent::Error(_) | StoreEvent::Dns { .. } | StoreEvent::Compose(_) => {}
        })];
        ImageStore {
            containers,
            socket: None,
            rows: IndexMap::new(),
            refs: HashMap::new(),
            removing: HashSet::new(),
            pruning: false,
            pulls: IndexMap::new(),
            active: false,
            loaded: false,
            fresh: false,
            saved: HashMap::new(),
            pulled: Vec::new(),
            list_gen: 0,
            _subs: subs,
        }
    }

    /// True until the first list of the current engine arrives.
    pub fn is_loading(&self) -> bool {
        !self.loaded
    }

    /// Finds the image of a list key: a reference or an image ID.
    pub fn image(&self, key: &str) -> Option<&ImageRow> {
        self.rows.get(key).or_else(|| self.rows.get(self.refs.get(key)?))
    }

    /// Starts the first load when the images section shows. The engine
    /// events then keep the list current.
    pub fn activate(&mut self, cx: &mut Context<Self>) {
        if !self.active {
            self.active = true;
            self.reload(cx);
        }
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
            let refs = std::mem::take(&mut self.refs);
            if self.loaded
                && let Some(old) = self.socket.take()
            {
                self.saved.insert(old, (rows, refs));
            }
            self.socket = socket;
            self.list_gen += 1;
            self.removing.clear();
            self.pulls.clear();
            self.pulled.clear();
            self.pruning = false;
            self.loaded = false;
            self.fresh = false;
            if let Some((rows, refs)) = self.socket.as_ref().and_then(|s| self.saved.get(s)) {
                self.rows = rows.clone();
                self.refs = refs.clone();
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
        let rx = Backend::global(cx).spawn(async move { images::list_all(&docker).await });
        cx.spawn(async move |this, cx| {
            let Ok(result) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.list_gen != generation {
                    return;
                }
                match result {
                    Ok(rows) => {
                        this.refs = rows
                            .iter()
                            .flat_map(|r| r.tags.iter().map(|t| (t.clone(), r.id.clone())))
                            .collect();
                        this.rows = rows.into_iter().map(|r| (r.id.clone(), r)).collect();
                        let removing = std::mem::take(&mut this.removing);
                        this.removing = removing.into_iter().filter(|k| this.image(k).is_some()).collect();
                        this.loaded = true;
                        for reference in std::mem::take(&mut this.pulled) {
                            cx.emit(ImageEvent::Pulled(reference));
                        }
                    }
                    Err(e) => {
                        // Show the empty list, not a spinner that never ends.
                        this.loaded = true;
                        cx.emit(ImageEvent::Error(format!("Could not list images: {}", clean_error(&e))));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Removes a tag, or an untagged image by its ID. Docker deletes the
    /// image when its last tag goes. Docker refuses to delete an image that
    /// a container uses, also a stopped container.
    pub fn remove(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        if !self.removing.insert(key.to_string()) {
            return;
        }
        cx.notify();
        let key = key.to_string();
        let key2 = key.clone();
        let rx = Backend::global(cx).spawn(async move { images::remove(&docker, &key2).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("cancelled")));
            let _ = this.update(cx, |this, cx| {
                this.removing.remove(&key);
                match result {
                    // The delete event also reloads the list. Load it now,
                    // so the list does not wait for the event.
                    Ok(()) => this.reload(cx),
                    Err(e) => {
                        let shown = if key.starts_with("sha256:") { images::short_id(&key) } else { &key };
                        cx.emit(ImageEvent::Error(format!("Could not remove {shown}: {}", clean_error(&e))))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Removes some keys, one call each. A key that fails gives its own
    /// error, and the others continue.
    pub fn remove_many(&mut self, keys: &[String], cx: &mut Context<Self>) {
        for key in keys {
            self.remove(key, cx);
        }
    }

    /// Deletes all the images that no container uses.
    pub fn prune(&mut self, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        if self.pruning {
            return;
        }
        self.pruning = true;
        cx.notify();
        let rx = Backend::global(cx).spawn(async move { images::prune(&docker).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("cancelled")));
            let _ = this.update(cx, |this, cx| {
                this.pruning = false;
                match result {
                    Ok(p) => cx.emit(ImageEvent::Pruned(p.count, p.bytes)),
                    Err(e) => cx.emit(ImageEvent::Error(format!(
                        "Could not remove the unused images: {}",
                        clean_error(&e)
                    ))),
                }
                this.reload(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Pulls an image. `reference` must be in the form of
    /// `images::normalize_reference`. A second pull of the same reference
    /// does nothing while the first one runs.
    pub fn pull(&mut self, reference: &str, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        if self.pulls.contains_key(reference) {
            return;
        }
        let reference = reference.to_string();
        let rx = Backend::global(cx).stream(images::pull(&docker, &reference));
        let task = cx.spawn({
            let reference = reference.clone();
            async move |this, cx| {
                let mut error = None;
                while let Some(batch) = recv_batch(&rx, 256).await {
                    let running = this.update(cx, |this, cx| {
                        let Some(pull) = this.pulls.get_mut(&reference) else { return false };
                        for item in batch {
                            match item {
                                Ok(info) => pull.progress.apply(&info),
                                Err(e) => error = Some(e),
                            }
                        }
                        cx.notify();
                        true
                    });
                    if !running.unwrap_or(false) || error.is_some() {
                        break;
                    }
                }
                let _ = this.update(cx, |this, cx| {
                    // This task is the task of the pull, so it must not drop
                    // itself while it runs.
                    let Some(pull) = this.pulls.shift_remove(&reference) else { return };
                    pull.task.detach();
                    match error {
                        Some(e) => cx.emit(ImageEvent::Error(format!(
                            "Could not pull {reference}: {}",
                            clean_error(&anyhow::anyhow!(e))
                        ))),
                        None => {
                            this.pulled.push(reference.clone());
                            this.reload(cx);
                        }
                    }
                    cx.notify();
                });
            }
        });
        self.pulls.insert(reference, Pull { progress: PullProgress::default(), task });
        cx.notify();
    }

    /// Stops a pull. The layers that came stay in the engine.
    pub fn cancel_pull(&mut self, reference: &str, cx: &mut Context<Self>) {
        if self.pulls.shift_remove(reference).is_some() {
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::containers::State;

    fn image(id: &str, tags: &[&str], size: u64) -> ImageRow {
        ImageRow {
            id: format!("sha256:{id}"),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            digests: Vec::new(),
            created: 0,
            size,
            icon: None,
        }
    }

    fn container(name: &str, image_id: &str) -> ContainerRow {
        ContainerRow {
            id: name.into(),
            name: name.into(),
            image: String::new(),
            image_id: format!("sha256:{image_id}"),
            state: State::Running,
            status: String::new(),
            ports: Vec::new(),
            compose_project: None,
            compose_service: None,
            compose_file: None,
            icon: None,
            created: 0,
            finished_at: None,
            volumes: Vec::new(),
            networks: Vec::new(),
        }
    }

    fn keys(list: &[ImageItem]) -> Vec<String> {
        list.iter()
            .map(|i| match i {
                ImageItem::Pull { reference } => format!("<{reference}>"),
                ImageItem::Group { key, .. } => format!("[{}]", key.trim_start_matches('\0')),
                ImageItem::Row { key, .. } => key.clone(),
            })
            .collect()
    }

    fn rows() -> Vec<ImageRow> {
        vec![
            image("aaa111", &["redis:7-alpine"], 50),
            image("bbb222", &["node:24-alpine"], 200),
            image("ccc333", &["node:24-bookworm"], 1600),
            image("ddd444", &[], 900),
            image("eee555", &["alpine:latest", "alpine:3.22"], 10),
            image("fff666", &[], 5),
        ]
    }

    #[test]
    fn repositories_with_more_tags_are_groups() {
        let rows = rows();
        let containers = [container("cache", "aaa111"), container("api", "ccc333")];
        let users = image_users(containers.iter());
        let collapsed = HashSet::new();
        let pulls = ["nginx:latest".to_string()];
        let options = |filter| ListOptions {
            filter,
            unused_only: false,
            sort: ImageSort::Name,
            collapsed: &collapsed,
            users: &users,
        };
        let list = build_list(rows.iter(), pulls.iter(), &options(""));
        assert_eq!(
            keys(&list),
            [
                "<nginx:latest>",
                "[alpine]",
                "alpine:3.22",
                "alpine:latest",
                "[node]",
                "node:24-alpine",
                "node:24-bookworm",
                "redis:7-alpine",
                "[untagged]",
                "sha256:ddd444",
                "sha256:fff666",
            ]
        );
        assert_eq!(list[4], ImageItem::Group { key: "node".into(), total: 2, unused: 1, collapsed: false });
        // A filter can make a group of one tag into a single row.
        let none: [String; 0] = [];
        assert_eq!(keys(&build_list(rows.iter(), none.iter(), &options("BOOKWORM"))), ["node:24-bookworm"]);
        // A filter also matches the start of the ID.
        assert_eq!(keys(&build_list(rows.iter(), none.iter(), &options("ddd"))), ["[untagged]", "sha256:ddd444"]);
    }

    #[test]
    fn unused_filter_and_size_sort() {
        let rows = rows();
        let containers = [container("cache", "aaa111"), container("api", "ccc333")];
        let users = image_users(containers.iter());
        let collapsed = HashSet::from([UNTAGGED_GROUP.to_string()]);
        let none: [String; 0] = [];
        let options = ListOptions {
            filter: "",
            unused_only: false,
            sort: ImageSort::Size,
            collapsed: &collapsed,
            users: &users,
        };
        // Two tags of one image count one time in the size of a group.
        assert_eq!(
            keys(&build_list(rows.iter(), none.iter(), &options)),
            ["[node]", "node:24-bookworm", "node:24-alpine", "redis:7-alpine", "[alpine]", "alpine:3.22", "alpine:latest", "[untagged]"]
        );
        let unused = ListOptions { unused_only: true, ..options };
        assert_eq!(
            keys(&build_list(rows.iter(), none.iter(), &unused)),
            ["node:24-alpine", "[alpine]", "alpine:3.22", "alpine:latest", "[untagged]"]
        );
    }

    #[test]
    fn users_come_from_the_image_ids() {
        let containers = [container("web", "aaa111"), container("db", "aaa111"), container("x", "bbb222")];
        let users = image_users(containers.iter());
        let names: Vec<&str> = users["sha256:aaa111"].iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["db", "web"]);
        assert!(!users.contains_key("sha256:ccc333"));
    }
}

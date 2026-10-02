//! The grouped, virtualized network list.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::networks::NetworkRow;
use crate::state::container_store::ContainerStore;
use crate::state::network_store::{
    BUILT_IN_GROUP, ListOptions, NetworkItem, NetworkStore, build_list, is_unused, network_users,
};
use crate::ui::container_list::{
    CHILD_INSET, DOCKER_BLUE, Entrance, GUTTER, ICON_SIZE, ROW_HEIGHT, ROW_INSET, ROW_TRAILING_INSET, Tree, divider_line, tree_lines,
};
use crate::ui::key_nav::{self, CursorList, Stop};
use crate::ui::native::MenuEntry;
use crate::ui::{
    ENTER_DURATION, SNAPPY_SPRING, TOOLBAR_HEIGHT, delayed_reveal, drag_area, native, spinner, tabular_nums, text, toast,
};

actions!(
    network_list,
    [NewNetwork, RemoveSelected, FocusSearch, ClearSelection, ToggleUnusedOnly]
);

const CONTEXT: &str = "NetworkList";
/// The list context, and the context of the keys that all lists share.
const KEY_CONTEXT: &str = "NetworkList NavList";
const SUBNET_WIDTH: f32 = 130.;
const ACTIONS_WIDTH: f32 = 40.;
/// The most containers that the subtitle of a row names.
const USERS_SHOWN: usize = 2;
const ROW_GROUP: &str = "network-row";
const HEADER_GROUP: &str = "network-group";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-n", NewNetwork, Some(CONTEXT)),
        KeyBinding::new("cmd-backspace", RemoveSelected, Some(CONTEXT)),
        KeyBinding::new("cmd-f", FocusSearch, Some(CONTEXT)),
        KeyBinding::new("escape", ClearSelection, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-u", ToggleUnusedOnly, Some(CONTEXT)),
    ]);
}

pub enum NetworkListEvent {
    /// `pointer` is true when a click made the selection, and false for a key.
    Selected { name: Option<String>, pointer: bool },
    /// Show a container in the container list.
    RevealContainer(String),
}

/// The containers on one network, by name. `rebuild` sets it, so the rows
/// do not read all the containers.
#[derive(Default)]
struct Users {
    running: Vec<String>,
    stopped: Vec<String>,
}

pub struct NetworkList {
    store: Entity<NetworkStore>,
    containers: Entity<ContainerStore>,
    search: Entity<InputState>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    collapsed: HashSet<String>,
    items: Rc<Vec<NetworkItem>>,
    users: HashMap<String, Users>,
    /// The rows that come into the list now, with the entrance and an
    /// animation number. A row stays here for one `ENTER_DURATION` only.
    entering: HashMap<String, (Entrance, usize)>,
    entrances: usize,
    /// The network names at the last rebuild. It is `None` until the first
    /// full list arrives, so the first list does not fade in row by row.
    known: Option<HashSet<String>>,
    selected: Option<String>,
    /// The row with the hover. The pointer and the arrow keys move it.
    cursor: key_nav::ListCursor,
    /// A network to select when it comes into the list. A link from a
    /// container can ask for a network before the first list arrives.
    pending_reveal: Option<String>,
    search_open: bool,
    /// Show only the networks that a prune deletes.
    unused_only: bool,
    /// Extra space at the left of the toolbar. The sidebar button uses this
    /// space when the sidebar is collapsed.
    leading_inset: Pixels,
    _subs: Vec<Subscription>,
}

impl EventEmitter<NetworkListEvent> for NetworkList {}

impl Focusable for NetworkList {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl NetworkList {
    pub fn new(
        store: Entity<NetworkStore>,
        containers: Entity<ContainerStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search networks"));
        let subs = vec![
            cx.observe(&store, |this, _, cx| this.rebuild(cx)),
            // The networks of the containers tell which networks are in use.
            cx.observe(&containers, |this, _, cx| this.rebuild(cx)),
            cx.subscribe(&search, |this, _, ev: &InputEvent, cx| {
                if let InputEvent::Change = ev {
                    this.rebuild(cx);
                }
            }),
        ];
        let mut this = NetworkList {
            store,
            containers,
            search,
            focus: cx.focus_handle().tab_stop(true),
            scroll: UniformListScrollHandle::new(),
            collapsed: HashSet::new(),
            items: Rc::new(Vec::new()),
            users: HashMap::new(),
            entering: HashMap::new(),
            entrances: 0,
            known: None,
            selected: None,
            cursor: key_nav::ListCursor::default(),
            pending_reveal: None,
            search_open: false,
            unused_only: false,
            leading_inset: px(0.),
            _subs: subs,
        };
        this.rebuild(cx);
        this
    }

    pub fn leading_inset(&self) -> Pixels {
        self.leading_inset
    }

    pub fn set_leading_inset(&mut self, inset: Pixels, cx: &mut Context<Self>) {
        self.leading_inset = inset;
        cx.notify();
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let filter = self.search.read(cx).value();
        let store = self.store.read(cx);
        let containers = self.containers.read(cx);
        let users = network_users(containers.set.rows.values());
        let options = ListOptions {
            filter: &filter,
            unused_only: self.unused_only,
            collapsed: &self.collapsed,
            users: &users,
        };
        self.items = Rc::new(build_list(store.rows.values(), &options));
        self.users = users
            .into_iter()
            .map(|(n, list)| {
                let (running, stopped): (Vec<_>, Vec<_>) = list.into_iter().partition(|c| c.state.is_running());
                let names = |list: Vec<&crate::docker::ContainerRow>| list.into_iter().map(|c| c.name.clone()).collect();
                (n.to_string(), Users { running: names(running), stopped: names(stopped) })
            })
            .collect();
        // A network that appears after the first list fades in. The engine
        // fade covers a new connection, so it starts again.
        let mut new = Vec::new();
        if store.is_loading() {
            self.known = None;
        } else {
            let names: HashSet<String> = store.rows.keys().cloned().collect();
            if let Some(known) = &self.known {
                new = names.difference(known).cloned().collect();
            }
            self.known = Some(names);
        }
        let gone = self.selected.as_ref().is_some_and(|sel| !store.rows.contains_key(sel));
        if gone {
            self.select(None, false, cx);
        }
        self.enter(new, Entrance::New, cx);
        if let Some(name) = self.pending_reveal.take() {
            self.reveal(&name, cx);
        }
        cx.notify();
    }

    /// Plays an entrance on `names`, and removes the mark when it ends.
    fn enter(&mut self, names: Vec<String>, entrance: Entrance, cx: &mut Context<Self>) {
        if names.is_empty() {
            return;
        }
        self.entrances += 1;
        let n = self.entrances;
        for name in names {
            self.entering.insert(name, (entrance, n));
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ENTER_DURATION).await;
            let _ = this.update(cx, |this, _| this.entering.retain(|_, (_, m)| *m != n));
        })
        .detach();
    }

    fn select(&mut self, name: Option<String>, pointer: bool, cx: &mut Context<Self>) {
        if self.selected == name {
            return;
        }
        self.selected = name.clone();
        cx.emit(NetworkListEvent::Selected { name, pointer });
        cx.notify();
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.select(None, true, cx);
    }

    /// Selects a network, and scrolls to it. A collapsed group of the
    /// network expands.
    pub fn reveal(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(row) = self.store.read(cx).rows.get(name) else {
            // The list is not loaded yet. `rebuild` tries again.
            if self.store.read(cx).is_loading() {
                self.pending_reveal = Some(name.to_string());
            }
            return;
        };
        let key = group_key(row);
        // The filter would hide a network that is in use.
        let hidden = self.unused_only && !self.is_unused(row);
        if self.collapsed.remove(&key) || hidden {
            self.unused_only &= !hidden;
            self.rebuild(cx);
        }
        let ix = self
            .items
            .iter()
            .position(|item| matches!(item, NetworkItem::Row { name: row, .. } if row == name));
        if let Some(ix) = ix {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        self.select(Some(name.to_string()), false, cx);
    }

    /// True if a prune deletes the network.
    fn is_unused(&self, row: &NetworkRow) -> bool {
        !row.is_built_in() && self.users.get(&row.name).is_none_or(|u| u.running.is_empty())
    }

    /// Asks for a name, then makes a network. A name that the engine
    /// refuses asks again, with the reason.
    fn request_create(&mut self, value: String, problem: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let request = native::TextRequest {
            message: "New Network".into(),
            info: problem.unwrap_or("Docker makes a bridge network and chooses its subnet.").into(),
            value,
            placeholder: "Name".into(),
            ok: "Create".into(),
        };
        native::ask_text(request, window, cx, |this, name, window, cx| {
            let name = name.trim().to_string();
            let problem = match crate::docker::check_name(&name) {
                Err(problem) => Some(problem),
                Ok(()) if this.store.read(cx).rows.contains_key(&name) => Some("A network with this name exists."),
                Ok(()) => None,
            };
            match problem {
                Some(problem) => this.request_create(name, Some(problem), window, cx),
                None => this.store.update(cx, |s, cx| s.create(&name, cx)),
            }
        });
    }

    fn toggle_group(&mut self, key: &str, cx: &mut Context<Self>) {
        let expanded = self.collapsed.remove(key);
        if !expanded {
            self.collapsed.insert(key.to_string());
        }
        self.rebuild(cx);
        // The members fade in below their group. A collapse is immediate,
        // so the rows below move up with no wait.
        if expanded {
            let members = self.group_members(key, cx);
            let shown: Vec<String> = self
                .items
                .iter()
                .filter_map(|item| match item {
                    NetworkItem::Row { name, grouped: true } if members.contains(name) => Some(name.clone()),
                    _ => None,
                })
                .collect();
            self.enter(shown, Entrance::Expand, cx);
        }
    }

    /// The containers on a network: the ID and the name.
    fn containers_on(&self, name: &str, cx: &App) -> Vec<(String, String)> {
        let mut list: Vec<(String, String)> = self
            .containers
            .read(cx)
            .set
            .rows
            .values()
            .filter(|c| c.networks.iter().any(|n| n == name))
            .map(|c| (c.id.clone(), c.name.clone()))
            .collect();
        list.sort_by(|a, b| a.1.cmp(&b.1));
        list
    }

    /// Removes a network after the user confirms.
    pub fn request_remove(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let Some(row) = store.read(cx).rows.get(&name).cloned() else { return };
        let users = self.users.get(&name);
        let running = users.map_or(&[][..], |u| u.running.as_slice());
        let stopped = users.map_or(&[][..], |u| u.stopped.as_slice());
        // Docker refuses these removes, so tell the user why before the call.
        // The sheets have one button, so the answer is not necessary.
        if row.is_built_in() {
            drop(window.prompt(
                PromptLevel::Info,
                "This network is built in",
                Some("Docker makes this network. You cannot remove it."),
                &[PromptButton::ok("OK")],
                cx,
            ));
            return;
        }
        if !running.is_empty() {
            drop(window.prompt(
                PromptLevel::Info,
                "The network is in use",
                Some(&format!("Stop the containers on it first: {}.", running.join(", "))),
                &[PromptButton::ok("OK")],
                cx,
            ));
            return;
        }
        let detail = if stopped.is_empty() {
            "Docker deletes the network. You cannot undo this.".to_string()
        } else {
            format!(
                "Docker deletes the network. These stopped containers cannot start again until the network exists again: {}.",
                stopped.join(", ")
            )
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Remove “{}”?", row.name),
            Some(&detail),
            &[PromptButton::Cancel("Cancel".into()), PromptButton::new("Remove")],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(1) {
                store.update(cx, |s, cx| s.remove(&name, cx));
            }
        })
        .detach();
    }

    /// The text of a prune or a group remove: the stopped containers that
    /// cannot start after the networks go away.
    fn stopped_note(&self, names: &[String]) -> String {
        let mut stopped: Vec<&str> = names
            .iter()
            .filter_map(|n| self.users.get(n))
            .flat_map(|u| u.stopped.iter().map(String::as_str))
            .collect();
        stopped.sort();
        stopped.dedup();
        if stopped.is_empty() {
            String::new()
        } else {
            format!(" These stopped containers cannot start again until their network exists again: {}.", stopped.join(", "))
        }
    }

    /// Removes all the networks that no running container is on, after the
    /// user confirms.
    fn request_prune(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let unused: Vec<String> = {
            let containers = self.containers.read(cx);
            let users = network_users(containers.set.rows.values());
            store.read(cx).rows.values().filter(|n| is_unused(n, &users)).map(|n| n.name.clone()).collect()
        };
        let count = unused.len();
        if count == 0 {
            drop(window.prompt(
                PromptLevel::Info,
                "No unused networks",
                Some("A running container is on each network, or Docker made the network."),
                &[PromptButton::ok("OK")],
                cx,
            ));
            return;
        }
        let noun = if count == 1 { "network" } else { "networks" };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Remove {count} unused {noun}?"),
            Some(&format!(
                "Docker deletes the {noun} that no running container is on. The built-in networks stay.{}",
                self.stopped_note(&unused)
            )),
            &[PromptButton::Cancel("Cancel".into()), PromptButton::new("Remove")],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(1) {
                store.update(cx, |s, cx| s.prune(cx));
            }
        })
        .detach();
    }

    fn toggle_unused_only(&mut self, cx: &mut Context<Self>) {
        self.unused_only = !self.unused_only;
        self.rebuild(cx);
    }

    /// The names of the networks of a group, sorted.
    fn group_members(&self, key: &str, cx: &App) -> Vec<String> {
        let mut names: Vec<String> = self
            .store
            .read(cx)
            .rows
            .values()
            .filter(|n| group_key(n) == key)
            .map(|n| n.name.clone())
            .collect();
        names.sort();
        names
    }

    fn show_group_menu(&mut self, key: String, at: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        let members = self.group_members(&key, cx);
        let rows = &self.store.read(cx).rows;
        let unused = members.iter().filter(|n| rows.get(*n).is_some_and(|r| self.is_unused(r))).count();
        let entries = vec![
            MenuEntry::Item { label: "Remove Unused Networks…".into(), enabled: unused > 0 },
            MenuEntry::Separator,
            MenuEntry::item("Copy Names"),
        ];
        native::show_context_menu_at(entries, at, window, cx, move |this, ix, window, cx| match ix {
            0 => this.request_group_remove(key.clone(), window, cx),
            2 => {
                let noun = if members.len() == 1 { "name" } else { "names" };
                toast::copy(members.join("\n"), format!("{} network {noun}", members.len()), cx)
            }
            _ => {}
        });
    }

    /// Removes the networks of a group that no running container is on,
    /// after the user confirms.
    fn request_group_remove(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let unused: Vec<String> = {
            let rows = &store.read(cx).rows;
            self.group_members(&key, cx)
                .into_iter()
                .filter(|n| rows.get(n).is_some_and(|r| self.is_unused(r)))
                .collect()
        };
        if unused.is_empty() {
            return;
        }
        let count = unused.len();
        let noun = if count == 1 { "network" } else { "networks" };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Remove {count} unused {noun} of “{key}”?"),
            Some(&format!(
                "Docker deletes the {noun} that no running container is on.{}",
                self.stopped_note(&unused)
            )),
            &[PromptButton::Cancel("Cancel".into()), PromptButton::new("Remove")],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(1) {
                store.update(cx, |s, cx| s.remove_many(&unused, cx));
            }
        })
        .detach();
    }

    fn show_row_menu(&mut self, name: String, at: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.store.read(cx).rows.get(&name).cloned() else {
            return;
        };
        let users = self.containers_on(&name, cx);
        type Handler = Box<dyn Fn(&mut NetworkList, &mut Window, &mut Context<NetworkList>)>;
        let mut entries: Vec<(MenuEntry, Option<Handler>)> = Vec::new();
        for (id, container) in users.clone() {
            entries.push((
                MenuEntry::item(format!("Show Container “{container}”")),
                Some(Box::new(move |_, _, cx| cx.emit(NetworkListEvent::RevealContainer(id.clone())))),
            ));
        }
        if !users.is_empty() {
            entries.push((MenuEntry::Separator, None));
        }
        let full_name = row.name.clone();
        entries.push((
            MenuEntry::item("Copy Name"),
            Some(Box::new(move |_, _, cx| toast::copy(full_name.clone(), full_name.clone(), cx))),
        ));
        let id = row.id.clone();
        entries.push((
            MenuEntry::item("Copy ID"),
            Some(Box::new(move |_, _, cx| toast::copy(id.clone(), id[..id.len().min(12)].to_string(), cx))),
        ));
        if let Some(subnet) = row.subnets.first().cloned() {
            entries.push((
                MenuEntry::item("Copy Subnet"),
                Some(Box::new(move |_, _, cx| toast::copy(subnet.clone(), subnet.clone(), cx))),
            ));
        }
        entries.push((MenuEntry::Separator, None));
        entries.push((
            MenuEntry::Item { label: "Remove…".into(), enabled: self.can_remove(&row) },
            Some(Box::new(move |this, window, cx| this.request_remove(name.clone(), window, cx))),
        ));
        let (entries, handlers): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        native::show_context_menu_at(entries, at, window, cx, move |this, ix, window, cx| {
            if let Some(Some(handler)) = handlers.get(ix) {
                handler(this, window, cx);
            }
        });
    }

    /// True if Docker accepts a remove of the network.
    fn can_remove(&self, row: &NetworkRow) -> bool {
        !row.is_built_in() && self.users.get(&row.name).is_none_or(|u| u.running.is_empty())
    }

    fn render_item(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let item = match &self.items[ix] {
            NetworkItem::Group { key, total, unused, collapsed } => {
                self.render_group(ix, key, *total, *unused, *collapsed, cx)
            }
            NetworkItem::Row { name, grouped } => self.render_row(ix, name, *grouped, window, cx),
        };
        // A line separates the groups from the single networks, and the
        // single networks from the group of built-in networks.
        let prev = ix.checked_sub(1).map(|i| &self.items[i]);
        let divider = match (&self.items[ix], prev) {
            (NetworkItem::Row { grouped: false, .. }, Some(p)) => {
                matches!(p, NetworkItem::Row { grouped: true, .. } | NetworkItem::Group { .. })
            }
            (NetworkItem::Group { key, .. }, Some(NetworkItem::Row { grouped: false, .. })) => key == BUILT_IN_GROUP,
            _ => false,
        };
        let next_is_child = matches!(self.items.get(ix + 1), Some(NetworkItem::Row { grouped: true, .. }));
        let tree = match &self.items[ix] {
            NetworkItem::Group { .. } if next_is_child => Some(Tree::Stem),
            NetworkItem::Row { grouped: true, .. } if next_is_child => Some(Tree::Branch),
            NetworkItem::Row { grouped: true, .. } => Some(Tree::Last),
            _ => None,
        };
        let line = cx.theme().muted_foreground.opacity(0.35);
        div()
            .relative()
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(ROW_INSET))
            .pr(px(ROW_TRAILING_INSET))
            .py_px()
            .when(divider, |d| d.pt(px(4.)).child(divider_line(cx)))
            .when_some(tree, |d, tree| d.children(tree_lines(tree, line)))
            .child(item)
            .map(|d| {
                let entering = match &self.items[ix] {
                    NetworkItem::Row { name, .. } => self.entering.get(name).map(|e| (name, *e)),
                    NetworkItem::Group { .. } => None,
                };
                match entering {
                    Some((name, (entrance, n))) => d
                        .with_animation(
                            ElementId::named_usize(format!("enter-{name}"), n),
                            Animation::new(ENTER_DURATION).with_easing(ease_out_quint()),
                            move |d, t| {
                                let d = d.opacity(t);
                                if entrance == Entrance::Expand {
                                    d.top(px(-4. * (1. - t)))
                                } else {
                                    d
                                }
                            },
                        )
                        .into_any_element(),
                    None => d.into_any_element(),
                }
            })
    }

    fn render_group(
        &self,
        ix: usize,
        key: &str,
        total: usize,
        unused: usize,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = cx.theme();
        let built_in = key == BUILT_IN_GROUP;
        let mut summary = format!("{total} {}", if total == 1 { "network" } else { "networks" });
        if !built_in && unused == total {
            summary.push_str(" · none in use");
        } else if unused > 0 {
            summary.push_str(&format!(" · {unused} not in use"));
        }
        let toggle = key.to_string();
        let menu_key = key.to_string();
        let tile = div()
            .size(px(ICON_SIZE))
            .flex_shrink_0()
            .rounded(px(8.))
            .flex()
            .items_center()
            .justify_center();
        let tile = if built_in {
            tile.bg(t.muted).child(Icon::new(IconName::Globe).small().text_color(t.muted_foreground))
        } else {
            tile.bg(DOCKER_BLUE)
                .child(svg().path(crate::assets::DOCKER_LOGO).size(px(20.)).text_color(gpui::white()))
        };
        let cursor = self.cursor.is_group(key);
        let hover_key = key.to_string();
        h_flex()
            .id(("group", ix))
            .group(HEADER_GROUP)
            .relative()
            .size_full()
            .pr_2()
            .gap_3()
            .rounded(px(8.))
            // The pointer and the arrow keys move the same hover.
            .when(cursor, |d| d.bg(native::hover(cx)).child(key_nav::track_bounds(self.cursor.bounds.clone())))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                key_nav::hover_row(this, key_nav::Cursor::Group(hover_key.clone()), *hovered, cx)
            }))
            .child(
                div().w(px(GUTTER - 12.)).flex().justify_end().child(
                    Icon::new(IconName::ChevronRight)
                        .xsmall()
                        .text_color(t.muted_foreground)
                        .with_spring(
                            "chevron",
                            SpringAnimation::new(SNAPPY_SPRING).to(if collapsed { 0. } else { 1. }),
                            |icon, turn: f32| icon.rotate(radians(turn * std::f32::consts::FRAC_PI_2)),
                        ),
                ),
            )
            .child(tile)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(text::BODY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if built_in { "Built-in Networks".to_string() } else { key.to_string() }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(text::CALLOUT))
                            .font_features(tabular_nums())
                            .text_color(t.muted_foreground)
                            .child(summary),
                    ),
            )
            .child(div().w(px(SUBNET_WIDTH + ACTIONS_WIDTH)).flex_shrink_0())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus.focus(window, cx);
                this.toggle_group(&toggle, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| this.show_group_menu(menu_key.clone(), None, window, cx)),
            )
            .into_any_element()
    }

    fn render_row(&self, ix: usize, name: &str, grouped: bool, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let Some(row) = store.rows.get(name) else {
            return div().h(px(ROW_HEIGHT)).into_any_element();
        };
        let t = cx.theme();
        let removing = store.removing.contains(name);
        let selected = self.selected.as_deref() == Some(name);
        let hovered = self.cursor.is_row(name);
        let emphasized = window.is_window_active() && self.focus.contains_focused(window, cx);
        let on_accent = selected && emphasized && native::has_emphasized_selection(cx);
        let secondary = if on_accent { gpui::white().opacity(0.8) } else { t.muted_foreground };
        let users = self.users.get(name);
        let running = users.map_or(&[][..], |u| u.running.as_slice());
        let stopped = users.map_or(&[][..], |u| u.stopped.as_slice());
        let in_use = !running.is_empty();
        let named = |list: &[String]| {
            let more = list.len().saturating_sub(USERS_SHOWN);
            let mut s = list[..list.len().min(USERS_SHOWN)].join(", ");
            if more > 0 {
                s.push_str(&format!(" and {more} more"));
            }
            s
        };
        let mut subtitle = if in_use {
            format!("Used by {}", named(running))
        } else if !stopped.is_empty() {
            format!("Stopped: {}", named(stopped))
        } else {
            "Not in use".to_string()
        };
        if row.driver != "bridge" && !row.driver.is_empty() {
            subtitle.push_str(&format!(" · {}", row.driver));
        }
        if row.internal {
            subtitle.push_str(" · internal");
        }
        let removable = self.can_remove(row);

        let actions = if removing {
            h_flex()
                .gap_1()
                .text_size(px(text::CALLOUT))
                .text_color(t.muted_foreground)
                .child(delayed_reveal(("removing", ix), div().child(spinner().xsmall())))
                .into_any_element()
        } else {
            let remove_name = name.to_string();
            let tooltip = if row.is_built_in() {
                "Docker makes this network. You cannot remove it."
            } else if in_use {
                "In use. Stop the containers first."
            } else {
                "Remove (⌘⌫)"
            };
            h_flex()
                .when(!selected && !hovered, |d| d.invisible())
                .child(
                    Button::new(("remove", ix))
                        .icon(IconName::Trash)
                        .when(selected, |b| b.outline())
                        .when(!selected, |b| b.ghost())
                        .xsmall()
                        // Command-Delete and the context menu remove the
                        // row, so Tab does not stop on each row.
                        .tab_stop(false)
                        .disabled(!removable)
                        .tooltip(tooltip)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.request_remove(remove_name.clone(), window, cx);
                        })),
                )
                .into_any_element()
        };

        let icon_color = if on_accent {
            gpui::white()
        } else if in_use {
            native::accent(cx)
        } else {
            t.muted_foreground
        };
        let row_name = name.to_string();
        let hover_key = name.to_string();
        h_flex()
            .id(("row", ix))
            .group(ROW_GROUP)
            .relative()
            .h_full()
            .flex_1()
            .map(|d| if grouped { d.ml(px(CHILD_INSET - ROW_INSET)).pl(px(12.)) } else { d.pl(px(GUTTER)) })
            .pr_2()
            .gap_3()
            .text_size(px(text::BODY))
            .rounded(px(8.))
            .map(|d| {
                if selected {
                    d.bg(native::selection(emphasized, cx))
                } else if hovered {
                    d.bg(native::hover(cx))
                } else {
                    d
                }
            })
            // The context menu key opens the menu of the hovered row, or of
            // the selected row when no row has the hover.
            .when(hovered || (selected && self.cursor.at.is_none()), |d| {
                d.child(key_nav::track_bounds(self.cursor.bounds.clone()))
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                key_nav::hover_row(this, key_nav::Cursor::Row(hover_key.clone()), *hovered, cx)
            }))
            .when(on_accent, |d| d.text_color(gpui::white()))
            .when(removing, |d| d.opacity(0.7))
            .child(
                div()
                    .size(px(ICON_SIZE))
                    .flex_shrink_0()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if on_accent { gpui::white().opacity(0.4) } else { t.border })
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(IconName::Globe).small().text_color(icon_color)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .when(!in_use, |d| d.text_color(secondary))
                            .child(row.display_name(grouped).to_string()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(text::CALLOUT))
                            .text_color(secondary)
                            .child(subtitle),
                    ),
            )
            .child(
                div()
                    .w(px(SUBNET_WIDTH))
                    .flex_shrink_0()
                    .flex()
                    .justify_end()
                    .truncate()
                    .font_family(t.mono_font_family.clone())
                    .text_size(px(text::CALLOUT))
                    .text_color(secondary)
                    .when_some(row.subnets.first(), |d, s| d.child(s.clone())),
            )
            .child(h_flex().w(px(ACTIONS_WIDTH)).flex_shrink_0().justify_end().child(actions))
            .on_click({
                let row_name = row_name.clone();
                cx.listener(move |this, _, window, cx| {
                    this.focus.focus(window, cx);
                    this.select(Some(row_name.clone()), true, cx);
                })
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    this.focus.focus(window, cx);
                    this.select(Some(row_name.clone()), true, cx);
                    this.show_row_menu(row_name.clone(), None, window, cx);
                }),
            )
            .into_any_element()
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = !self.search_open;
        if self.search_open {
            self.search.update(cx, |s, cx| s.focus(window, cx));
        } else {
            // A hidden search must not filter the list.
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
            self.focus.focus(window, cx);
        }
        cx.notify();
    }
}

/// The group of a network in the list. A network with no group has an
/// empty key.
fn group_key(row: &NetworkRow) -> String {
    match &row.compose_project {
        _ if row.is_built_in() => BUILT_IN_GROUP.to_string(),
        Some(p) => p.clone(),
        None => String::new(),
    }
}

impl CursorList for NetworkList {
    fn stops(&self) -> Vec<Stop> {
        key_nav::grouped_stops(self.items.iter(), |item| match item {
            NetworkItem::Group { key, collapsed, .. } => Some(Ok((key.as_str(), *collapsed))),
            NetworkItem::Row { name, grouped } => Some(Err((name.as_str(), *grouped))),
        })
    }

    fn list_cursor(&self) -> &key_nav::ListCursor {
        &self.cursor
    }

    fn list_cursor_mut(&mut self) -> &mut key_nav::ListCursor {
        &mut self.cursor
    }

    fn selected_key(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    fn select_key(&mut self, key: String, cx: &mut Context<Self>) {
        self.select(Some(key), false, cx);
    }

    fn scroll(&self) -> &UniformListScrollHandle {
        &self.scroll
    }

    fn toggle_group_at(&mut self, key: &str, cx: &mut Context<Self>) {
        self.toggle_group(key, cx);
    }

    fn show_menu_at(&mut self, stop: &Stop, at: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        match stop {
            Stop::Group { key, .. } => self.show_group_menu(key.clone(), at, window, cx),
            Stop::Row { key, .. } => self.show_row_menu(key.clone(), at, window, cx),
            Stop::Skip => {}
        }
    }
}

impl Render for NetworkList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let store = self.store.read(cx);
        let total = store.rows.len();
        let loading = store.is_loading();
        let pruning = store.pruning;
        let count = self.items.len();
        let mut summary = format!("{total} {}", if total == 1 { "network" } else { "networks" });
        if self.unused_only {
            summary.push_str(" · in use hidden");
        }

        let body = if count == 0 {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(t.muted_foreground)
                .child(if loading {
                    delayed_reveal("networks-loading", div().child(spinner())).into_any_element()
                } else if total == 0 {
                    div().child("No networks").into_any_element()
                } else if self.unused_only && self.search.read(cx).value().trim().is_empty() {
                    div().child("All the networks are in use").into_any_element()
                } else {
                    div().child("No networks match the search").into_any_element()
                })
                .into_any_element()
        } else {
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    uniform_list(
                        "networks",
                        count,
                        cx.processor(|this, range: Range<usize>, window, cx| {
                            range.map(|ix| this.render_item(ix, window, cx)).collect::<Vec<_>>()
                        }),
                    )
                    .size_full()
                    .track_scroll(&self.scroll),
                )
                .child(Scrollbar::vertical(&self.scroll))
                .into_any_element()
        };

        let root = key_nav::list_actions(v_flex(), cx);
        root.key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|this, _: &RemoveSelected, window, cx| {
                if let Some(name) = this.selected.clone() {
                    this.request_remove(name, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| {
                this.cursor.at = None;
                this.select(None, false, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NewNetwork, window, cx| this.request_create(String::new(), None, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleUnusedOnly, _, cx| this.toggle_unused_only(cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.search_open = true;
                this.search.update(cx, |s, cx| s.focus(window, cx));
                cx.notify();
            }))
            .child(
                h_flex()
                    .h(px(TOOLBAR_HEIGHT + 8.))
                    .flex_shrink_0()
                    .px_5()
                    .pl(px(20.) + self.leading_inset)
                    .gap_1()
                    .border_b_1()
                    .border_color(t.border)
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(text::TITLE3))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Networks"),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(text::CALLOUT))
                                    .font_features(tabular_nums())
                                    .text_color(t.muted_foreground)
                                    .child(summary),
                            ),
                    )
                    .child(drag_area("network-list-drag").flex_1().h_full())
                    .when(self.search_open, |d| {
                        d.child(
                            div().w(px(220.)).min_w(px(120.)).mr_1().child(
                                Input::new(&self.search)
                                    .small()
                                    .prefix(Icon::new(IconName::Search).small())
                                    .cleanable(true),
                            ),
                        )
                    })
                    .child(
                        Button::new("new-network")
                            .icon(Icon::new(IconName::Plus))
                            .custom(native::toolbar_button(cx))
                            .small()
                            .tooltip("New network (⌘N)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request_create(String::new(), None, window, cx)
                            })),
                    )
                    .child(
                        Button::new("prune-networks")
                            .map(|b| if pruning { b.icon(spinner()) } else { b.icon(IconName::BrushCleaning) })
                            .custom(native::toolbar_button(cx))
                            .small()
                            .disabled(pruning || total == 0)
                            .tooltip("Remove unused networks…")
                            .on_click(cx.listener(|this, _, window, cx| this.request_prune(window, cx))),
                    )
                    .child(
                        Button::new("unused-only")
                            .icon(Icon::new(IconName::ListFilter))
                            .custom(native::toolbar_button(cx))
                            .small()
                            .selected(self.unused_only)
                            .tooltip("Show unused networks only (⇧⌘U)")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_unused_only(cx))),
                    )
                    .child(
                        Button::new("toggle-search")
                            .icon(Icon::new(IconName::Search))
                            .custom(native::toolbar_button(cx))
                            .small()
                            .selected(self.search_open)
                            .tooltip("Search (⌘F)")
                            .on_click(cx.listener(|this, _, window, cx| this.toggle_search(window, cx))),
                    ),
            )
            .child(div().h_2().flex_shrink_0())
            .child(body)
    }
}

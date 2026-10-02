//! The grouped, virtualized container list.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Icon, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::EngineStatus;
use crate::docker::containers::Action;
use crate::state::container_store::{ContainerStore, ListItem, StoreEvent, build_list};
use crate::state::stats_store::{StatsStore, select_wanted};
use crate::ui::native::MenuEntry;
use crate::ui::{
    ENTER_DURATION, SNAPPY_SPRING, TOOLBAR_HEIGHT, delayed_reveal, drag_area, native, spinner, tabular_nums, text, toast,
};

actions!(
    container_list,
    [
        StartSelected,
        StopSelected,
        RestartSelected,
        RemoveSelected,
        SelectNext,
        SelectPrev,
        FocusSearch,
        ClearSelection,
        ToggleRunningOnly
    ]
);

const CONTEXT: &str = "ContainerList";
pub(super) const ROW_HEIGHT: f32 = 54.;
const NAME_MIN_WIDTH: f32 = 170.;
pub(super) const ICON_SIZE: f32 = 32.;
/// The left space of a row. The chevron of a group sits in it, so the
/// icons of all rows align.
pub(super) const GUTTER: f32 = 34.;
/// The x position of the tree line, in the row: the center of the group
/// tile.
pub(super) const TREE_X: f32 = ROW_INSET + GUTTER + ICON_SIZE / 2.;
/// The left edge of a service row. It starts right of the tree line, so a
/// selected row does not cover the line.
pub(super) const CHILD_INSET: f32 = TREE_X + 6.;
const PORTS_WIDTH: f32 = 150.;
const ACTIONS_WIDTH: f32 = 80.;
/// The maximum number of group members that get a stats stream.
const GROUP_STATS_LIMIT: usize = 30;
/// The space between the list edge and the rounded row background.
pub(super) const ROW_INSET: f32 = 8.;
/// The space between the right list edge and the row background. The
/// scrollbar overlays the rows, so this inset is the scrollbar width.
pub(super) const ROW_TRAILING_INSET: f32 = 16.;
/// The list width that a service row with all its columns needs: the
/// indent, the icon, the name, the ports, the actions, the three gaps, the
/// right padding, and the right inset. A narrower list hides the ports, and
/// the names truncate. The actions always show.
const WIDE_LIST_WIDTH: f32 = CHILD_INSET + 12. + ICON_SIZE + NAME_MIN_WIDTH + PORTS_WIDTH
    + ACTIONS_WIDTH + 3. * 12. + 8. + ROW_TRAILING_INSET;
const ROW_GROUP: &str = "container-row";
const HEADER_GROUP: &str = "container-group";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-enter", StartSelected, Some(CONTEXT)),
        KeyBinding::new("cmd-.", StopSelected, Some(CONTEXT)),
        KeyBinding::new("cmd-r", RestartSelected, Some(CONTEXT)),
        KeyBinding::new("cmd-backspace", RemoveSelected, Some(CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("up", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("cmd-f", FocusSearch, Some(CONTEXT)),
        KeyBinding::new("escape", ClearSelection, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-r", ToggleRunningOnly, Some(CONTEXT)),
    ]);
}

pub enum ListEvent {
    /// `pointer` is true when a click made the selection, and false for a key.
    Selected { id: Option<String>, pointer: bool },
}

/// How a row comes into the list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entrance {
    /// The row is new in Docker. It fades in.
    New,
    /// The group of the row expanded. The row fades in and moves down a
    /// small distance, out from the group row.
    Expand,
}

pub struct ContainerList {
    store: Entity<ContainerStore>,
    stats: Entity<StatsStore>,
    search: Entity<InputState>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    collapsed: HashSet<String>,
    items: Rc<Vec<ListItem>>,
    /// The rows that come into the list now, with the entrance and an
    /// animation number. A row stays here for one `ENTER_DURATION` only.
    /// A row that scrolls out and in again then does not play the
    /// entrance again.
    entering: HashMap<String, (Entrance, usize)>,
    entrances: usize,
    /// The container ids at the last rebuild. It is `None` until the first
    /// full list arrives, so the first list does not fade in row by row.
    known: Option<HashSet<String>>,
    selected: Option<String>,
    search_open: bool,
    /// Show only the containers that run.
    running_only: bool,
    /// Extra space at the left of the toolbar. The sidebar button uses this
    /// space when the sidebar is collapsed.
    leading_inset: Pixels,
    /// True when the list is too narrow for the ports column.
    compact: bool,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ListEvent> for ContainerList {}

impl Focusable for ContainerList {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ContainerList {
    pub fn new(
        store: Entity<ContainerStore>,
        stats: Entity<StatsStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search containers"));
        let subs = vec![
            cx.subscribe(&store, |this, _, ev: &StoreEvent, cx| {
                if let StoreEvent::Changed = ev {
                    this.rebuild(cx);
                }
            }),
            cx.subscribe(&search, |this, _, ev: &InputEvent, cx| {
                if let InputEvent::Change = ev {
                    this.rebuild(cx);
                }
            }),
            // The group rows show the CPU and memory of their members.
            cx.observe(&stats, |_, _, cx| cx.notify()),
        ];
        let mut this = ContainerList {
            store,
            stats,
            search,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            collapsed: HashSet::new(),
            items: Rc::new(Vec::new()),
            entering: HashMap::new(),
            entrances: 0,
            known: None,
            selected: None,
            search_open: false,
            running_only: false,
            leading_inset: px(0.),
            compact: false,
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

    /// Tells the list its width, so that the rows can drop the ports column
    /// when the space is small.
    pub fn set_width(&mut self, width: Pixels, cx: &mut Context<Self>) {
        let compact = width < px(WIDE_LIST_WIDTH);
        if compact != self.compact {
            self.compact = compact;
            cx.notify();
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let filter = self.search.read(cx).value();
        let store = self.store.read(cx);
        let running_only = self.running_only;
        let rows = store
            .set
            .rows
            .values()
            .filter(|r| !running_only || r.state.is_running());
        self.items = Rc::new(build_list(rows, &filter, &self.collapsed));
        // A container that appears after the first list fades in. The
        // engine fade covers a new connection, so it starts again.
        let mut new = Vec::new();
        if store.status == EngineStatus::Connected && !store.is_loading() {
            let ids: HashSet<String> = store.set.rows.keys().cloned().collect();
            if let Some(known) = &self.known {
                new = ids.difference(known).cloned().collect();
            }
            self.known = Some(ids);
        } else {
            self.known = None;
        }
        // Clear a selection that points to a removed container.
        if let Some(sel) = &self.selected
            && !store.set.rows.contains_key(sel) {
                self.select(None, false, cx);
            }
        self.enter(new, Entrance::New, cx);
        self.sync_stats(cx);
        cx.notify();
    }

    /// Opens a stats stream for the selection, if it runs, and for the
    /// running members of the groups in the list. The detail pane shows the
    /// values of the selection. The group rows show the totals.
    fn sync_stats(&mut self, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let selected = self
            .selected
            .as_deref()
            .filter(|id| store.set.rows.get(*id).is_some_and(|r| r.state.is_running()));
        let projects: HashSet<&str> = self
            .items
            .iter()
            .filter_map(|i| match i {
                ListItem::Group { project, .. } => Some(project.as_str()),
                ListItem::Row { .. } => None,
            })
            .collect();
        let mut members: Vec<String> = store
            .set
            .rows
            .values()
            .filter(|r| r.state.is_running())
            .filter(|r| r.compose_project.as_deref().is_some_and(|p| projects.contains(p)))
            .map(|r| r.id.clone())
            .collect();
        members.sort();
        let wanted = select_wanted(&members, selected, GROUP_STATS_LIMIT);
        let docker = store.docker().cloned();
        self.stats
            .update(cx, |stats, cx| stats.set_wanted(docker.as_ref(), wanted, cx));
    }

    fn select(&mut self, id: Option<String>, pointer: bool, cx: &mut Context<Self>) {
        if self.selected == id {
            return;
        }
        self.selected = id.clone();
        cx.emit(ListEvent::Selected { id, pointer });
        self.sync_stats(cx);
        cx.notify();
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.select(None, true, cx);
    }

    /// Selects a container, and scrolls to it. A collapsed group of the
    /// container expands.
    pub fn reveal(&mut self, id: &str, cx: &mut Context<Self>) {
        let project = self.store.read(cx).set.rows.get(id).and_then(|r| r.compose_project.clone());
        if let Some(project) = project
            && self.collapsed.remove(&project)
        {
            self.rebuild(cx);
        }
        let ix = self
            .items
            .iter()
            .position(|item| matches!(item, ListItem::Row { id: row, .. } if row == id));
        if let Some(ix) = ix {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        self.select(Some(id.to_string()), false, cx);
    }

    fn toggle_group(&mut self, project: &str, cx: &mut Context<Self>) {
        let expanded = self.collapsed.remove(project);
        if !expanded {
            self.collapsed.insert(project.to_string());
        }
        self.rebuild(cx);
        // The services fade in below their group. A collapse is
        // immediate, so the rows below move up with no wait.
        if expanded {
            let rows = &self.store.read(cx).set.rows;
            let services: Vec<String> = self
                .items
                .iter()
                .filter_map(|item| match item {
                    ListItem::Row { id, grouped: true } => Some(id),
                    _ => None,
                })
                .filter(|id| rows.get(*id).and_then(|r| r.compose_project.as_deref()) == Some(project))
                .cloned()
                .collect();
            self.enter(services, Entrance::Expand, cx);
        }
    }

    /// Plays an entrance on `ids`, and removes the mark when it ends.
    fn enter(&mut self, ids: Vec<String>, entrance: Entrance, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        self.entrances += 1;
        let n = self.entrances;
        for id in ids {
            self.entering.insert(id, (entrance, n));
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ENTER_DURATION).await;
            let _ = this.update(cx, |this, _| this.entering.retain(|_, (_, m)| *m != n));
        })
        .detach();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows: Vec<(usize, &String)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(ix, item)| match item {
                ListItem::Row { id, .. } => Some((ix, id)),
                ListItem::Group { .. } => None,
            })
            .collect();
        if rows.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|sel| rows.iter().position(|(_, id)| *id == sel));
        let next = match current {
            None if delta > 0 => 0,
            None => rows.len() - 1,
            Some(i) => (i as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
        };
        let (ix, id) = rows[next];
        let id = id.clone();
        self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        self.select(Some(id), false, cx);
    }

    fn run_on_selected(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else { return };
        self.request_action(id, action, window, cx);
    }

    /// Runs an action. A remove asks for confirmation first, because it
    /// cannot be undone.
    fn request_action(&mut self, id: String, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        if action != Action::Remove {
            store.update(cx, |s, cx| s.run_action(&id, action, cx));
            return;
        }
        let name = store
            .read(cx)
            .set
            .rows
            .get(&id)
            .map(|r| r.name.clone())
            .unwrap_or_default();
        // A sheet on the window, as AppKit shows it. The destructive button
        // is not the default, so Return does not remove the container.
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Remove “{name}”?"),
            Some("The container stops and Docker deletes it. You cannot undo this."),
            &[PromptButton::Cancel("Cancel".into()), PromptButton::new("Remove")],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(1) {
                store.update(cx, |s, cx| s.run_action(&id, Action::Remove, cx));
            }
        })
        .detach();
    }

    /// Removes all the containers of a Compose project after the user
    /// confirms, because a remove cannot be undone.
    fn request_group_remove(&mut self, project: String, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let count = store
            .read(cx)
            .set
            .rows
            .values()
            .filter(|r| r.compose_project.as_deref() == Some(project.as_str()))
            .count();
        let noun = if count == 1 { "container" } else { "containers" };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Remove “{project}”?"),
            Some(&format!(
                "The {count} {noun} of this project stop and Docker deletes them. You cannot undo this."
            )),
            &[PromptButton::Cancel("Cancel".into()), PromptButton::new("Remove All")],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(1) {
                store.update(cx, |s, cx| s.run_group_action(&project, Action::Remove, cx));
            }
        })
        .detach();
    }

    fn render_item(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let item = match &self.items[ix] {
            ListItem::Group {
                project,
                total,
                running,
                failed,
                collapsed,
            } => self.render_group(ix, project, *total, *running, *failed, *collapsed, window, cx),
            ListItem::Row { id, grouped } => self.render_row(ix, id, *grouped, window, cx),
        };
        // A line separates the Compose projects from the single containers.
        let divider = matches!(self.items[ix], ListItem::Row { grouped: false, .. })
            && ix > 0
            && matches!(
                self.items[ix - 1],
                ListItem::Row { grouped: true, .. } | ListItem::Group { .. }
            );
        // The tree lines join the services to their project.
        let next_is_child = matches!(self.items.get(ix + 1), Some(ListItem::Row { grouped: true, .. }));
        let tree = match &self.items[ix] {
            ListItem::Group { .. } if next_is_child => Some(Tree::Stem),
            ListItem::Row { grouped: true, .. } if next_is_child => Some(Tree::Branch),
            ListItem::Row { grouped: true, .. } => Some(Tree::Last),
            _ => None,
        };
        let t = cx.theme();
        div()
            .relative()
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(ROW_INSET))
            .pr(px(ROW_TRAILING_INSET))
            .py_px()
            .when(divider, |d| d.pt(px(4.)).child(divider_line(cx)))
            .when_some(tree, |d, tree| d.children(tree_lines(tree, t.muted_foreground.opacity(0.35))))
            .child(item)
            .map(|d| {
                let entering = match &self.items[ix] {
                    ListItem::Row { id, .. } => self.entering.get(id).map(|e| (id, *e)),
                    ListItem::Group { .. } => None,
                };
                match entering {
                    Some((id, (entrance, n))) => d
                        .with_animation(
                            ElementId::named_usize(format!("enter-{id}"), n),
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

    #[allow(clippy::too_many_arguments)]
    fn render_group(
        &self,
        ix: usize,
        project: &str,
        total: usize,
        running: usize,
        failed: usize,
        collapsed: bool,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = cx.theme();
        let p = project.to_string();
        let group_button = |icon: IconName, name: &'static str, tip: &'static str, action: Action| {
            let p = p.clone();
            Button::new((name, ix))
                .icon(icon)
                .ghost()
                .xsmall()
                .tooltip(tip)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.store
                        .update(cx, |s, cx| s.run_group_action(&p, action, cx));
                }))
        };
        let toggle = p.clone();
        let menu_project = p.clone();
        let members: Vec<_> = self
            .store
            .read(cx)
            .set
            .rows
            .values()
            .filter(|r| r.compose_project.as_deref() == Some(project))
            .collect();
        let compose_file = members.iter().find_map(|r| r.compose_file.clone());
        // The summary keeps the failed containers apart from the clean
        // stops, so a crash shows when the group is collapsed.
        let stopped = total - running - failed;
        let mut summary: Vec<(String, Option<Hsla>)> = vec![(
            format!("{total} {}", if total == 1 { "container" } else { "containers" }),
            None,
        )];
        // When all members run, the count is sufficient.
        if running < total {
            if failed > 0 {
                summary.push((format!("{failed} failed"), Some(t.red)));
            }
            if running > 0 {
                summary.push((format!("{running} running"), None));
            }
            if stopped > 0 {
                let all = running == 0 && failed == 0;
                summary.push((if all { "all stopped".into() } else { format!("{stopped} stopped") }, None));
            }
        }
        // The right column shows the total CPU and memory of the members
        // that run. When no member runs, it shows the last exit.
        let meta: Option<String> = if running > 0 {
            let stats = self.stats.read(cx);
            let samples: Vec<_> = members
                .iter()
                .filter(|r| r.state.is_running())
                .filter_map(|r| stats.latest(&r.id))
                .collect();
            (!samples.is_empty()).then(|| {
                let cpu: f64 = samples.iter().map(|s| s.cpu_percent).sum();
                let mem: u64 = samples.iter().map(|s| s.mem_bytes).sum();
                format!("{cpu:.1}% CPU · {}", crate::docker::stats::format_bytes(mem))
            })
        } else {
            members
                .iter()
                .filter_map(|r| Some((r.exited_ago()?, r.status_parts().1?)))
                .min_by_key(|(ago, _)| *ago)
                .map(|(_, time)| format!("Exited {time}"))
        };
        h_flex()
            .id(("group", ix))
            .group(HEADER_GROUP)
            .size_full()
            .pr_2()
            .gap_3()
            .rounded(px(8.))
            .hover(|s| s.bg(native::hover(cx)))
            .child(
                div().w(px(GUTTER - 12.)).flex().justify_end().child(
                    // The chevron turns to the new state and does not jump. A
                    // spring can reverse in the middle of a turn.
                    Icon::new(IconName::ChevronRight)
                        .xsmall()
                        .text_color(t.muted_foreground)
                        .with_spring(
                            "chevron",
                            SpringAnimation::new(SNAPPY_SPRING).to(if collapsed { 0. } else { 1. }),
                            |icon, turn: f32| {
                                icon.rotate(radians(turn * std::f32::consts::FRAC_PI_2))
                            },
                        ),
                ),
            )
            .child(
                div()
                    .size(px(ICON_SIZE))
                    .flex_shrink_0()
                    .rounded(px(8.))
                    .bg(DOCKER_BLUE)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path(crate::assets::DOCKER_LOGO)
                            .size(px(20.))
                            .text_color(gpui::white()),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(text::BODY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(project.to_string()),
                    )
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1()
                            .text_size(px(text::CALLOUT))
                            .font_features(tabular_nums())
                            .text_color(t.muted_foreground)
                            .when_some(compose_file, |d, path| {
                                d.child(compose_file_link(ix, path, cx)).child("·")
                            })
                            .children(summary.into_iter().enumerate().flat_map(|(i, (part, color))| {
                                let sep = (i > 0).then(|| div().child("·"));
                                let part = div()
                                    .flex_shrink_0()
                                    .when_some(color, |d, c| d.text_color(c))
                                    .child(part);
                                sep.into_iter().chain([part])
                            })),
                    ),
            )
            // The column has the width of the ports column of the service
            // rows, so the values align with the ports below them.
            .when(!self.compact, |d| {
                d.child(
                    div()
                        .w(px(PORTS_WIDTH))
                        .flex_shrink_0()
                        .truncate()
                        .text_size(px(text::CALLOUT))
                        .font_features(tabular_nums())
                        .text_color(t.muted_foreground)
                        .when_some(meta, |d, meta| d.child(meta)),
                )
            })
            .child(
                h_flex()
                    .w(px(ACTIONS_WIDTH))
                    .justify_end()
                    .invisible()
                    .group_hover(HEADER_GROUP, |s| s.visible())
                    .child(group_button(IconName::Play, "g-start", "Start all", Action::Start))
                    .child(group_button(IconName::Square, "g-stop", "Stop all", Action::Stop))
                    .child(group_button(
                        IconName::RotateCcw,
                        "g-restart",
                        "Restart all",
                        Action::Restart,
                    )),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(&toggle, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| this.show_group_menu(&menu_project, window, cx)),
            )
            .into_any_element()
    }

    fn show_group_menu(&mut self, project: &str, window: &mut Window, cx: &mut Context<Self>) {
        let rows = &self.store.read(cx).set.rows;
        let members = || rows.values().filter(|r| r.compose_project.as_deref() == Some(project));
        let any_running = members().any(|r| r.state.is_running());
        let any_stopped = members().any(|r| !r.state.is_running());
        let compose_file = members().find_map(|r| r.compose_file.clone());
        let mut entries = vec![
            (MenuEntry::Item { label: "Start All".into(), enabled: any_stopped }, Some(Action::Start)),
            (MenuEntry::Item { label: "Stop All".into(), enabled: any_running }, Some(Action::Stop)),
            (MenuEntry::item("Restart All"), Some(Action::Restart)),
        ];
        if compose_file.is_some() {
            entries.push((MenuEntry::Separator, None));
            entries.push((MenuEntry::item("Show Compose File in Finder"), None));
        }
        entries.push((MenuEntry::Separator, None));
        entries.push((MenuEntry::item("Remove All…"), Some(Action::Remove)));
        let (entries, actions): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        let project = project.to_string();
        native::show_context_menu(entries, window, cx, move |this, ix, window, cx| match actions[ix] {
            Some(Action::Remove) => this.request_group_remove(project.clone(), window, cx),
            Some(action) => this.store.update(cx, |s, cx| s.run_group_action(&project, action, cx)),
            None => {
                if let Some(path) = &compose_file {
                    cx.reveal_path(std::path::Path::new(path));
                }
            }
        });
    }

    fn show_row_menu(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.store.read(cx).set.rows.get(&id).cloned() else {
            return;
        };
        let running = row.state.is_running();
        let url = row.ports.iter().find_map(|p| p.url());
        type Handler = Box<dyn Fn(&mut ContainerList, &mut Window, &mut Context<ContainerList>)>;
        let mut entries: Vec<(MenuEntry, Option<Handler>)> = Vec::new();
        let action = |action: Action, id: &str| -> Option<Handler> {
            let id = id.to_string();
            Some(Box::new(move |this, window, cx| this.request_action(id.clone(), action, window, cx)))
        };
        if running {
            entries.push((MenuEntry::item("Stop"), action(Action::Stop, &id)));
        } else {
            entries.push((MenuEntry::item("Start"), action(Action::Start, &id)));
        }
        entries.push((MenuEntry::item("Restart"), action(Action::Restart, &id)));
        entries.push((MenuEntry::Separator, None));
        if let Some(url) = url {
            let label = format!("Open {url}");
            entries.push((MenuEntry::item(label), Some(Box::new(move |_, _, cx| cx.open_url(&url)))));
        }
        if let Some(path) = row.compose_file.clone() {
            entries.push((
                MenuEntry::item("Show Compose File in Finder"),
                Some(Box::new(move |_, _, cx| cx.reveal_path(std::path::Path::new(&path)))),
            ));
        }
        let name = row.name.clone();
        entries.push((
            MenuEntry::item("Copy Name"),
            Some(Box::new(move |_, _, cx| toast::copy(name.clone(), name.clone(), cx))),
        ));
        let full_id = row.id.clone();
        entries.push((
            MenuEntry::item("Copy ID"),
            Some(Box::new(move |_, _, cx| toast::copy(full_id.clone(), full_id.clone(), cx))),
        ));
        entries.push((MenuEntry::Separator, None));
        entries.push((MenuEntry::item("Remove…"), action(Action::Remove, &id)));
        let (entries, handlers): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        native::show_context_menu(entries, window, cx, move |this, ix, window, cx| {
            if let Some(Some(handler)) = handlers.get(ix) {
                handler(this, window, cx);
            }
        });
    }

    fn render_row(
        &self,
        ix: usize,
        id: &str,
        grouped: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let store = self.store.read(cx);
        let Some(row) = store.set.rows.get(id) else {
            return div().h(px(ROW_HEIGHT)).into_any_element();
        };
        let t = cx.theme();
        let pending = store.set.pending.get(id).copied();
        let selected = self.selected.as_deref() == Some(id);
        // A selected row in the focused list of the active window has the
        // accent fill and white text, as in the AppKit lists. In other
        // cases, the fill is gray and the text keeps its colors.
        let emphasized = window.is_window_active() && self.focus.contains_focused(window, cx);
        let on_accent = selected && emphasized && native::has_emphasized_selection(cx);
        let secondary = if on_accent { gpui::white().opacity(0.8) } else { t.muted_foreground };
        let running = row.state.is_running();
        let dot = super::state_color(row.state, cx);

        let action_button = |icon: IconName, name: &'static str, tip: &'static str, action: Action| {
            let id = id.to_string();
            Button::new((name, ix))
                .icon(icon)
                .when(selected, |b| b.outline())
                .when(!selected, |b| b.ghost())
                .xsmall()
                .tooltip(tip)
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.request_action(id.clone(), action, window, cx);
                }))
        };

        let actions = match pending {
            Some(action) => h_flex()
                .gap_1()
                .text_size(px(text::CALLOUT))
                .text_color(t.muted_foreground)
                // The label shows at once. The spinner waits, so a fast
                // action does not show it for one frame.
                .child(delayed_reveal(("pending", ix), div().child(spinner().xsmall())))
                .child(action.pending_label())
                .into_any_element(),
            None => h_flex()
                .gap_1()
                // The actions show on hover and on the selected row only.
                .when(!selected, |d| d.invisible().group_hover(ROW_GROUP, |s| s.visible()))
                .map(|d| {
                    if running {
                        d.child(action_button(IconName::Square, "stop", "Stop (⌘.)", Action::Stop))
                    } else {
                        d.child(action_button(IconName::Play, "start", "Start (⌘↩)", Action::Start))
                    }
                })
                .child(action_button(IconName::Trash, "remove", "Remove (⌘⌫)", Action::Remove))
                .into_any_element(),
        };

        let name = if grouped {
            row.compose_service.clone().unwrap_or_else(|| row.name.clone())
        } else {
            row.name.clone()
        };
        let failed = row.failed_in(store.set.rows.values());
        // A failed container shows why and when. The others show the image.
        let status = match row.status_parts() {
            _ if running => row.status.clone(),
            (head, Some(time)) => format!("{head} · {time}"),
            (head, None) => head,
        };
        let subtitle = if failed { status } else { format!("{} · {status}", row.image) };
        let row_id = id.to_string();
        h_flex()
            .id(("row", ix))
            .group(ROW_GROUP)
            .h_full()
            .flex_1()
            .map(|d| {
                if grouped {
                    d.ml(px(CHILD_INSET - ROW_INSET)).pl(px(12.))
                } else {
                    d.pl(px(GUTTER))
                }
            })
            .pr_2()
            .gap_3()
            .text_size(px(text::BODY))
            .rounded(px(8.))
            .map(|d| {
                if selected {
                    d.bg(native::selection(emphasized, cx))
                } else {
                    d.hover(|s| s.bg(native::hover(cx)))
                }
            })
            .when(on_accent, |d| d.text_color(gpui::white()))
            .when(pending.is_some(), |d| d.opacity(0.7))
            .child(
                super::image_icon::ContainerIcon::new(
                    SharedString::from(format!("icon-{id}")),
                    &row.image,
                    row.icon.as_deref(),
                    dot,
                    running,
                )
                    .size(px(ICON_SIZE)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(if self.compact { 0. } else { NAME_MIN_WIDTH }))
                    .overflow_hidden()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .when(!running, |d| d.text_color(secondary))
                            .child(name),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(text::CALLOUT))
                            .text_color(if failed && !on_accent { t.red } else { secondary })
                            .child(subtitle),
                    ),
            )
            // The Info tab of the detail pane also shows the ports.
            .when(!self.compact, |d| {
                d.child(
                    div()
                        .w(px(PORTS_WIDTH))
                        .flex_shrink_0()
                        .when_some(row.ports.first(), |d, port| {
                            d.child(port_link(("port", ix), port, row.ports.len() - 1, on_accent, cx))
                        }),
                )
            })
            .child(h_flex().w(px(ACTIONS_WIDTH)).flex_shrink_0().justify_end().child(actions))
            .on_click({
                let row_id = row_id.clone();
                cx.listener(move |this, _, window, cx| {
                    this.focus.focus(window, cx);
                    this.select(Some(row_id.clone()), true, cx);
                })
            })
            // A right click selects the row, then shows its actions.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    this.focus.focus(window, cx);
                    this.select(Some(row_id.clone()), true, cx);
                    this.show_row_menu(row_id.clone(), window, cx);
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

/// The color of the Docker logo.
pub(super) const DOCKER_BLUE: Hsla = Hsla {
    h: 206. / 360.,
    s: 0.93,
    l: 0.52,
    a: 1.,
};

#[derive(Clone, Copy)]
pub(super) enum Tree {
    /// A group that has services below it.
    Stem,
    /// A service that has more services below it.
    Branch,
    /// The last service of a group.
    Last,
}

/// The line that separates the groups from the single rows.
pub(super) fn divider_line(cx: &App) -> Div {
    div()
        .absolute()
        .top_0()
        .left(px(ROW_INSET + 10.))
        .right(px(ROW_TRAILING_INSET + 10.))
        .h_px()
        .bg(cx.theme().border)
}

/// The tree lines that join the rows of a group to the group row.
pub(super) fn tree_lines(tree: Tree, line: Hsla) -> Vec<Div> {
    let mid = ROW_HEIGHT / 2.;
    // The line from the bottom of the group tile to the next row.
    let stem = || {
        div()
            .absolute()
            .left(px(TREE_X))
            .top(px((ROW_HEIGHT + ICON_SIZE) / 2.))
            .bottom_0()
            .w_px()
            .bg(line)
    };
    // A rounded elbow into the icon of the row.
    let elbow = || {
        div()
            .absolute()
            .left(px(TREE_X))
            .top_0()
            .w(px(CHILD_INSET + 6. - TREE_X))
            .h(px(mid))
            .border_l_1()
            .border_b_1()
            .rounded_bl(px(6.))
            .border_color(line)
    };
    // The line that continues to the rows below.
    let rest = || div().absolute().left(px(TREE_X)).top(px(mid)).bottom_0().w_px().bg(line);
    match tree {
        Tree::Stem => vec![stem()],
        Tree::Branch => vec![elbow(), rest()],
        Tree::Last => vec![elbow()],
    }
}

/// The name of the main Compose file. A click shows the file in Finder.
fn compose_file_link(ix: usize, path: String, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    let tip = path.clone();
    h_flex()
        .id(("compose-file", ix))
        .flex_shrink_0()
        .gap_1()
        .cursor_pointer()
        .hover(|d| d.text_color(t.foreground))
        .child(Icon::new(IconName::FileCode).xsmall())
        .child(name)
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(format!("Show {tip} in Finder")).build(window, cx)
        })
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            cx.reveal_path(std::path::Path::new(&path));
        })
}

/// A published port as a link that opens in the browser. `more` is the
/// number of other ports, which show as "+N".
pub fn port_link(
    id: impl Into<ElementId>,
    port: &crate::docker::containers::Port,
    more: usize,
    on_accent: bool,
    cx: &App,
) -> impl IntoElement {
    let t = cx.theme();
    let url = port.url();
    // On the accent fill of a selected row, the link is white.
    let (color, secondary) = if on_accent {
        (gpui::white(), gpui::white().opacity(0.8))
    } else {
        (t.link, t.muted_foreground)
    };
    h_flex()
        .id(id)
        .gap_1()
        .text_size(px(text::BODY))
        .text_color(color)
        .child(Icon::new(IconName::Link).xsmall())
        .child(port.host())
        .when(more > 0, |d| {
            d.child(div().text_size(px(text::CALLOUT)).text_color(secondary).child(format!("+{more}")))
        })
        .when_some(url, |d, url| {
            d.cursor_pointer()
                .hover(|d| d.underline())
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(format!("Open {url}")).build(window, cx)
                })
                .on_click({
                    let url = port.url().unwrap_or_default();
                    move |_, _, cx| {
                        cx.stop_propagation();
                        cx.open_url(&url);
                    }
                })
        })
}

impl Render for ContainerList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let store = self.store.read(cx);
        let total = store.set.rows.len();
        let running = store.set.rows.values().filter(|r| r.state.is_running()).count();
        let stopped = total - running;
        let loading = store.is_loading();
        let count = self.items.len();

        let body = if count == 0 {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(t.muted_foreground)
                .child(if loading {
                    delayed_reveal("list-loading", div().child(spinner())).into_any_element()
                } else if total == 0 {
                    div().child("No containers").into_any_element()
                } else if self.running_only && running == 0 {
                    div().child("No containers run").into_any_element()
                } else {
                    div().child("No containers match the search").into_any_element()
                })
                .into_any_element()
        } else {
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    uniform_list(
                        "containers",
                        count,
                        cx.processor(|this, range: Range<usize>, window, cx| {
                            range
                                .map(|ix| this.render_item(ix, window, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .size_full()
                    .track_scroll(&self.scroll),
                )
                .child(Scrollbar::vertical(&self.scroll))
                .into_any_element()
        };

        v_flex()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|this, _: &StartSelected, w, cx| {
                this.run_on_selected(Action::Start, w, cx)
            }))
            .on_action(cx.listener(|this, _: &StopSelected, w, cx| {
                this.run_on_selected(Action::Stop, w, cx)
            }))
            .on_action(cx.listener(|this, _: &RestartSelected, w, cx| {
                this.run_on_selected(Action::Restart, w, cx)
            }))
            .on_action(cx.listener(|this, _: &RemoveSelected, w, cx| {
                this.run_on_selected(Action::Remove, w, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| {
                this.select(None, false, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleRunningOnly, _, cx| {
                this.running_only = !this.running_only;
                this.rebuild(cx);
            }))
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
                                    .child("Containers"),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(text::CALLOUT))
                                    .font_features(tabular_nums())
                                    .text_color(t.muted_foreground)
                                    .child(if self.running_only && stopped > 0 {
                                        format!("{running} running · {stopped} stopped (hidden)")
                                    } else {
                                        format!("{running} running · {stopped} stopped")
                                    }),
                            ),
                    )
                    .child(drag_area("list-drag").flex_1().h_full())
                    .when(self.search_open, |d| {
                        d.child(
                            // The field gets narrower before the buttons
                            // go out of the toolbar.
                            div().w(px(220.)).min_w(px(120.)).mr_1().child(
                                Input::new(&self.search)
                                    .small()
                                    .prefix(Icon::new(IconName::Search).small())
                                    .cleanable(true),
                            ),
                        )
                    })
                    .child(
                        Button::new("running-only")
                            .icon(Icon::new(IconName::ListFilter))
                            .custom(native::toolbar_button(cx))
                            .small()
                            .selected(self.running_only)
                            // The tooltip names the filter, and the selected
                            // state shows if the filter is on. With no
                            // stopped containers, the filter hides nothing.
                            .tooltip(if stopped == 0 {
                                "Show running containers only (⇧⌘R). No containers are stopped."
                            } else {
                                "Show running containers only (⇧⌘R)"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.running_only = !this.running_only;
                                this.rebuild(cx);
                            })),
                    )
                    .child(
                        Button::new("toggle-search")
                            .icon(Icon::new(IconName::Search))
                            .custom(native::toolbar_button(cx))
                            .small()
                            .selected(self.search_open)
                            .tooltip("Search (⌘F)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_search(window, cx)
                            })),
                    ),
            )
            .child(div().h_2().flex_shrink_0())
            .child(body)
    }
}

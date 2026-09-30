//! The grouped, virtualized container list.

use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::Scrollbar;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::containers::Action;
use crate::docker::stats::format_bytes;
use crate::state::container_store::{ContainerStore, ListItem, StoreEvent, build_list};
use crate::state::stats_store::{MAX_VISIBLE_STREAMS, StatsStore, select_wanted};
use crate::ui::{SNAPPY_SPRING, TOOLBAR_HEIGHT, delayed_reveal, drag_area, tabular_nums};

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
        ClearSelection
    ]
);

const CONTEXT: &str = "ContainerList";
const ROW_HEIGHT: f32 = 44.;
const NAME_MIN_WIDTH: f32 = 170.;
/// The space between the list edge and the rounded row background.
const ROW_INSET: f32 = 8.;
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
    ]);
}

pub enum ListEvent {
    /// `pointer` is true when a click made the selection, and false for a key.
    Selected { id: Option<String>, pointer: bool },
}

pub struct ContainerList {
    store: Entity<ContainerStore>,
    stats: Entity<StatsStore>,
    search: Entity<InputState>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    collapsed: HashSet<String>,
    items: Rc<Vec<ListItem>>,
    selected: Option<String>,
    visible: Range<usize>,
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
            // The stats store notifies at most one time each second.
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
            selected: None,
            visible: 0..0,
            _subs: subs,
        };
        this.rebuild(cx);
        this
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let filter = self.search.read(cx).value();
        let store = self.store.read(cx);
        self.items = Rc::new(build_list(store.set.rows.values(), &filter, &self.collapsed));
        // Clear a selection that points to a removed container.
        if let Some(sel) = &self.selected
            && !store.set.rows.contains_key(sel) {
                self.select(None, false, cx);
            }
        self.sync_stats(cx);
        cx.notify();
    }

    /// Opens stats streams for the visible running rows and the selection.
    fn sync_stats(&mut self, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        // The list can be shorter than the last visible range.
        let end = self.visible.end.min(self.items.len());
        let start = self.visible.start.min(end);
        let visible: Vec<String> = self.items[start..end]
            .iter()
            .filter_map(|item| match item {
                ListItem::Row { id, .. } => Some(id),
                ListItem::Group { .. } => None,
            })
            .filter(|id| store.set.rows.get(*id).is_some_and(|r| r.state.is_running()))
            .cloned()
            .collect();
        let selected = self
            .selected
            .as_deref()
            .filter(|id| store.set.rows.get(*id).is_some_and(|r| r.state.is_running()));
        let wanted = select_wanted(&visible, selected, MAX_VISIBLE_STREAMS);
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

    fn toggle_group(&mut self, project: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(project) {
            self.collapsed.insert(project.to_string());
        }
        self.rebuild(cx);
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
        window.open_alert_dialog(cx, move |alert, _, _| {
            let store = store.clone();
            let id = id.clone();
            alert
                .title(format!("Remove {name}?"))
                .description("The container stops and Docker deletes it. You cannot undo this.")
                .ok_text("Remove")
                .ok_variant(gpui_component::button::ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, _, cx| {
                    store.update(cx, |s, cx| s.run_action(&id, Action::Remove, cx));
                    true
                })
        });
    }

    fn render_header(&self, cx: &App) -> impl IntoElement {
        let t = cx.theme();
        h_flex()
            .h(px(28.))
            .px(px(ROW_INSET + 10.))
            .gap_2()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.muted_foreground)
            .child(div().flex_1().min_w(px(NAME_MIN_WIDTH)).pl(px(36.)).child("Name"))
            .child(div().w(px(200.)).child("Image"))
            .child(div().w(px(130.)).child("Ports"))
            .child(div().w(px(56.)).text_right().child("CPU"))
            .child(div().w(px(76.)).text_right().child("Memory"))
            .child(div().w(px(92.)))
    }

    fn render_item(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let item = match &self.items[ix] {
            ListItem::Group {
                project,
                total,
                running,
                collapsed,
            } => self.render_group(ix, project, *total, *running, *collapsed, window, cx),
            ListItem::Row { id, grouped } => self.render_row(ix, id, *grouped, cx),
        };
        div()
            .w_full()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_INSET))
            .py_px()
            .child(item)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_group(
        &self,
        ix: usize,
        project: &str,
        total: usize,
        running: usize,
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
        h_flex()
            .id(("group", ix))
            .group(HEADER_GROUP)
            .size_full()
            .px_2p5()
            .gap_2()
            .rounded(px(8.))
            .hover(|d| d.bg(t.table_hover))
            .active(|d| d.bg(t.table_active.opacity(0.6)))
            .child({
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
                    )
            })
            .child(Icon::new(IconName::Boxes).small().text_color(t.muted_foreground))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(project.to_string()),
            )
            .child(
                div()
                    .text_xs()
                    .font_features(tabular_nums())
                    .text_color(t.muted_foreground)
                    .child(format!("{running} of {total} running")),
            )
            .child(
                h_flex()
                    .w(px(92.))
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
            .into_any_element()
    }

    fn render_row(&self, ix: usize, id: &str, grouped: bool, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let Some(row) = store.set.rows.get(id) else {
            return div().h(px(ROW_HEIGHT)).into_any_element();
        };
        let t = cx.theme();
        let pending = store.set.pending.get(id).copied();
        let selected = self.selected.as_deref() == Some(id);
        let sample = self.stats.read(cx).latest(id).copied();
        let running = row.state.is_running();
        let dot = super::state_color(row.state, cx);

        let action_button = |icon: IconName, name: &'static str, tip: &'static str, action: Action| {
            let id = id.to_string();
            Button::new((name, ix))
                .icon(icon)
                .ghost()
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
                .text_xs()
                .text_color(t.muted_foreground)
                .child(Spinner::new().xsmall())
                .child(action.pending_label())
                .into_any_element(),
            None => h_flex()
                // The actions show on hover and on the selected row only.
                .when(!selected, |d| d.invisible().group_hover(ROW_GROUP, |s| s.visible()))
                .when(running, |d| {
                    d.child(action_button(IconName::Square, "stop", "Stop (⌘.)", Action::Stop))
                        .child(action_button(
                            IconName::RotateCcw,
                            "restart",
                            "Restart (⌘R)",
                            Action::Restart,
                        ))
                })
                .when(!running, |d| {
                    d.child(action_button(IconName::Play, "start", "Start (⌘↩)", Action::Start))
                })
                .child(action_button(IconName::Trash, "remove", "Remove (⌘⌫)", Action::Remove))
                .into_any_element(),
        };

        let name = if grouped {
            row.compose_service.clone().unwrap_or_else(|| row.name.clone())
        } else {
            row.name.clone()
        };
        let row_id = id.to_string();
        h_flex()
            .id(("row", ix))
            .group(ROW_GROUP)
            .size_full()
            .px_2p5()
            .gap_2()
            .text_sm()
            .rounded(px(8.))
            .when(selected, |d| d.bg(t.table_active))
            .when(!selected, |d| {
                d.hover(|d| d.bg(t.table_hover))
                    .active(|d| d.bg(t.table_active.opacity(0.6)))
            })
            .when(pending.is_some(), |d| d.opacity(0.7))
            // One flexible column for the indent, the icon, and the name. All
            // rows then have the same width here, so the columns align.
            .child(
                h_flex()
                    .flex_1()
                    .min_w(px(NAME_MIN_WIDTH))
                    .gap_2()
                    .when(grouped, |d| d.pl_4())
                    .child(super::image_icon::ContainerIcon::new(&row.image, row.icon.as_deref(), dot, running))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(div().truncate().font_weight(FontWeight::MEDIUM).child(name))
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(t.muted_foreground)
                                    .child(row.status.clone()),
                            ),
                    ),
            )
            .child(
                div()
                    .w(px(200.))
                    .truncate()
                    .text_color(t.muted_foreground)
                    .child(row.image.clone()),
            )
            .child(
                div()
                    .w(px(130.))
                    .truncate()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(row.ports.clone()),
            )
            .child(
                div()
                    .w(px(56.))
                    .text_right()
                    .text_xs()
                    .font_features(tabular_nums())
                    .child(match (running, sample) {
                        (true, Some(s)) => format!("{:.1}%", s.cpu_percent),
                        _ => "–".into(),
                    }),
            )
            .child(
                div()
                    .w(px(76.))
                    .text_right()
                    .text_xs()
                    .font_features(tabular_nums())
                    .child(match (running, sample) {
                        (true, Some(s)) => format_bytes(s.mem_bytes),
                        _ => "–".into(),
                    }),
            )
            .child(h_flex().w(px(92.)).justify_end().child(actions))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus.focus(window, cx);
                this.select(Some(row_id.clone()), true, cx);
            }))
            .into_any_element()
    }
}

impl Render for ContainerList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let store = self.store.read(cx);
        let total = store.set.rows.len();
        let running = store.set.rows.values().filter(|r| r.state.is_running()).count();
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
                    delayed_reveal("list-loading", div().child(Spinner::new())).into_any_element()
                } else if total == 0 {
                    div().child("No containers").into_any_element()
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
                            if range != this.visible {
                                this.visible = range.clone();
                                // Change the streams after this frame, not in it.
                                let weak = cx.weak_entity();
                                window.defer(cx, move |_, cx| {
                                    let _ = weak.update(cx, |this, cx| this.sync_stats(cx));
                                });
                            }
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
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.search.update(cx, |s, cx| s.focus(window, cx));
            }))
            .child(
                h_flex()
                    .h(px(TOOLBAR_HEIGHT))
                    .flex_shrink_0()
                    .px_4()
                    .gap_3()
                    .child(
                        h_flex()
                            .items_baseline()
                            .gap_2()
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Containers"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_features(tabular_nums())
                                    .text_color(t.muted_foreground)
                                    .child(format!("{running} of {total} running")),
                            ),
                    )
                    .child(drag_area("list-drag").flex_1().h_full())
                    .child(
                        div().w(px(240.)).child(
                            Input::new(&self.search)
                                .small()
                                .prefix(Icon::new(IconName::Search).small())
                                .cleanable(true),
                        ),
                    ),
            )
            .child(self.render_header(cx))
            .child(body)
    }
}

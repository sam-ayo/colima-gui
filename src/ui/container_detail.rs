//! The detail pane: a summary, logs, stats, and inspect data for one
//! container.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::containers::{self, Action, Details, Mount, Port};
use crate::docker::stats::format_bytes;
use crate::docker::{ContainerRow, discover_socket};
use crate::runtime::Backend;
use crate::state::container_store::{ContainerStore, StoreEvent};
use crate::state::stats_store::StatsStore;
use crate::ui::json_view::{self, Json};
use crate::ui::key_nav::{self, DetailKeys, KeyNav, Move};
use crate::ui::logs_view::LogsView;
use crate::ui::stats_view::StatsView;
use crate::ui::{
    ENTER_DURATION, NATIVE_BUTTON_HEIGHT, SNAPPY_SPRING, TOOLBAR_HEIGHT, delayed_reveal,
    drag_area, native, spinner, tabular_nums, text,
};

/// The width of each tab segment. All segments have the same width, so the
/// selected fill can move from one segment to the next.
const TAB_WIDTH: f32 = 68.;
/// The space around and between the tab segments.
const TAB_GAP: f32 = 2.;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailTab {
    #[default]
    Info,
    Logs,
    Stats,
    Inspect,
}

impl DetailTab {
    const ALL: [DetailTab; 4] = [
        DetailTab::Info,
        DetailTab::Logs,
        DetailTab::Stats,
        DetailTab::Inspect,
    ];

    fn label(self) -> &'static str {
        match self {
            DetailTab::Info => "Info",
            DetailTab::Logs => "Logs",
            DetailTab::Stats => "Stats",
            DetailTab::Inspect => "Inspect",
        }
    }
}

pub enum DetailEvent {
    Close,
    Error(String),
    /// Show a volume in the volume list.
    RevealVolume(String),
    /// Show an image in the image list. The value is the image ID.
    RevealImage(String),
}

struct Loaded {
    ip: Option<String>,
    mounts: Vec<Mount>,
    /// For each mount: `Some(true)` for a folder on the Mac, `Some(false)`
    /// for a file on the Mac, `None` for a volume or a path in the VM only.
    on_mac: Vec<Option<bool>>,
    json: Rc<Json>,
}

enum Inspect {
    Loading,
    Loaded(Rc<Loaded>),
    Failed(String),
}

pub struct ContainerDetail {
    id: String,
    store: Entity<ContainerStore>,
    stats: Entity<StatsStore>,
    tab: DetailTab,
    /// The number of tab changes. Each change fades in the new tab body.
    tab_changes: usize,
    logs: Option<Entity<LogsView>>,
    inspect: Inspect,
    inspect_list: ListState,
    /// The last row that this pane saw. Used to find changes.
    row: Option<ContainerRow>,
    nav: KeyNav,
    _subs: Vec<Subscription>,
}

impl EventEmitter<DetailEvent> for ContainerDetail {}

impl Focusable for ContainerDetail {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.nav.focus.clone()
    }
}

impl DetailKeys for ContainerDetail {
    fn nav(&self) -> &KeyNav {
        &self.nav
    }

    fn tab_position(&self) -> (usize, usize) {
        let ix = DetailTab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0);
        (ix, DetailTab::ALL.len())
    }

    fn show_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.set_tab_at(ix, window, cx);
    }

    fn key_move(&mut self, m: Move, cx: &mut Context<Self>) {
        match self.tab {
            DetailTab::Info => match m {
                Move::By(d) if d.abs() >= key_nav::PAGE_ROWS => self.nav.scroll_page(d.signum() as f32),
                _ => self.nav.move_cursor(m),
            },
            DetailTab::Logs => {
                if let Some(logs) = &self.logs {
                    logs.update(cx, |l, cx| l.key_scroll(m, cx));
                }
            }
            DetailTab::Stats => {}
            DetailTab::Inspect => key_nav::scroll_lines(&self.inspect_list, m, false),
        }
    }
}

impl ContainerDetail {
    pub fn new(
        id: String,
        store: Entity<ContainerStore>,
        stats: Entity<StatsStore>,
        tab: DetailTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subs = vec![
            cx.subscribe(&store, |this, _, ev: &StoreEvent, cx| {
                if let StoreEvent::Changed = ev {
                    let row = this.store.read(cx).set.rows.get(&this.id).cloned();
                    if row != this.row {
                        this.row = row;
                        // The inspect data is out of date after a change.
                        this.load_inspect(cx);
                    }
                    cx.notify();
                }
            }),
            cx.observe(&stats, |this, _, cx| {
                if matches!(this.tab, DetailTab::Info | DetailTab::Stats) {
                    cx.notify();
                }
            }),
        ];
        let mut this = ContainerDetail {
            row: store.read(cx).set.rows.get(&id).cloned(),
            id,
            store,
            stats,
            tab: DetailTab::Info,
            tab_changes: 0,
            logs: None,
            inspect: Inspect::Loading,
            inspect_list: ListState::new(0, ListAlignment::Top, px(200.)),
            nav: KeyNav::new(cx),
            _subs: subs,
        };
        this.load_inspect(cx);
        this.set_tab(tab, window, cx);
        // The pane has its own entrance, so the first tab does not fade.
        this.tab_changes = 0;
        this
    }

    pub fn tab(&self) -> DetailTab {
        self.tab
    }

    fn open_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.logs.is_some() {
            return;
        }
        let Some(docker) = self.store.read(cx).docker().cloned() else {
            return;
        };
        let id = self.id.clone();
        self.logs = Some(cx.new(|cx| LogsView::new(&docker, &id, window, cx)));
    }

    /// Opens the logs again. A log stream ends when the container stops.
    fn reopen_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.logs = None;
        self.open_logs(window, cx);
        cx.notify();
    }

    fn load_inspect(&mut self, cx: &mut Context<Self>) {
        let Some(docker) = self.store.read(cx).docker().cloned() else {
            return;
        };
        // Keep the old data on screen until the new data arrives.
        if !matches!(self.inspect, Inspect::Loaded(_)) {
            self.inspect = Inspect::Loading;
        }
        let id = self.id.clone();
        let rx = Backend::global(cx).spawn(async move { containers::inspect(&docker, &id).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await;
            let _ = this.update(cx, |this, cx| {
                this.inspect = match result {
                    Ok(Ok(Details { ip, mounts, json })) => {
                        let json = Json::new(json);
                        this.sync_inspect_list(json.lines.len());
                        // Colima shares the home folder at the same path, so
                        // a bind mount source is often also a path on the Mac.
                        let on_mac = mounts
                            .iter()
                            .map(|m| {
                                let meta = (!m.volume).then(|| std::fs::metadata(&m.source).ok()).flatten();
                                meta.map(|meta| meta.is_dir())
                            })
                            .collect();
                        Inspect::Loaded(Rc::new(Loaded { ip, mounts, on_mac, json: Rc::new(json) }))
                    }
                    Ok(Err(e)) => Inspect::Failed(e.to_string()),
                    Err(_) => Inspect::Failed("cancelled".into()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Gives the inspect list a new line count. A reload keeps the scroll
    /// position, because the data changes each time the status changes.
    fn sync_inspect_list(&mut self, count: usize) {
        let list = &self.inspect_list;
        let old = list.item_count();
        if !matches!(self.inspect, Inspect::Loaded(_)) {
            list.reset_with_uniform_height(count, px(json_view::LINE_HEIGHT));
            return;
        }
        if count > old {
            list.splice(old..old, count - old);
        } else if count < old {
            list.splice(count..old, 0);
        }
        list.remeasure();
    }

    /// Shows the tab at a position, from 0. A position with no tab does
    /// nothing.
    pub fn set_tab_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(&tab) = DetailTab::ALL.get(ix) {
            self.set_tab(tab, window, cx);
        }
    }

    /// Shows the next tab, or the previous tab for a negative `delta`.
    pub fn cycle_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = DetailTab::ALL.len() as isize;
        let ix = DetailTab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0) as isize;
        self.set_tab(DetailTab::ALL[(ix + delta).rem_euclid(count) as usize], window, cx);
    }

    /// Takes the focus from the list. The cursor goes to the first row.
    pub fn focus_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.nav.focus.focus(window, cx);
        self.nav.start();
        cx.notify();
    }

    fn set_tab(&mut self, tab: DetailTab, window: &mut Window, cx: &mut Context<Self>) {
        if tab != self.tab {
            self.tab_changes += 1;
            self.nav.reset();
        }
        self.tab = tab;
        if tab == DetailTab::Logs {
            self.open_logs(window, cx);
        }
        cx.notify();
    }

    fn open_terminal(&mut self, cx: &mut Context<Self>) {
        let id = self.id.clone();
        let socket = self.store.read(cx).socket().map(str::to_string);
        let rx = Backend::global(cx).spawn(async move {
            tokio::task::spawn_blocking(move || {
                let socket = socket.unwrap_or_else(discover_socket);
                crate::terminal::open_shell(&id, &socket)
            })
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r)
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Err(e)) = rx.await {
                let _ = this.update(cx, |_, cx| cx.emit(DetailEvent::Error(e.to_string())));
            }
        })
        .detach();
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let labels: Vec<&'static str> = DetailTab::ALL.iter().map(|t| t.label()).collect();
        let selected = DetailTab::ALL.iter().position(|&tab| tab == self.tab).unwrap_or(0);
        let view = cx.weak_entity();
        segmented_control(
            "detail-tabs",
            labels,
            selected,
            Rc::new(move |ix, window, cx| {
                let _ = view.update(cx, |this, cx| this.set_tab(DetailTab::ALL[ix], window, cx));
            }),
            cx,
        )
    }

    fn render_inspect(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        match &self.inspect {
            Inspect::Loading => loading("inspect-loading"),
            Inspect::Failed(e) => div()
                .p_3()
                .text_color(t.red)
                .child(e.clone())
                .into_any_element(),
            Inspect::Loaded(data) => json_view::render(&data.json, &self.inspect_list, cx),
        }
    }

    fn render_info(&self, row: &ContainerRow, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let running = row.state.is_running();
        let sample = self.stats.read(cx).latest(&self.id).copied().filter(|_| running);
        let loaded = match &self.inspect {
            Inspect::Loaded(data) => Some(data.clone()),
            _ => None,
        };
        let mono = t.mono_font_family.clone();
        let muted = t.muted_foreground;

        let image_id = row.image_id.clone();
        // A click on the image shows it in the image list.
        let image = div().child(
            self.nav.row(
                field(
                    "Image",
                    h_flex()
                        .gap_1()
                        .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).child(row.image.clone()))
                        .child(Icon::new(IconName::ChevronRight).xsmall().text_color(muted)),
                    cx,
                )
                .id("show-image")
                .cursor_pointer()
                .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Show in Images").build(window, cx)),
                key_nav::handler(cx, move |_, _, cx| cx.emit(DetailEvent::RevealImage(image_id.clone()))),
                cx,
            ),
        );
        let summary = card(
            [
                field(
                "Container ID",
                div().font_family(mono.clone()).child(row.short_id().to_string()),
                cx,
            ),
            image,
            field(
                "IP address",
                div().font_family(mono.clone()).child(
                    loaded
                        .as_ref()
                        .and_then(|d| d.ip.clone())
                        .unwrap_or_else(|| "–".into()),
                ),
                cx,
            ),
            field(
                "CPU",
                meter(
                    sample.map(|s| s.cpu_percent as f32 / 100.),
                    sample.map_or("–".into(), |s| format!("{:.1}%", s.cpu_percent)),
                    cx,
                ),
                cx,
            ),
            field(
                "Memory",
                meter(
                    sample
                        .filter(|s| s.mem_limit > 0)
                        .map(|s| s.mem_bytes as f32 / s.mem_limit as f32),
                    sample.map_or("–".into(), |s| format_bytes(s.mem_bytes)),
                    cx,
                ),
                cx,
            ),
            ],
            cx,
        );

        let ports = (!row.ports.is_empty()).then(|| {
            section("Ports", cx).child(card(
                row.ports.iter().enumerate().map(|(ix, port)| {
                    // A port with a URL opens in the browser on Return.
                    let row = port_row(ix, port, cx);
                    match port.url() {
                        Some(url) => div().child(self.nav.row(
                            row.id(("port-row", ix)),
                            Rc::new(move |_, cx| cx.open_url(&url)),
                            cx,
                        )),
                        None => row,
                    }
                }),
                cx,
            ))
        });

        let home = std::env::var("HOME").unwrap_or_default();
        let mounts = loaded.as_ref().filter(|d| !d.mounts.is_empty()).map(|d| {
            section("Mounts", cx).child(card(d.mounts.iter().enumerate().map(|(ix, m)| {
                let source = match m.source.strip_prefix(&home) {
                    Some(rest) if !m.volume && !home.is_empty() => format!("~{rest}"),
                    _ => m.source.clone(),
                };
                let volume = m.volume.then(|| m.source.clone());
                let on_mac = d.on_mac.get(ix).copied().flatten();
                let row = list_row()
                    .id(("mount", ix))
                    .gap_2p5()
                    .child(
                        Icon::new(if m.volume { IconName::Database } else { IconName::Folder })
                            .small()
                            .text_color(t.blue),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(mono.clone())
                            .child(source),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .max_w(relative(0.45))
                            .truncate()
                            .font_family(mono.clone())
                            .text_color(muted)
                            .child(m.destination.clone()),
                    );
                // A volume opens in the volume list. A folder on the Mac
                // opens in Finder, and Finder shows a file on the Mac.
                let row = match (volume, on_mac) {
                    (Some(name), _) => self.nav.row(
                        row.cursor_pointer()
                            .child(Icon::new(IconName::ChevronRight).xsmall().text_color(muted))
                            .tooltip(|window, cx| {
                                gpui_component::tooltip::Tooltip::new("Show in Volumes").build(window, cx)
                            }),
                        key_nav::handler(cx, move |_, _, cx| cx.emit(DetailEvent::RevealVolume(name.clone()))),
                        cx,
                    ),
                    (None, Some(folder)) => {
                        let path = std::path::PathBuf::from(&m.source);
                        self.nav.row(
                            row.cursor_pointer()
                                .child(Icon::new(IconName::ChevronRight).xsmall().text_color(muted))
                                .tooltip(move |window, cx| {
                                    let label = if folder { "Open in Finder" } else { "Show in Finder" };
                                    gpui_component::tooltip::Tooltip::new(label).build(window, cx)
                                }),
                            Rc::new(move |_, cx| {
                                if folder {
                                    cx.open_with_system(&path)
                                } else {
                                    cx.reveal_path(&path)
                                }
                            }),
                            cx,
                        )
                    }
                    (None, None) => row,
                };
                // `card` takes plain rows.
                div().child(row)
            }), cx))
        });

        let restart = if running {
            (IconName::RotateCcw, "Restart", Action::Restart)
        } else {
            (IconName::Play, "Start", Action::Start)
        };
        let pending = self.store.read(cx).set.pending.contains_key(&self.id);
        let buttons = h_flex()
            .gap_3()
            .pt_1()
            .child(
                self.nav
                    .button(
                        Button::new("open-terminal")
                            .outline()
                            .small()
                            .h(px(NATIVE_BUTTON_HEIGHT))
                            .w_full()
                            .icon(Icon::new(IconName::SquareTerminal))
                            .label("Open Terminal")
                            .disabled(!running)
                            .tooltip("Open a shell in this container in the Terminal app"),
                        running,
                        key_nav::handler(cx, |this, _, cx| this.open_terminal(cx)),
                        cx,
                    )
                    .flex_1(),
            )
            .child(
                self.nav
                    .button(
                        Button::new("restart")
                            .outline()
                            .small()
                            .h(px(NATIVE_BUTTON_HEIGHT))
                            .w_full()
                            .icon(Icon::new(restart.0))
                            .when(pending, |b| b.icon(spinner()))
                            .label(restart.1)
                            .loading(pending),
                        !pending,
                        key_nav::handler(cx, move |this, _, cx| {
                            let id = this.id.clone();
                            this.store.update(cx, |s, cx| s.run_action(&id, restart.2, cx));
                        }),
                        cx,
                    )
                    .flex_1(),
            );

        // The content is in an inner column, so a long list scrolls and
        // the cards do not shrink to the height of the pane.
        div()
            .id("info")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.nav.scroll)
            .child(
                v_flex()
                    .px_5()
                    .pb_5()
                    .gap_5()
                    .text_size(px(text::BODY))
                    .child(summary)
                    .children(ports)
                    .children(mounts)
                    .child(buttons),
            )
            .into_any_element()
    }
}

/// Gets the index of the segment that the user clicks.
pub(super) type SegmentHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// A segmented control in the style of AppKit. The selected segment has
/// a raised fill, a border, and a shadow, so it is clear in the two themes.
///
/// The selected fill moves to the segment that the user clicks. The fill
/// clips a copy of the labels in the selected style, so the label colors
/// change at the edge of the fill, not with a separate fade.
pub(super) fn segmented_control(
    id: &'static str,
    labels: Vec<&'static str>,
    selected_ix: usize,
    on_select: SegmentHandler,
    cx: &App,
) -> impl IntoElement + use<> {
    let t = cx.theme();
    let dark = t.is_dark();
    let overlay = native::overlay(cx);
    let track = overlay.opacity(0.06);
    let selected_bg = if dark { overlay.opacity(0.22) } else { native::paper(cx) };
    let selected_border = overlay.opacity(if dark { 0.12 } else { 0.1 });
    let hover_bg = native::control_hover(cx);
    let fg = t.foreground;
    let muted = t.muted_foreground;
    let radius = t.radius;

    let segment = move |label: &'static str| {
        div()
            .w(px(TAB_WIDTH))
            .h(px(22.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(radius)
            .text_size(px(text::BODY))
            .child(label)
    };

    let indicator_labels = labels.clone();
    // A spring retargets from its current position, so a quick second
    // click moves the fill on from where it is.
    let indicator = div()
        .absolute()
        .top(px(TAB_GAP))
        .w(px(TAB_WIDTH))
        .h(px(22.))
        .rounded(radius)
        .border_1()
        .border_color(selected_border)
        .bg(selected_bg)
        .shadow_xs()
        .overflow_hidden()
        .with_spring(
            ElementId::Name(format!("{id}-indicator").into()),
            SpringAnimation::new(SNAPPY_SPRING).to(selected_ix as f32),
            move |d, ix: f32| {
                let left = TAB_GAP + ix * (TAB_WIDTH + TAB_GAP);
                d.left(px(left)).child(
                    h_flex()
                        .absolute()
                        // The border is inside the fill, so move the copy
                        // back by its width too.
                        .top(px(-1.))
                        .left(px(TAB_GAP - left - 1.))
                        .gap(px(TAB_GAP))
                        .text_color(fg)
                        .font_weight(FontWeight::MEDIUM)
                        .children(indicator_labels.iter().map(|&label| segment(label))),
                )
            },
        );

    h_flex()
        .id(id)
        .relative()
        .p(px(TAB_GAP))
        .gap(px(TAB_GAP))
        .rounded(radius + px(TAB_GAP))
        .bg(track)
        .children(labels.iter().enumerate().map(|(ix, &label)| {
            let on_select = on_select.clone();
            segment(label)
                .id(label)
                .text_color(muted)
                .when(ix != selected_ix, |d| {
                    d.cursor_pointer().hover(|s| s.bg(hover_bg).text_color(fg))
                })
                .on_click(move |_, window, cx| on_select(ix, window, cx))
        }))
        .child(indicator)
}

pub(super) fn loading(id: &'static str) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .child(delayed_reveal(id, div().child(spinner())))
        .into_any_element()
}

/// A bordered box of rows with lines between them.
pub(super) fn card(rows: impl IntoIterator<Item = Div>, cx: &App) -> Div {
    let t = cx.theme();
    v_flex()
        .rounded(px(10.))
        .border_1()
        .border_color(t.border)
        .bg(t.background)
        .overflow_hidden()
        .children(rows.into_iter().enumerate().map(|(ix, row)| {
            row.when(ix > 0, |d| d.border_t_1().border_color(t.border))
        }))
}

pub(super) fn list_row() -> Div {
    h_flex().min_h(px(40.)).px_3p5().py_2()
}

/// A heading and its content.
pub(super) fn section(title: &'static str, cx: &App) -> Div {
    let t = cx.theme();
    v_flex().gap_2().child(
        div()
            .text_size(px(text::SUBHEADLINE))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.muted_foreground)
            .child(title),
    )
}

/// A label on the left and a value on the right.
pub(super) fn field(label: &'static str, value: impl IntoElement, cx: &App) -> Div {
    let t = cx.theme();
    list_row()
        .child(
            div()
                .w(px(116.))
                .flex_shrink_0()
                .text_color(t.muted_foreground)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(value))
}

/// A bar that shows `fraction`, and the value as text at its right.
fn meter(fraction: Option<f32>, value: String, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    h_flex()
        .gap_3()
        .child(
            div()
                .flex_1()
                .h(px(4.))
                .rounded_full()
                .bg(t.muted)
                .overflow_hidden()
                .when_some(fraction, |d, f| {
                    d.child(
                        div()
                            .h_full()
                            .w(relative(f.clamp(0., 1.)))
                            .rounded_full()
                            .bg(t.blue),
                    )
                }),
        )
        .child(
            div()
                .w(px(76.))
                .flex_shrink_0()
                .text_right()
                .whitespace_nowrap()
                .font_weight(FontWeight::MEDIUM)
                .font_features(tabular_nums())
                .child(value),
        )
}

fn port_row(ix: usize, port: &Port, cx: &App) -> Div {
    let t = cx.theme();
    list_row()
        .gap_2()
        .child(div().flex_1().child(super::container_list::port_link(("detail-port", ix), port, 0, false, cx)))
        .child(Icon::new(IconName::ArrowRight).xsmall().text_color(t.muted_foreground))
        .child(
            div()
                .font_family(t.mono_font_family.clone())
                .text_color(t.muted_foreground)
                .child(format!("{}/{}", port.private, port.proto)),
        )
        .when_some(port.url(), |d, url| {
            d.child(
                Button::new(("open-port", ix))
                    .icon(Icon::new(IconName::ExternalLink))
                    .ghost()
                    .xsmall()
                    .tooltip("Open in the browser")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
        })
}

impl Render for ContainerDetail {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.nav.begin(window);
        let t = cx.theme().clone();
        let store = self.store.read(cx);
        let row = store.set.rows.get(&self.id).cloned();

        let body = match (self.tab, &row) {
            (DetailTab::Info, Some(row)) => self.render_info(row, cx),
            (DetailTab::Info, None) => div().into_any_element(),
            (DetailTab::Logs, _) => match &self.logs {
                // A stats tick renders this pane again, but not the logs.
                Some(logs) => logs.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
                None => div().into_any_element(),
            },
            (DetailTab::Stats, _) => {
                let samples = self
                    .stats
                    .read(cx)
                    .history
                    .get(&self.id)
                    .map(|h| h.samples.iter().copied().collect())
                    .unwrap_or_default();
                let running = row.as_ref().is_some_and(|r| r.state.is_running());
                StatsView::new(samples, running).into_any_element()
            }
            (DetailTab::Inspect, _) => self.render_inspect(cx),
        };
        // Fade in the new tab body, so it does not replace the old one in a
        // single frame. Opacity only: the user reads this content, so it
        // does not move.
        let body = if self.tab_changes > 0 {
            div()
                .size_full()
                .child(body)
                .with_animation(
                    ("detail-tab-fade", self.tab_changes),
                    Animation::new(ENTER_DURATION).with_easing(ease_out_quint()),
                    |d, t| d.opacity(t),
                )
                .into_any_element()
        } else {
            body
        };

        let header = row.as_ref().map(|r| {
            let subtitle = match &r.compose_project {
                Some(p) => format!("{p} · {}", r.image),
                None => r.image.clone(),
            };
            let title = match (&r.compose_project, &r.compose_service) {
                (Some(_), Some(service)) => service.clone(),
                _ => r.name.clone(),
            };
            h_flex()
                .px_5()
                .pt_3()
                .pb_5()
                .gap_3()
                .child(
                    super::image_icon::ContainerIcon::new(
                        SharedString::from(format!("detail-icon-{}", r.id)),
                        &r.image,
                        r.icon.as_deref(),
                        super::state_color(r.state, cx),
                        r.state.is_running(),
                    )
                    .size(px(48.)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(text::TITLE3))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(text::CALLOUT))
                                .text_color(t.muted_foreground)
                                .child(subtitle),
                        ),
                )
                .when(self.tab == DetailTab::Logs, |d| {
                    d.child(
                        Button::new("reload-logs")
                            .icon(Icon::new(IconName::RefreshCw))
                            .ghost()
                            .small()
                            .tooltip("Open the log stream again")
                            .on_click(cx.listener(|this, _, window, cx| this.reopen_logs(window, cx))),
                    )
                })
        });

        key_nav::detail_actions(v_flex(), cx)
            .track_focus(&self.nav.focus)
            .size_full()
            .border_l_1()
            .border_color(t.border)
            .bg(t.sidebar.opacity(0.5))
            .child(
                h_flex()
                    .h(px(TOOLBAR_HEIGHT + 8.))
                    .flex_shrink_0()
                    .px_3()
                    .child(drag_area("detail-drag-left").flex_1().h_full())
                    .child(
                        self.render_tabs(cx),
                    )
                    .child(drag_area("detail-drag-right").flex_1().h_full())
                    .child(
                        Button::new("close-detail")
                            .icon(Icon::new(IconName::X))
                            .custom(native::toolbar_button(cx))
                            .xsmall()
                            .tooltip("Close (Esc)")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DetailEvent::Close))),
                    ),
            )
            .children(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}

//! The detail pane: a summary, logs, stats, and inspect data for one
//! container.

use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::Scrollbar;
use gpui_component::spinner::Spinner;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::{ActiveTheme, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::containers::{self, Action, Details, Mount, Port};
use crate::docker::stats::format_bytes;
use crate::docker::{ContainerRow, discover_socket};
use crate::runtime::Backend;
use crate::state::container_store::{ContainerStore, StoreEvent};
use crate::state::stats_store::StatsStore;
use crate::ui::logs_view::LogsView;
use crate::ui::stats_view::StatsView;
use crate::ui::{NATIVE_BUTTON_HEIGHT, TOOLBAR_HEIGHT, text, delayed_reveal, drag_area, tabular_nums};

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailTab {
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
}

struct Loaded {
    ip: Option<String>,
    mounts: Vec<Mount>,
    lines: Rc<Vec<SharedString>>,
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
    logs: Option<Entity<LogsView>>,
    inspect: Inspect,
    inspect_scroll: UniformListScrollHandle,
    /// The last row that this pane saw. Used to find changes.
    row: Option<ContainerRow>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<DetailEvent> for ContainerDetail {}

impl ContainerDetail {
    pub fn new(
        id: String,
        store: Entity<ContainerStore>,
        stats: Entity<StatsStore>,
        _: &mut Window,
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
            logs: None,
            inspect: Inspect::Loading,
            inspect_scroll: UniformListScrollHandle::new(),
            _subs: subs,
        };
        this.load_inspect(cx);
        this
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
                    Ok(Ok(Details { ip, mounts, json })) => Inspect::Loaded(Rc::new(Loaded {
                        ip,
                        mounts,
                        lines: Rc::new(json.lines().map(|l| SharedString::from(l.to_string())).collect()),
                    })),
                    Ok(Err(e)) => Inspect::Failed(e.to_string()),
                    Err(_) => Inspect::Failed("cancelled".into()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn set_tab(&mut self, tab: DetailTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == DetailTab::Logs {
            self.open_logs(window, cx);
        }
        cx.notify();
    }

    fn open_terminal(&mut self, cx: &mut Context<Self>) {
        let id = self.id.clone();
        let rx = Backend::global(cx).spawn(async move {
            tokio::task::spawn_blocking(move || crate::terminal::open_shell(&id, &discover_socket()))
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

    fn render_inspect(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        match &self.inspect {
            Inspect::Loading => loading("inspect-loading"),
            Inspect::Failed(e) => div()
                .p_3()
                .text_color(t.red)
                .child(e.clone())
                .into_any_element(),
            Inspect::Loaded(data) => {
                let lines = data.lines.clone();
                div()
                    .relative()
                    .size_full()
                    .font_family(t.mono_font_family.clone())
                    .text_size(px(text::SUBHEADLINE))
                    .child(
                        uniform_list("inspect", lines.len(), move |range: Range<usize>, _, _| {
                            range
                                .map(|ix| {
                                    div()
                                        .h(px(18.))
                                        .px_5()
                                        .whitespace_nowrap()
                                        .child(lines[ix].clone())
                                })
                                .collect::<Vec<_>>()
                        })
                        .size_full()
                        .track_scroll(&self.inspect_scroll),
                    )
                    .child(Scrollbar::vertical(&self.inspect_scroll))
                    .into_any_element()
            }
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

        let summary = card(
            [
                field(
                "Container ID",
                div().font_family(mono.clone()).child(row.short_id().to_string()),
                cx,
            ),
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
                row.ports.iter().enumerate().map(|(ix, port)| port_row(ix, port, cx)),
                cx,
            ))
        });

        let home = std::env::var("HOME").unwrap_or_default();
        let mounts = loaded.as_ref().filter(|d| !d.mounts.is_empty()).map(|d| {
            section("Mounts", cx).child(card(d.mounts.iter().map(|m| {
                let source = match m.source.strip_prefix(&home) {
                    Some(rest) if !m.volume && !home.is_empty() => format!("~{rest}"),
                    _ => m.source.clone(),
                };
                list_row()
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
                    )
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
                Button::new("open-terminal")
                    .outline()
                    .small()
                    .h(px(NATIVE_BUTTON_HEIGHT))
                    .flex_1()
                    .icon(Icon::new(IconName::SquareTerminal))
                    .label("Open Terminal")
                    .disabled(!running)
                    .tooltip("Open a shell in this container in the Terminal app")
                    .on_click(cx.listener(|this, _, _, cx| this.open_terminal(cx))),
            )
            .child(
                Button::new("restart")
                    .outline()
                    .small()
                    .h(px(NATIVE_BUTTON_HEIGHT))
                    .flex_1()
                    .icon(Icon::new(restart.0))
                    .label(restart.1)
                    .loading(pending)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = this.id.clone();
                        this.store.update(cx, |s, cx| s.run_action(&id, restart.2, cx));
                    })),
            );

        v_flex()
            .id("info")
            .size_full()
            .overflow_y_scroll()
            .px_5()
            .pb_5()
            .gap_5()
            .text_size(px(text::BODY))
            .child(summary)
            .children(ports)
            .children(mounts)
            .child(buttons)
            .into_any_element()
    }
}

fn loading(id: &'static str) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .child(delayed_reveal(id, div().child(Spinner::new())))
        .into_any_element()
}

/// A bordered box of rows with lines between them.
fn card(rows: impl IntoIterator<Item = Div>, cx: &App) -> Div {
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

fn list_row() -> Div {
    h_flex().min_h(px(40.)).px_3p5().py_2()
}

/// A heading and its content.
fn section(title: &'static str, cx: &App) -> Div {
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
fn field(label: &'static str, value: impl IntoElement, cx: &App) -> Div {
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let store = self.store.read(cx);
        let row = store.set.rows.get(&self.id).cloned();
        let tab_ix = DetailTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);

        let body = match (self.tab, &row) {
            (DetailTab::Info, Some(row)) => self.render_info(row, cx),
            (DetailTab::Info, None) => div().into_any_element(),
            (DetailTab::Logs, _) => match &self.logs {
                Some(logs) => logs.clone().into_any_element(),
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

        v_flex()
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
                        TabBar::new("detail-tabs")
                            .segmented()
                            .small()
                            .selected_index(tab_ix)
                            .children(DetailTab::ALL.map(|tab| Tab::new().label(tab.label()).px_3()))
                            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                                this.set_tab(DetailTab::ALL[*ix], window, cx)
                            })),
                    )
                    .child(drag_area("detail-drag-right").flex_1().h_full())
                    .child(
                        Button::new("close-detail")
                            .icon(Icon::new(IconName::X))
                            .ghost()
                            .xsmall()
                            .tooltip("Close (Esc)")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DetailEvent::Close))),
                    ),
            )
            .children(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}

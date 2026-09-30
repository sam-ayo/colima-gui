//! The detail pane: logs, stats, and inspect data for one container.

use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::Scrollbar;
use gpui_component::spinner::Spinner;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::{ContainerRow, containers};
use crate::runtime::Backend;
use crate::state::container_store::{ContainerStore, StoreEvent};
use crate::state::stats_store::StatsStore;
use crate::ui::logs_view::LogsView;
use crate::ui::stats_view::StatsView;
use crate::ui::{TOOLBAR_HEIGHT, delayed_reveal, drag_area};

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    Logs,
    Stats,
    Inspect,
}

impl DetailTab {
    const ALL: [DetailTab; 3] = [DetailTab::Logs, DetailTab::Stats, DetailTab::Inspect];
}

pub enum DetailEvent {
    Close,
}

enum Inspect {
    Loading,
    Loaded(Rc<Vec<SharedString>>),
    Failed(String),
}

pub struct ContainerDetail {
    id: String,
    store: Entity<ContainerStore>,
    stats: Entity<StatsStore>,
    tab: DetailTab,
    logs: Option<Entity<LogsView>>,
    inspect: Option<Inspect>,
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
                        if this.tab == DetailTab::Inspect {
                            this.load_inspect(cx);
                        } else {
                            this.inspect = None;
                        }
                    }
                    cx.notify();
                }
            }),
            cx.observe(&stats, |this, _, cx| {
                if this.tab == DetailTab::Stats {
                    cx.notify();
                }
            }),
        ];
        let mut this = ContainerDetail {
            row: store.read(cx).set.rows.get(&id).cloned(),
            id,
            store,
            stats,
            tab: DetailTab::Logs,
            logs: None,
            inspect: None,
            inspect_scroll: UniformListScrollHandle::new(),
            _subs: subs,
        };
        this.open_logs(window, cx);
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
        if !matches!(self.inspect, Some(Inspect::Loaded(_))) {
            self.inspect = Some(Inspect::Loading);
        }
        let id = self.id.clone();
        let rx = Backend::global(cx).spawn(async move { containers::inspect_json(&docker, &id).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await;
            let _ = this.update(cx, |this, cx| {
                this.inspect = Some(match result {
                    Ok(Ok(json)) => Inspect::Loaded(Rc::new(
                        json.lines().map(|l| SharedString::from(l.to_string())).collect(),
                    )),
                    Ok(Err(e)) => Inspect::Failed(e.to_string()),
                    Err(_) => Inspect::Failed("cancelled".into()),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn set_tab(&mut self, tab: DetailTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        match tab {
            DetailTab::Logs => self.open_logs(window, cx),
            DetailTab::Inspect => {
                if !matches!(self.inspect, Some(Inspect::Loaded(_))) {
                    self.load_inspect(cx)
                }
            }
            DetailTab::Stats => {}
        }
        cx.notify();
    }

    fn render_inspect(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        match &self.inspect {
            None | Some(Inspect::Loading) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(delayed_reveal("inspect-loading", div().child(Spinner::new())))
                .into_any_element(),
            Some(Inspect::Failed(e)) => div()
                .p_3()
                .text_color(t.red)
                .child(e.clone())
                .into_any_element(),
            Some(Inspect::Loaded(lines)) => {
                let lines = lines.clone();
                div()
                    .relative()
                    .size_full()
                    .font_family(t.mono_font_family.clone())
                    .text_xs()
                    .child(
                        uniform_list("inspect", lines.len(), move |range: Range<usize>, _, _| {
                            range
                                .map(|ix| {
                                    div()
                                        .h(px(18.))
                                        .px_4()
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
}

impl Render for ContainerDetail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let store = self.store.read(cx);
        let row = store.set.rows.get(&self.id).cloned();
        let (title, subtitle, state) = match &row {
            Some(r) => (
                r.name.clone(),
                format!("{} · {} · {}", r.image, r.short_id(), r.status),
                Some(r.state),
            ),
            None => (self.id.clone(), String::new(), None),
        };
        let running = state.is_some_and(|s| s.is_running());
        let tab_ix = DetailTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);

        let body = match self.tab {
            DetailTab::Logs => match &self.logs {
                Some(logs) => logs.clone().into_any_element(),
                None => div().into_any_element(),
            },
            DetailTab::Stats => {
                let samples = self
                    .stats
                    .read(cx)
                    .history
                    .get(&self.id)
                    .map(|h| h.samples.iter().copied().collect())
                    .unwrap_or_default();
                StatsView::new(samples, running).into_any_element()
            }
            DetailTab::Inspect => self.render_inspect(cx),
        };

        v_flex()
            .size_full()
            .border_l_1()
            .border_color(t.border)
            .bg(t.background)
            .child(
                h_flex()
                    .h(px(TOOLBAR_HEIGHT))
                    .flex_shrink_0()
                    .px_4()
                    .gap_2p5()
                    .when_some(row.as_ref().zip(state), |d, (r, s)| {
                        d.child(
                            super::image_icon::ContainerIcon::new(
                                &r.image,
                                super::state_color(s, cx),
                                s.is_running(),
                            )
                            .size(px(30.)),
                        )
                    })
                    .child(
                        drag_area("detail-drag")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(t.muted_foreground)
                                    .child(subtitle),
                            ),
                    )
                    .when(self.tab == DetailTab::Logs, |d| {
                        d.child(
                            Button::new("reload-logs")
                                .icon(Icon::new(IconName::RefreshCw))
                                .ghost()
                                .xsmall()
                                .tooltip("Open the log stream again")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.reopen_logs(window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("close-detail")
                            .icon(Icon::new(IconName::X))
                            .ghost()
                            .xsmall()
                            .tooltip("Close")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DetailEvent::Close))),
                    ),
            )
            .child(
                div().px_4().pb_2().child(
                    TabBar::new("detail-tabs")
                        .segmented()
                        .small()
                        .w_full()
                        .selected_index(tab_ix)
                        .child(Tab::new().label("Logs").flex_1())
                        .child(Tab::new().label("Stats").flex_1())
                        .child(Tab::new().label("Inspect").flex_1())
                        .on_click(cx.listener(|this, ix: &usize, window, cx| {
                            this.set_tab(DetailTab::ALL[*ix], window, cx)
                        })),
                ),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

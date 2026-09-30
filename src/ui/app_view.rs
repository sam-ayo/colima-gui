//! The root view: the sidebar, the container list, and the detail pane.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::notification::Notification;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Icon, Theme, WindowExt as _, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::EngineStatus;
use crate::state::container_store::{ContainerStore, StoreEvent};
use crate::state::stats_store::StatsStore;
use crate::ui::container_detail::{ContainerDetail, DetailEvent};
use crate::ui::container_list::{ContainerList, ListEvent};
use crate::ui::sidebar::{Section, Sidebar};
use crate::ui::{ENTER_DURATION, TOOLBAR_HEIGHT, delayed_reveal, drag_area};

pub struct AppView {
    store: Entity<ContainerStore>,
    stats: Entity<StatsStore>,
    list: Entity<ContainerList>,
    detail: Option<Entity<ContainerDetail>>,
    section: Section,
    /// Counts the times that the detail pane opened. Each open gets a new
    /// animation id, so the entrance plays one time for each open.
    detail_opens: usize,
    /// True when a pointer opened the detail pane. A pane that a key opens
    /// shows immediately, because keys are fast and frequent.
    detail_animated: bool,
    /// The engine status at the last change, and the number of changes.
    /// Each change gets a new animation id, so the main area fades one time.
    engine_status: EngineStatus,
    engine_changes: usize,
    _subs: Vec<Subscription>,
    _detail_sub: Option<Subscription>,
}

impl AppView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = cx.new(ContainerStore::new);
        let stats = cx.new(|_| StatsStore::new());
        let list = cx.new(|cx| ContainerList::new(store.clone(), stats.clone(), window, cx));
        let subs = vec![
            cx.subscribe_in(&store, window, |this, _, ev: &StoreEvent, window, cx| match ev {
                StoreEvent::Error(msg) => {
                    window.push_notification(Notification::error(msg.clone()), cx)
                }
                StoreEvent::Changed => {
                    let status = this.store.read(cx).status.clone();
                    // Close the detail pane when the engine goes away.
                    if status != EngineStatus::Connected {
                        this.close_detail(cx);
                    }
                    if std::mem::discriminant(&status) != std::mem::discriminant(&this.engine_status) {
                        this.engine_status = status;
                        this.engine_changes += 1;
                    }
                    cx.notify();
                }
            }),
            cx.subscribe_in(&list, window, |this, _, ev: &ListEvent, window, cx| match ev {
                ListEvent::Selected { id: Some(id), pointer } => {
                    this.open_detail(id.clone(), *pointer, window, cx)
                }
                ListEvent::Selected { id: None, .. } => this.close_detail(cx),
            }),
        ];
        list.focus_handle(cx).focus(window, cx);
        AppView {
            store,
            stats,
            list,
            detail: None,
            section: Section::Containers,
            detail_opens: 0,
            detail_animated: false,
            engine_status: EngineStatus::Connecting,
            engine_changes: 0,
            _subs: subs,
            _detail_sub: None,
        }
    }

    fn open_detail(&mut self, id: String, pointer: bool, window: &mut Window, cx: &mut Context<Self>) {
        // A change to a different container keeps the pane where it is.
        if self.detail.is_none() {
            self.detail_opens += 1;
            self.detail_animated = pointer;
        }
        let (store, stats) = (self.store.clone(), self.stats.clone());
        let detail = cx.new(|cx| ContainerDetail::new(id, store, stats, window, cx));
        self._detail_sub = Some(cx.subscribe(&detail, |this, _, ev: &DetailEvent, cx| match ev {
            DetailEvent::Close => {
                this.list.update(cx, |l, cx| l.clear_selection(cx));
            }
        }));
        self.detail = Some(detail);
        cx.notify();
    }

    fn close_detail(&mut self, cx: &mut Context<Self>) {
        // A dropped detail view drops its log stream too.
        if self.detail.take().is_some() {
            self._detail_sub = None;
            cx.notify();
        }
    }

    fn render_engine_state(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let store = self.store.read(cx);
        let el = match &store.status {
            EngineStatus::Connected => return None,
            EngineStatus::Connecting => {
                // A fast connection shows nothing here, not a spinner that
                // flashes.
                let t = cx.theme();
                empty_state(t)
                    .child(delayed_reveal(
                        "connecting",
                        v_flex()
                            .items_center()
                            .gap_2()
                            .child(Spinner::new())
                            .child(div().text_sm().child("Connecting to Docker…")),
                    ))
                    .into_any_element()
            }
            EngineStatus::NotRunning | EngineStatus::Error(_) => {
                let starting = store.starting_colima;
                let detail = match &store.status {
                    EngineStatus::Error(e) => e.clone(),
                    _ => "Start the Colima virtual machine to manage your containers.".to_string(),
                };
                let t = cx.theme();
                empty_state(t)
                    .child(icon_tile(IconName::Container, t))
                    .child(empty_title("Colima is not running", t))
                    .child(div().max_w(px(360.)).text_center().text_sm().child(detail))
                    .child(
                        div().pt_2().child(
                            Button::new("start-colima")
                                .primary()
                                .icon(Icon::new(IconName::Play))
                                .label(if starting { "Starting Colima…" } else { "Start Colima" })
                                .loading(starting)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.store.update(cx, |s, cx| s.start_colima(cx))
                                })),
                        ),
                    )
                    .into_any_element()
            }
        };
        Some(el)
    }
}

/// The centered column of an empty or placeholder view.
fn empty_state(t: &Theme) -> Div {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_2()
        .pb(px(TOOLBAR_HEIGHT))
        .text_color(t.muted_foreground)
}

fn icon_tile(icon: IconName, t: &Theme) -> impl IntoElement {
    div()
        .size(px(56.))
        .mb_2()
        .rounded(px(14.))
        .bg(t.muted)
        .flex()
        .items_center()
        .justify_center()
        .child(Icon::new(icon).size(px(26.)).text_color(t.muted_foreground))
}

fn empty_title(text: &'static str, t: &Theme) -> impl IntoElement {
    div()
        .text_lg()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.foreground)
        .child(text)
}

/// Adds a strip at the top that moves the window, for views that have no
/// toolbar.
fn with_drag_area(content: impl IntoElement) -> AnyElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .child(drag_area("main-drag").w_full().h(px(TOOLBAR_HEIGHT)).flex_shrink_0())
        .child(content)
        .into_any_element()
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let store = self.store.read(cx);
        // The rows are out of date while the engine is not available.
        let running = if store.status == EngineStatus::Connected {
            store.set.rows.values().filter(|r| r.state.is_running()).count()
        } else {
            0
        };
        let view = cx.weak_entity();
        let engine = store.status.clone();
        let sidebar = Sidebar::new(self.section, running, engine, move |section, _, cx| {
            let _ = view.update(cx, |this, cx| {
                this.section = section;
                cx.notify();
            });
        });

        let main = if !self.section.is_available() {
            with_drag_area(
                empty_state(&t)
                    .child(icon_tile(self.section.icon(), &t))
                    .child(empty_title(self.section.label(), &t))
                    .child(div().text_sm().child("This section comes in a later version.")),
            )
        } else if let Some(state) = self.render_engine_state(cx) {
            with_drag_area(state)
        } else {
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(div().flex_1().min_w_0().h_full().child(self.list.clone()))
                .when_some(self.detail.clone(), |d, detail| {
                    let pane = div().relative().w(relative(0.45)).min_w(px(380.)).h_full().child(detail);
                    if self.detail_animated {
                        // Come in from the right edge, where the pane lives.
                        d.child(pane.with_animation(
                            ("detail-enter", self.detail_opens),
                            Animation::new(ENTER_DURATION).with_easing(ease_out_quint()),
                            |pane, t| pane.opacity(t).left(px(16. * (1. - t))),
                        ))
                    } else {
                        d.child(pane)
                    }
                })
                .into_any_element()
        };

        // Fade between engine states, so a stopped engine or a new connection
        // does not replace the view in one frame. The wrapper stays in the
        // tree, so a section change does not play the fade again.
        let main = div()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(main)
            .with_animation(
                ("engine-fade", self.engine_changes),
                Animation::new(ENTER_DURATION).with_easing(ease_out_quint()),
                |d, t| d.opacity(t),
            );

        div()
            .flex()
            .size_full()
            .bg(t.background)
            .text_color(t.foreground)
            .child(sidebar)
            .child(main)
    }
}

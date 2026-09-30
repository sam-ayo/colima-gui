//! The root view: the sidebar, the container list, and the detail pane.

use std::time::Instant;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::notification::Notification;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Icon, Sizable as _, Theme, WindowExt as _, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::EngineStatus;
use crate::state::container_store::{ContainerStore, StoreEvent};
use crate::state::machine_store::{MachineEvent, MachineStore};
use crate::state::stats_store::StatsStore;
use crate::ui::container_detail::{ContainerDetail, DetailEvent};
use crate::ui::container_list::{ContainerList, ListEvent};
use crate::ui::sidebar::{Section, Sidebar};
use crate::ui::{
    NATIVE_BUTTON_HEIGHT, native, text,
    ENTER_DURATION, SIDEBAR_INSET, SIDEBAR_WIDTH, TOOLBAR_HEIGHT, delayed_reveal, drag_area,
};

actions!(app_view, [ToggleSidebar]);

const SIDEBAR_MIN_WIDTH: f32 = 180.;
const SIDEBAR_MAX_WIDTH: f32 = 360.;
/// A drag to the left of this position collapses the sidebar, as in the
/// macOS apps.
const SIDEBAR_COLLAPSE_AT: f32 = 110.;
/// The sidebar button sits to the right of the traffic lights. It stays in
/// the same position when the sidebar is open or collapsed.
const SIDEBAR_BUTTON_LEFT: f32 = 86.;
const SIDEBAR_BUTTON_TOP: f32 = 13.;
/// The space that the toolbar of the main area gives to the traffic lights
/// and the sidebar button when the sidebar is collapsed.
const COLLAPSED_INSET: f32 = 104.;
/// The spring that opens and collapses the sidebar. It is critically
/// damped, so the edge does not bounce. It is almost at rest after about
/// 200 ms, and a new toggle in the middle keeps the speed of the edge.
const SIDEBAR_SPRING: SpringConfig = SpringConfig::new(400., 40., 1.);
/// The spring is at rest when it is nearer than this to the target.
const SIDEBAR_SPRING_EPSILON: f32 = 0.5;

pub fn bind_keys(cx: &mut App) {
    // The standard macOS shortcut for "Show Sidebar" and "Hide Sidebar".
    cx.bind_keys([KeyBinding::new("cmd-ctrl-s", ToggleSidebar, None)]);
}

/// The value of a sidebar resize drag: the panel width at the start.
struct SidebarResize(Pixels);

impl Render for SidebarResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

pub struct AppView {
    store: Entity<ContainerStore>,
    stats: Entity<StatsStore>,
    machines: Entity<MachineStore>,
    /// The Colima profile that the app connects to.
    profile: String,
    list: Entity<ContainerList>,
    detail: Option<Entity<ContainerDetail>>,
    section: Section,
    /// The width of the sidebar panel, without the inset around it.
    sidebar_width: Pixels,
    sidebar_collapsed: bool,
    /// The width of the sidebar area on the screen, and its speed. It goes
    /// to `sidebar_area_width` on a spring.
    sidebar_spring: SpringState,
    /// The time of the last spring step. It is `None` when the spring is at
    /// rest, so the next movement does not start with a long step.
    sidebar_stepped_at: Option<Instant>,
    /// True while a drag changes the width. The sidebar then follows the
    /// pointer with no spring. An open or a collapse always uses the spring.
    sidebar_follows_pointer: bool,
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
        let machines = cx.new(MachineStore::new);
        let list = cx.new(|cx| ContainerList::new(store.clone(), stats.clone(), window, cx));
        let subs = vec![
            // Follow the system when it changes between light and dark mode.
            cx.observe_window_appearance(window, |_, window, cx| {
                native::sync_theme(window, cx);
            }),
            // The accent color can change while the app is in the
            // background. Read the system colors again when the window comes
            // back. The selected row also changes between the accent fill
            // and the gray fill.
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    native::sync_theme(window, cx);
                }
                cx.notify();
            }),
            cx.observe(&machines, |_, _, cx| cx.notify()),
            cx.subscribe_in(&machines, window, |_, _, ev: &MachineEvent, window, cx| match ev {
                MachineEvent::Error(msg) => {
                    window.push_notification(Notification::error(msg.clone()), cx)
                }
            }),
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
                        // The machine list is out of date when the engine
                        // comes or goes.
                        this.machines.update(cx, |m, cx| m.refresh(cx));
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
            machines,
            profile: crate::docker::current_profile(),
            list,
            detail: None,
            section: Section::Containers,
            sidebar_width: px(SIDEBAR_WIDTH),
            sidebar_collapsed: false,
            sidebar_spring: SpringState {
                position: SIDEBAR_WIDTH + 2. * SIDEBAR_INSET,
                velocity: 0.,
            },
            sidebar_stepped_at: None,
            sidebar_follows_pointer: false,
            detail_opens: 0,
            detail_animated: false,
            engine_status: EngineStatus::Connecting,
            engine_changes: 0,
            _subs: subs,
            _detail_sub: None,
        }
    }

    /// The width of the sidebar area, with the inset around the panel.
    fn sidebar_area_width(&self) -> Pixels {
        if self.sidebar_collapsed {
            px(0.)
        } else {
            self.sidebar_width + px(2. * SIDEBAR_INSET)
        }
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.set_sidebar_collapsed(!self.sidebar_collapsed, cx);
    }

    fn set_sidebar_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        if self.sidebar_collapsed == collapsed {
            return;
        }
        self.sidebar_collapsed = collapsed;
        self.sidebar_follows_pointer = false;
        cx.notify();
    }

    /// Moves the sidebar spring one frame toward the target, and returns
    /// the width of the sidebar area for this frame.
    fn step_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Pixels {
        let target = f32::from(self.sidebar_area_width());
        let now = Instant::now();
        let spring = &mut self.sidebar_spring;
        if self.sidebar_follows_pointer || cx.reduce_motion() {
            *spring = SpringState { position: target, velocity: 0. };
        } else if spring.position != target || spring.velocity != 0. {
            let elapsed = self
                .sidebar_stepped_at
                .map_or(0., |at| now.duration_since(at).as_secs_f32());
            *spring = SIDEBAR_SPRING.step(*spring, target, elapsed);
            if SIDEBAR_SPRING.is_settled(*spring, target, SIDEBAR_SPRING_EPSILON) {
                *spring = SpringState { position: target, velocity: 0. };
            }
        }
        let moving = spring.position != target;
        self.sidebar_stepped_at = moving.then_some(now);
        if moving {
            window.request_animation_frame();
        }
        let width = px(spring.position.max(0.));

        // The toolbar of the main area moves with the edge of the sidebar.
        // It stops before it goes below the sidebar button.
        let inset = (px(COLLAPSED_INSET) - width).max(px(0.));
        if self.list.read(cx).leading_inset() != inset {
            self.list.update(cx, |l, cx| l.set_leading_inset(inset, cx));
        }
        width
    }

    /// Follows a resize drag. `x` is the pointer position in the window.
    fn resize_sidebar(&mut self, x: Pixels, start_width: Pixels, cx: &mut Context<Self>) {
        if x < px(SIDEBAR_COLLAPSE_AT) {
            // The drag went through the minimum width on its way. The
            // sidebar opens again at the width from before the drag.
            self.sidebar_width = start_width;
            self.set_sidebar_collapsed(true, cx);
            return;
        }
        self.set_sidebar_collapsed(false, cx);
        // The handle is the inset at the right of the panel. Its center
        // follows the pointer.
        let width = (x - px(1.5 * SIDEBAR_INSET))
            .clamp(px(SIDEBAR_MIN_WIDTH), px(SIDEBAR_MAX_WIDTH));
        if width != self.sidebar_width {
            self.sidebar_width = width;
            self.sidebar_follows_pointer = true;
            cx.notify();
        }
    }

    fn render_sidebar_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .left(px(SIDEBAR_BUTTON_LEFT))
            .top(px(SIDEBAR_BUTTON_TOP))
            // Keep the press away from the window drag areas below.
            .occlude()
            .child(
                Button::new("toggle-sidebar")
                    .icon(Icon::new(IconName::PanelLeft))
                    .ghost()
                    .small()
                    .tooltip(if self.sidebar_collapsed {
                        "Show the sidebar (⌃⌘S)"
                    } else {
                        "Hide the sidebar (⌃⌘S)"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
            )
    }

    fn open_detail(&mut self, id: String, pointer: bool, window: &mut Window, cx: &mut Context<Self>) {
        // A change to a different container keeps the pane where it is.
        if self.detail.is_none() {
            self.detail_opens += 1;
            self.detail_animated = pointer;
        }
        let (store, stats) = (self.store.clone(), self.stats.clone());
        let detail = cx.new(|cx| ContainerDetail::new(id, store, stats, window, cx));
        self._detail_sub = Some(cx.subscribe_in(&detail, window, |this, _, ev: &DetailEvent, window, cx| {
            match ev {
                DetailEvent::Close => {
                    this.list.update(cx, |l, cx| l.clear_selection(cx));
                }
                DetailEvent::Error(msg) => {
                    window.push_notification(Notification::error(msg.clone()), cx)
                }
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
                            .child(div().text_size(px(text::BODY)).child("Connecting to Docker…")),
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
                    .child(div().max_w(px(360.)).text_center().text_size(px(text::BODY)).child(detail))
                    .child(
                        div().pt_2().child(
                            Button::new("start-colima")
                                .primary()
                                .small()
                                .h(px(NATIVE_BUTTON_HEIGHT))
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
        .text_size(px(text::TITLE2))
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar_area = self.step_sidebar(window, cx);
        let t = cx.theme().clone();
        let view = cx.weak_entity();
        let machines = self.machines.read(cx);
        let toggle_view = view.clone();
        let sidebar = Sidebar::new(self.section, self.sidebar_width, move |section, _, cx| {
            let _ = view.update(cx, |this, cx| {
                this.section = section;
                cx.notify();
            });
        })
        .machines(
            machines.machines.clone(),
            machines.busy.clone(),
            self.profile.clone(),
            move |name, _, cx| {
                let _ = toggle_view.update(cx, |this, cx| {
                    this.machines.update(cx, |m, cx| m.toggle(name, cx))
                });
            },
        );

        let main = if !self.section.is_available() {
            with_drag_area(
                empty_state(&t)
                    .child(icon_tile(self.section.icon(), &t))
                    .child(empty_title(self.section.label(), &t))
                    .child(div().text_size(px(text::BODY)).child("This section comes in a later version.")),
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
                    let pane = div()
                        .relative()
                        .w(relative(0.36))
                        .min_w(px(380.))
                        .max_w(px(480.))
                        .h_full()
                        .child(detail);
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

        let main = div().flex().flex_1().min_w_0().h_full().bg(t.background).child(main);

        // The panel keeps its width and sits at the right edge of the
        // area. When the area gets narrow, the panel moves out at the left,
        // and its content does not wrap.
        let sidebar = div()
            .relative()
            .h_full()
            .flex_shrink_0()
            .overflow_hidden()
            .w(sidebar_area)
            .child(div().absolute().top_0().right_0().h_full().child(sidebar))
            .when(!self.sidebar_collapsed, |d| {
                d.child(
                    div()
                        .id("sidebar-resize")
                        .absolute()
                        .top_0()
                        .right_0()
                        .w(px(SIDEBAR_INSET))
                        .h_full()
                        .occlude()
                        .cursor_col_resize()
                        .on_drag(SidebarResize(self.sidebar_width), |drag, _, _, cx| {
                            cx.new(|_| SidebarResize(drag.0))
                        }),
                )
            });

        div()
            .relative()
            .flex()
            .size_full()
            .text_size(px(text::BODY))
            .text_color(t.foreground)
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_drag_move(cx.listener(|this, ev: &DragMoveEvent<SidebarResize>, _, cx| {
                this.resize_sidebar(ev.event.position.x, ev.drag(cx).0, cx)
            }))
            .child(sidebar)
            .child(main)
            .child(self.render_sidebar_button(cx))
    }
}

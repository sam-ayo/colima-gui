//! The root view: the sidebar, the container list, and the detail pane.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Icon, Sizable as _, Theme, WindowExt as _, v_flex};
use gpui_kit_assets::IconName;

use crate::colima::{self, MachineAction};
use crate::docker::EngineStatus;
use crate::state::container_store::{ContainerStore, StoreEvent};
use crate::state::machine_store::{MachineEvent, MachineStore};
use crate::state::stats_store::StatsStore;
use crate::ui::container_detail::{ContainerDetail, DetailEvent};
use crate::ui::container_list::{ContainerList, ListEvent};
use crate::ui::sidebar::{Section, Sidebar};
use crate::ui::{
    NATIVE_BUTTON_HEIGHT, native, native::MenuEntry, text,
    ENTER_DURATION, SIDEBAR_INSET, SIDEBAR_WIDTH, TOOLBAR_HEIGHT, delayed_reveal, drag_area, spinner,
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
/// In full screen, macOS hides the traffic lights. The sidebar button then
/// moves to the left edge, in line with the sidebar rows.
const SIDEBAR_BUTTON_LEFT_FULLSCREEN: f32 = 14.;
const SIDEBAR_BUTTON_TOP: f32 = 13.;
/// The space that the toolbar of the main area gives to the sidebar button
/// when the sidebar is collapsed. The value is from the left edge of the
/// button.
const COLLAPSED_BUTTON_SPACE: f32 = 18.;
/// The limits of the detail pane width. Before a resize, the pane takes a
/// part of the main area between the minimum and `DETAIL_DEFAULT_MAX_WIDTH`.
const DETAIL_MIN_WIDTH: f32 = 380.;
const DETAIL_MAX_WIDTH: f32 = 800.;
const DETAIL_DEFAULT_MAX_WIDTH: f32 = 480.;
const DETAIL_DEFAULT_FRACTION: f32 = 0.36;
/// The container list keeps this width when the sidebar or the detail pane
/// gets wider. At this width, the rows hide the ports column, and the row
/// actions still show. The minimum window width leaves this space next to
/// the smallest sidebar and detail pane.
const LIST_MIN_WIDTH: f32 = 320.;
/// The width of the resize handle at the left edge of the detail pane.
const DETAIL_HANDLE_WIDTH: f32 = 6.;
/// The spring that opens and collapses the sidebar. It is critically
/// damped, so the edge does not bounce. It is almost at rest after about
/// 200 ms, and a new toggle in the middle keeps the speed of the edge.
const SIDEBAR_SPRING: SpringConfig = SpringConfig::new(400., 40., 1.);
/// The spring is at rest when it is nearer than this to the target.
const SIDEBAR_SPRING_EPSILON: f32 = 0.5;
/// The detail pane opens and closes on the sidebar spring, so the two
/// panels move the same way. The spring moves a part of the pane width,
/// from 0 to 1. This value is about half a pixel of a pane.
const DETAIL_SPRING_EPSILON: f32 = 0.001;

pub fn bind_keys(cx: &mut App) {
    // The standard macOS shortcut for "Show Sidebar" and "Hide Sidebar".
    cx.bind_keys([KeyBinding::new("cmd-ctrl-s", ToggleSidebar, None)]);
}

/// The value of a sidebar resize drag.
struct SidebarResize {
    /// The panel width at the start. A collapse by the drag keeps it.
    start_width: Pixels,
    /// The pointer position in the handle at the start. The edge then keeps
    /// the same distance to the pointer, and does not jump.
    grab: Rc<Cell<Pixels>>,
}

/// The value of a detail pane resize drag.
struct DetailResize {
    /// The pointer position in the handle at the start.
    grab: Rc<Cell<Pixels>>,
}

/// The view that follows the pointer in a resize drag. It shows nothing.
struct ResizeDrag;

impl Render for ResizeDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

pub struct AppView {
    store: Entity<ContainerStore>,
    stats: Entity<StatsStore>,
    machines: Entity<MachineStore>,
    list: Entity<ContainerList>,
    detail: Option<Entity<ContainerDetail>>,
    /// The detail pane that goes out after a click on its close button.
    /// It stays until the spring closes it.
    closing_detail: Option<Entity<ContainerDetail>>,
    /// The part of the detail pane width that shows, from 0 to 1, and its
    /// speed. The list gets the space that the pane does not use.
    detail_spring: SpringState,
    /// The time of the last spring step, or `None` when the spring is at
    /// rest.
    detail_stepped_at: Option<Instant>,
    section: Section,
    /// The width of the sidebar panel that the user set, without the inset
    /// around it. A narrow window can show the panel narrower.
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
    /// The width of the detail pane after a resize. It is `None` until the
    /// first resize, and the pane then uses a part of the main area. A
    /// narrow main area can show the pane narrower.
    detail_width: Option<Pixels>,
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
            cx.subscribe_in(&machines, window, |this, _, ev: &MachineEvent, window, cx| match ev {
                MachineEvent::Error(msg) => {
                    window.push_notification(Notification::error(msg.clone()), cx)
                }
                MachineEvent::Ready(profile) => {
                    if this.store.read(cx).profile() == Some(profile.as_str()) {
                        this.store.update(cx, |s, cx| s.wake(cx));
                    }
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
                        this.close_detail(false, cx);
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
                ListEvent::Selected { id: None, pointer } => this.close_detail(*pointer, cx),
            }),
        ];
        list.focus_handle(cx).focus(window, cx);
        AppView {
            store,
            stats,
            machines,
            list,
            detail: None,
            closing_detail: None,
            detail_spring: SpringState { position: 0., velocity: 0. },
            detail_stepped_at: None,
            section: Section::Containers,
            sidebar_width: px(SIDEBAR_WIDTH),
            sidebar_collapsed: false,
            sidebar_spring: SpringState {
                position: SIDEBAR_WIDTH + 2. * SIDEBAR_INSET,
                velocity: 0.,
            },
            sidebar_stepped_at: None,
            sidebar_follows_pointer: false,
            detail_width: None,
            engine_status: EngineStatus::Connecting,
            engine_changes: 0,
            _subs: subs,
            _detail_sub: None,
        }
    }

    /// True when the main area shows the detail pane.
    fn detail_shown(&self, cx: &App) -> bool {
        self.detail.is_some()
            && self.section.is_available()
            && self.store.read(cx).status == EngineStatus::Connected
    }

    /// The largest sidebar panel that leaves space for the list and the
    /// detail pane. It is never less than the minimum panel width.
    fn sidebar_max_width(&self, window_width: Pixels, cx: &App) -> Pixels {
        let detail = if self.detail_shown(cx) { DETAIL_MIN_WIDTH } else { 0. };
        (window_width - px(2. * SIDEBAR_INSET + LIST_MIN_WIDTH + detail))
            .min(px(SIDEBAR_MAX_WIDTH))
            .max(px(SIDEBAR_MIN_WIDTH))
    }

    /// The width of the sidebar panel on the screen.
    fn sidebar_panel_width(&self, window_width: Pixels, cx: &App) -> Pixels {
        self.sidebar_width.min(self.sidebar_max_width(window_width, cx))
    }

    /// The width of the sidebar area, with the inset around the panel.
    fn sidebar_area_width(&self, window_width: Pixels, cx: &App) -> Pixels {
        if self.sidebar_collapsed {
            px(0.)
        } else {
            self.sidebar_panel_width(window_width, cx) + px(2. * SIDEBAR_INSET)
        }
    }

    /// The largest detail pane that leaves space for the list. It is never
    /// less than the minimum pane width.
    fn detail_max_width(main_width: Pixels) -> Pixels {
        (main_width - px(LIST_MIN_WIDTH))
            .min(px(DETAIL_MAX_WIDTH))
            .max(px(DETAIL_MIN_WIDTH))
    }

    /// The width of the detail pane on the screen.
    fn detail_pane_width(&self, main_width: Pixels) -> Pixels {
        let width = self.detail_width.unwrap_or_else(|| {
            (main_width * DETAIL_DEFAULT_FRACTION)
                .min(px(DETAIL_DEFAULT_MAX_WIDTH))
                .max(px(DETAIL_MIN_WIDTH))
        });
        width.min(Self::detail_max_width(main_width)).max(px(DETAIL_MIN_WIDTH))
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
        let target = f32::from(self.sidebar_area_width(window.viewport_size().width, cx));
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
        let collapsed_inset = Self::sidebar_button_left(window) + COLLAPSED_BUTTON_SPACE;
        let inset = (px(collapsed_inset) - width).max(px(0.));
        if self.list.read(cx).leading_inset() != inset {
            self.list.update(cx, |l, cx| l.set_leading_inset(inset, cx));
        }
        width
    }

    /// Moves the detail spring one frame toward the target, and returns
    /// the part of the pane width that shows in this frame. At the end of
    /// a close, the pane that goes out is dropped.
    fn step_detail(&mut self, window: &mut Window, cx: &App) -> f32 {
        let target = if self.detail.is_some() { 1. } else { 0. };
        let now = Instant::now();
        let spring = &mut self.detail_spring;
        if cx.reduce_motion() {
            *spring = SpringState { position: target, velocity: 0. };
        } else if spring.position != target || spring.velocity != 0. {
            let elapsed = self
                .detail_stepped_at
                .map_or(0., |at| now.duration_since(at).as_secs_f32());
            *spring = SIDEBAR_SPRING.step(*spring, target, elapsed);
            if SIDEBAR_SPRING.is_settled(*spring, target, DETAIL_SPRING_EPSILON) {
                *spring = SpringState { position: target, velocity: 0. };
            }
        }
        let moving = spring.position != target;
        self.detail_stepped_at = moving.then_some(now);
        if moving {
            window.request_animation_frame();
        } else if target == 0. {
            // A dropped detail view drops its log stream too.
            self.closing_detail = None;
        }
        spring.position.clamp(0., 1.)
    }

    /// Follows a resize drag. `x` is the pointer position in the window.
    fn resize_sidebar(&mut self, x: Pixels, drag: &SidebarResize, window: &Window, cx: &mut Context<Self>) {
        if x < px(SIDEBAR_COLLAPSE_AT) {
            // The drag went through the minimum width on its way. The
            // sidebar opens again at the width from before the drag.
            self.sidebar_width = drag.start_width;
            self.set_sidebar_collapsed(true, cx);
            return;
        }
        self.set_sidebar_collapsed(false, cx);
        // The handle is the inset at the right of the panel. The left edge
        // of the handle is the right edge of the panel.
        let max = self.sidebar_max_width(window.viewport_size().width, cx);
        let width = (x - drag.grab.get() - px(SIDEBAR_INSET))
            .min(max)
            .max(px(SIDEBAR_MIN_WIDTH));
        if width != self.sidebar_width {
            self.sidebar_width = width;
            self.sidebar_follows_pointer = true;
            cx.notify();
        }
    }

    /// Follows a detail pane resize drag. `x` is the pointer position in the
    /// window. The pane is at the right edge of the window.
    fn resize_detail(&mut self, x: Pixels, drag: &DetailResize, window: &Window, cx: &mut Context<Self>) {
        // The pane can close during the drag, for example when its
        // container goes away. The drag then changes nothing.
        if !self.detail_shown(cx) {
            return;
        }
        let window_width = window.viewport_size().width;
        // Use the sidebar width on the screen, because the sidebar can move
        // on its spring during the drag.
        let main_width = window_width - px(self.sidebar_spring.position.max(0.));
        // The center of the handle is on the left edge of the pane.
        let edge = x - drag.grab.get() + px(DETAIL_HANDLE_WIDTH / 2.);
        let width = (window_width - edge)
            .min(Self::detail_max_width(main_width))
            .max(px(DETAIL_MIN_WIDTH));
        if self.detail_width != Some(width) {
            self.detail_width = Some(width);
            cx.notify();
        }
    }

    /// The left position of the sidebar button in the window.
    fn sidebar_button_left(window: &Window) -> f32 {
        if window.is_fullscreen() {
            SIDEBAR_BUTTON_LEFT_FULLSCREEN
        } else {
            SIDEBAR_BUTTON_LEFT
        }
    }

    fn render_sidebar_button(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .left(px(Self::sidebar_button_left(window)))
            .top(px(SIDEBAR_BUTTON_TOP))
            // Keep the press away from the window drag areas below.
            .occlude()
            .child(
                Button::new("toggle-sidebar")
                    .icon(Icon::new(IconName::PanelLeft))
                    .custom(native::toolbar_button(cx))
                    .small()
                    .tooltip(if self.sidebar_collapsed {
                        "Show the sidebar (⌃⌘S)"
                    } else {
                        "Hide the sidebar (⌃⌘S)"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
            )
    }

    /// Connects the app to the engine of a Colima profile.
    fn open_machine(&mut self, name: &str, cx: &mut Context<Self>) {
        let socket = crate::docker::profile_socket(name);
        self.store.update(cx, |s, cx| s.switch_to(socket, cx));
    }

    /// Shows the actions of a Colima profile. They are the `colima`
    /// commands for one profile.
    fn show_machine_menu(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(machine) = self.machines.read(cx).machines.iter().find(|m| m.name == name).cloned() else {
            return;
        };
        let running = machine.is_running();
        let idle = !self.machines.read(cx).busy.contains(name);
        type Handler = Box<dyn Fn(&mut AppView, &mut Window, &mut Context<AppView>)>;
        fn item(label: &str, enabled: bool, handler: Handler) -> (MenuEntry, Option<Handler>) {
            (MenuEntry::Item { label: label.to_string().into(), enabled }, Some(handler))
        }
        let separator = || (MenuEntry::Separator, None);
        let action = |action: MachineAction| -> Handler {
            let name = machine.name.clone();
            Box::new(move |this, window, cx| this.request_machine_action(&name, action, window, cx))
        };
        let open_name = machine.name.clone();
        let mut entries = vec![
            item("Show Containers", true, Box::new(move |this, _, cx| this.open_machine(&open_name, cx))),
            separator(),
        ];
        if running {
            entries.push(item("Stop", idle, action(MachineAction::Stop)));
            entries.push(item("Restart", idle, action(MachineAction::Restart)));
            entries.push(item("Force Stop", idle, action(MachineAction::ForceStop)));
            entries.push(separator());
            if machine.kubernetes {
                entries.push(item("Stop Kubernetes", idle, action(MachineAction::StopKubernetes)));
                entries.push(item("Reset Kubernetes…", idle, action(MachineAction::ResetKubernetes)));
            } else {
                entries.push(item("Start Kubernetes", idle, action(MachineAction::StartKubernetes)));
            }
            entries.push(item("Update Container Runtime", idle, action(MachineAction::Update)));
        } else {
            entries.push(item("Start", idle, action(MachineAction::Start)));
        }
        entries.push(separator());
        let ssh_name = machine.name.clone();
        entries.push(item(
            "Open in Terminal",
            running,
            Box::new(move |_, window, cx| {
                if let Err(e) = colima::ssh_in_terminal(&ssh_name) {
                    window.push_notification(Notification::error(e.to_string()), cx);
                }
            }),
        ));
        if let Some(file) = machine.config_file() {
            entries.push(item(
                "Open Configuration File",
                true,
                Box::new(move |_, _, cx| cx.open_with_system(&file)),
            ));
        }
        let socket = crate::docker::profile_socket(&machine.name);
        entries.push(item(
            "Copy Docker Socket",
            true,
            Box::new(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(socket.clone()))),
        ));
        entries.push(separator());
        entries.push(item("Delete…", idle, action(MachineAction::Delete)));
        let (entries, handlers): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        native::show_context_menu(entries, window, cx, move |this, ix, window, cx| {
            if let Some(Some(handler)) = handlers.get(ix) {
                handler(this, window, cx);
            }
        });
    }

    /// Runs an action on a profile. A delete and a Kubernetes reset ask for
    /// confirmation first, because they cannot be undone.
    fn request_machine_action(
        &mut self,
        name: &str,
        action: MachineAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let machines = self.machines.clone();
        let (title, detail, button) = match action {
            MachineAction::Delete => (
                format!("Delete “{name}”?"),
                "Colima stops the machine and deletes it and its settings. The container data stays. You cannot undo this.",
                "Delete",
            ),
            MachineAction::ResetKubernetes => (
                format!("Reset Kubernetes on “{name}”?"),
                "Colima deletes the Kubernetes cluster and all its resources, and then makes a new cluster. You cannot undo this.",
                "Reset",
            ),
            _ => {
                machines.update(cx, |m, cx| m.run(name, action, cx));
                return;
            }
        };
        // The destructive button is not the default, so Return does not
        // start the action.
        let answer = window.prompt(
            PromptLevel::Warning,
            &title,
            Some(detail),
            &[PromptButton::Cancel("Cancel".into()), PromptButton::new(button)],
            cx,
        );
        let name = name.to_string();
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(1) {
                machines.update(cx, |m, cx| m.run(&name, action, cx));
            }
        })
        .detach();
    }

    /// Starts the machine of the engine. The machine store starts a listed
    /// profile, so the sidebar shows the progress too.
    fn start_engine(&mut self, cx: &mut Context<Self>) {
        let profile = self.store.read(cx).profile().map(str::to_string);
        let started = profile.is_some_and(|p| self.machines.update(cx, |m, cx| m.start(&p, cx)));
        if !started {
            self.store.update(cx, |s, cx| s.start_colima(cx));
        }
    }

    fn open_detail(&mut self, id: String, pointer: bool, window: &mut Window, cx: &mut Context<Self>) {
        // A new open stops an exit. The spring turns back from where it
        // is. A key opens the pane immediately, because keys are fast and
        // frequent. A change to a different container keeps the pane
        // where it is.
        self.closing_detail = None;
        if !pointer {
            self.detail_spring = SpringState { position: 1., velocity: 0. };
        }
        // A change to a different container keeps the selected tab.
        let tab = self.detail.as_ref().map(|d| d.read(cx).tab()).unwrap_or_default();
        let (store, stats) = (self.store.clone(), self.stats.clone());
        let detail = cx.new(|cx| ContainerDetail::new(id, store, stats, tab, window, cx));
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

    /// Closes the detail pane. A close by the pointer lets the pane go out
    /// to the right edge, the same path as its entrance. A key closes it
    /// immediately.
    fn close_detail(&mut self, pointer: bool, cx: &mut Context<Self>) {
        let Some(detail) = self.detail.take() else { return };
        self._detail_sub = None;
        if pointer && !cx.reduce_motion() {
            self.closing_detail = Some(detail);
        } else {
            self.closing_detail = None;
            self.detail_spring = SpringState { position: 0., velocity: 0. };
        }
        cx.notify();
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
                            .child(spinner())
                            .child(div().text_size(px(text::BODY)).child("Connecting to Docker…")),
                    ))
                    .into_any_element()
            }
            EngineStatus::NotRunning | EngineStatus::Error(_) => {
                let busy = store
                    .profile()
                    .is_some_and(|p| self.machines.read(cx).busy.contains(p));
                let starting = store.starting_colima || busy;
                // Only an error has body text. The stopped state is clear
                // from the title and the button.
                let detail = match &store.status {
                    EngineStatus::Error(e) => Some(e.clone()),
                    _ => None,
                };
                let t = cx.theme();
                empty_state(t)
                    .child(icon_tile(IconName::Power, t))
                    .child(empty_title("Colima is off", t))
                    .children(detail.map(|detail| {
                        div().max_w(px(360.)).text_center().text_size(px(text::BODY)).child(detail)
                    }))
                    .child(
                        div().pt_2().child(
                            Button::new("start-colima")
                                .primary()
                                .small()
                                .h(px(NATIVE_BUTTON_HEIGHT))
                                .icon(Icon::new(IconName::Play))
                                .when(starting, |b| b.icon(spinner()))
                                .label(if starting { "Starting…" } else { "Start" })
                                .loading(starting)
                                .on_click(cx.listener(|this, _, _, cx| this.start_engine(cx))),
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
        let detail_reveal = self.step_detail(window, cx);
        let window_width = window.viewport_size().width;
        let detail_width = self.detail_pane_width(window_width - sidebar_area);
        // The pane keeps its full width while it opens or closes. Only the
        // area that shows it changes, and the list gets the rest.
        let detail_area = detail_width * detail_reveal;
        let sidebar_width = self.sidebar_panel_width(window_width, cx);
        let main_width = window_width - sidebar_area;
        let list_width = main_width - detail_area;
        self.list.update(cx, |l, cx| l.set_width(list_width, cx));
        let t = cx.theme().clone();
        let view = cx.weak_entity();
        let machines = self.machines.read(cx);
        let open_view = view.clone();
        let menu_view = view.clone();
        let sidebar = Sidebar::new(self.section, sidebar_width, move |section, _, cx| {
            let _ = view.update(cx, |this, cx| {
                this.section = section;
                cx.notify();
            });
        })
        .machines(
            machines.machines.clone(),
            machines.busy.clone(),
            self.store.read(cx).profile().unwrap_or_default().to_string(),
            move |name, _, cx| {
                let _ = open_view.update(cx, |this, cx| this.open_machine(name, cx));
            },
            move |name, window, cx| {
                let _ = menu_view.update(cx, |this, cx| this.show_machine_menu(name, window, cx));
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
                .when_some(self.detail.clone().or_else(|| self.closing_detail.clone()), |d, detail| {
                    let open = self.detail.is_some();
                    // The pane sits at the left edge of its area, and the
                    // area cuts off the rest. The pane thus comes in from
                    // the right edge, where it lives, and the edge of the
                    // list moves with it.
                    let pane = div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .overflow_hidden()
                        .child(div().w(detail_width).h_full().child(detail));
                    d.child(
                        div()
                            .relative()
                            .w(detail_area)
                            .flex_shrink_0()
                            .h_full()
                            .child(pane)
                            .map(|d| {
                                if open {
                                    // The handle is on the border at the left
                                    // of the pane, and goes a small distance
                                    // into the list.
                                    d.child(
                                        div()
                                            .id("detail-resize")
                                            .absolute()
                                            .top_0()
                                            .left(px(-DETAIL_HANDLE_WIDTH / 2.))
                                            .w(px(DETAIL_HANDLE_WIDTH))
                                            .h_full()
                                            .occlude()
                                            .cursor_col_resize()
                                            .on_drag(
                                                DetailResize { grab: Rc::default() },
                                                |drag, offset, _, cx| {
                                                    drag.grab.set(offset.x);
                                                    cx.new(|_| ResizeDrag)
                                                },
                                            ),
                                    )
                                } else {
                                    // The pane that goes out takes no clicks.
                                    d.child(div().absolute().inset_0().occlude())
                                }
                            }),
                    )
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
                        .on_drag(
                            SidebarResize { start_width: self.sidebar_width, grab: Rc::default() },
                            |drag, offset, _, cx| {
                                drag.grab.set(offset.x);
                                cx.new(|_| ResizeDrag)
                            },
                        ),
                )
            });

        div()
            .relative()
            .flex()
            .size_full()
            .text_size(px(text::BODY))
            .text_color(t.foreground)
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_drag_move(cx.listener(|this, ev: &DragMoveEvent<SidebarResize>, window, cx| {
                let drag = ev.dragged_item().downcast_ref::<SidebarResize>();
                if let Some(drag) = drag {
                    this.resize_sidebar(ev.event.position.x, drag, window, cx)
                }
            }))
            .on_drag_move(cx.listener(|this, ev: &DragMoveEvent<DetailResize>, window, cx| {
                let drag = ev.dragged_item().downcast_ref::<DetailResize>();
                if let Some(drag) = drag {
                    this.resize_detail(ev.event.position.x, drag, window, cx)
                }
            }))
            .child(sidebar)
            .child(main)
            .child(self.render_sidebar_button(window, cx))
            // A resize drag keeps the resize cursor when the pointer leaves
            // the thin handle. The cover also stops hover effects and
            // tooltips below the pointer during the drag.
            .when(
                cx.active_drag_cursor_style() == Some(CursorStyle::ResizeColumn),
                |d| {
                    d.child(
                        div()
                            .id("resize-cover")
                            .absolute()
                            .inset_0()
                            .occlude()
                            .cursor_col_resize(),
                    )
                },
            )
    }
}

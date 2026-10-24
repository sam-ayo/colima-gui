//! The root view: the sidebar, the container, volume, image, or network
//! list, and the detail pane.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme, Icon, Sizable as _, Theme, v_flex};
use gpui_kit_assets::IconName;

use crate::colima::{self, MachineAction};
use crate::docker::EngineStatus;
use crate::state::container_store::{ComposeEvent, ContainerStore, StoreEvent};
use crate::state::image_store::{ImageEvent, ImageStore};
use crate::state::machine_store::{MachineEvent, MachineStore};
use crate::state::network_store::{NetworkEvent, NetworkStore};
use crate::state::stats_store::StatsStore;
use crate::state::usage_store::UsageStore;
use crate::state::volume_store::{VolumeEvent, VolumeStore};
use crate::ui::container_detail::{ContainerDetail, DetailEvent};
use crate::ui::container_list::{ContainerList, ListEvent, OpenComposeFile};
use crate::ui::image_detail::{ImageDetail, ImageDetailEvent};
use crate::ui::image_list::{ImageList, ImageListEvent};
use crate::ui::key_nav::{self, Move};
use crate::ui::network_detail::{NetworkDetail, NetworkDetailEvent};
use crate::ui::network_list::{NetworkList, NetworkListEvent};
use crate::ui::sidebar::{Section, Sidebar};
use crate::ui::toast::{Toast, Toaster};
use crate::ui::status_menu::{StatusMenu, StatusMenuEvent};
use crate::ui::volume_detail::{VolumeDetail, VolumeDetailEvent};
use crate::ui::volume_list::{VolumeList, VolumeListEvent};
use crate::ui::{
    NATIVE_BUTTON_HEIGHT, machine_form, native, native::MenuEntry, quit, text, toast,
    ENTER_DURATION, SIDEBAR_INSET, SIDEBAR_WIDTH, TOOLBAR_HEIGHT, delayed_reveal, drag_area, spinner,
};

actions!(
    app_view,
    [
        ToggleSidebar,
        SelectTab1,
        SelectTab2,
        SelectTab3,
        SelectTab4,
        ShowContainers,
        ShowVolumes,
        ShowImages,
        ShowNetworks,
        NewMachine
    ]
);

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
/// The time after the last scroll event until the hover effects show again.
const SCROLL_COVER_DELAY: Duration = Duration::from_millis(150);
/// The container list keeps this width when the sidebar or the detail pane
/// gets wider. At this width, the rows hide the ports column, and the row
/// actions still show. The minimum window width leaves this space next to
/// the smallest sidebar and detail pane.
const LIST_MIN_WIDTH: f32 = 320.;
/// The width of the resize handle at the left edge of the detail pane.
const DETAIL_HANDLE_WIDTH: f32 = 6.;
/// A drag past the minimum width pulls the pane out to the right, with
/// friction. The pull gets nearer to this limit, but never gets to it.
const DETAIL_PULL_LIMIT: f32 = 160.;
/// A drag this far past the minimum width closes the pane, as a drag past
/// the minimum width collapses the sidebar.
const DETAIL_CLOSE_DISTANCE: f32 = 100.;
/// A release of a pull at this speed or more closes the pane. The value is
/// in pane widths per second.
const DETAIL_FLICK_SPEED: f32 = 0.8;
/// A pointer that stays still for this time before the release has no
/// speed.
const DETAIL_FLICK_WINDOW: Duration = Duration::from_millis(60);
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

/// Tells the user that the VM of a profile cannot find hosts, so image
/// pulls fail. The toast stays until a later check finds hosts, or the
/// user closes it. A restart checks the DNS again.
fn show_dns_toast(profile: &str, works: bool, machines: &Entity<MachineStore>, cx: &mut App) {
    let id = format!("dns:{profile}");
    if works {
        toast::dismiss(&id, cx);
        return;
    }
    let machines = machines.downgrade();
    let name = profile.to_string();
    let toast = Toast::warning(format!("“{profile}” cannot find hosts on the internet"))
        .description("Image pulls will fail. A restart often fixes this after a change to the network or the VPN.")
        .id(id)
        .action("Restart", move |cx| {
            let _ = machines.update(cx, |m, cx| m.run(&name, MachineAction::Restart, cx));
        })
        .persist();
    toast::show(toast, cx);
}

/// One toast for each Compose action. The progress lines replace its
/// description, and the result replaces the toast.
fn show_compose_toast(ev: &ComposeEvent, cx: &mut App) {
    let toast = match ev {
        ComposeEvent::Progress { key, name, action, line } => {
            let verb = action.pending_label().trim_end_matches('…');
            let toast = Toast::loading(format!("{verb} “{name}”…")).id(format!("compose:{key}"));
            match line {
                Some(line) => toast.description(line.clone()),
                None => toast,
            }
        }
        ComposeEvent::Done { key, name, action, error: None } => {
            Toast::success(format!("{} “{name}”", action.done_label())).id(format!("compose:{key}"))
        }
        ComposeEvent::Done { key, name, action, error: Some(e) } => {
            Toast::error(format!("Could not {} “{name}”", action.verb()))
                .description(e.clone())
                .id(format!("compose:{key}"))
        }
    };
    toast::show(toast, cx);
}

pub fn bind_keys(cx: &mut App) {
    // The standard macOS shortcut for "Show Sidebar" and "Hide Sidebar".
    cx.bind_keys([KeyBinding::new("cmd-ctrl-s", ToggleSidebar, None)]);
    // The number keys show the tabs of the detail pane. A text field takes
    // the keys as text, so the bindings do not apply in a field.
    cx.bind_keys([
        KeyBinding::new("1", SelectTab1, Some("!Input")),
        KeyBinding::new("2", SelectTab2, Some("!Input")),
        KeyBinding::new("3", SelectTab3, Some("!Input")),
        KeyBinding::new("4", SelectTab4, Some("!Input")),
    ]);
    // Command and a number show a section, in the order of the sidebar, as
    // in the Finder and Mail.
    cx.bind_keys([
        KeyBinding::new("cmd-1", ShowContainers, None),
        KeyBinding::new("cmd-2", ShowVolumes, None),
        KeyBinding::new("cmd-3", ShowImages, None),
        KeyBinding::new("cmd-4", ShowNetworks, None),
    ]);
    cx.bind_keys([KeyBinding::new("cmd-o", OpenComposeFile, None)]);
    cx.bind_keys([KeyBinding::new("cmd-shift-n", NewMachine, None)]);
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
    /// The pane width that the user set at the start. A close by the drag
    /// keeps it.
    start_width: Option<Pixels>,
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
    /// The resource use of the VM of the current machine.
    usage: Entity<UsageStore>,
    list: Entity<ContainerList>,
    volumes: Entity<VolumeStore>,
    volume_list: Entity<VolumeList>,
    images: Entity<ImageStore>,
    image_list: Entity<ImageList>,
    networks: Entity<NetworkStore>,
    network_list: Entity<NetworkList>,
    detail: Option<Entity<ContainerDetail>>,
    volume_detail: Option<Entity<VolumeDetail>>,
    image_detail: Option<Entity<ImageDetail>>,
    network_detail: Option<Entity<NetworkDetail>>,
    /// The detail pane that goes out after a click on its close button.
    /// It stays until the spring closes it.
    closing_detail: Option<AnyView>,
    /// The part of the detail pane width that shows, from 0 to 1, and its
    /// speed. The list gets the space that the pane does not use.
    detail_spring: SpringState,
    /// The time of the last spring step, or `None` when the spring is at
    /// rest.
    detail_stepped_at: Option<Instant>,
    /// True while a resize drag moves the detail pane. The spring then
    /// follows the pointer. At the release, the spring continues from the
    /// speed of the pointer.
    detail_follows_pointer: bool,
    /// The time of the last move in a detail pane resize drag.
    detail_pulled_at: Option<Instant>,
    /// The pane width at the start of the last resize drag.
    detail_drag_start: Option<Pixels>,
    /// The pane width to use after a close by a drag. The pane keeps its
    /// width while it goes out, so it does not jump.
    detail_restore_width: Option<Option<Pixels>>,
    section: Section,
    /// The focus of the sidebar. The arrow keys then move `sidebar_cursor`
    /// through the sections and the machines.
    sidebar_focus: FocusHandle,
    sidebar_cursor: usize,
    /// The bounds of the sidebar row at the cursor, for a context menu that
    /// a key opens.
    sidebar_bounds: key_nav::BoundsCell,
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
    /// The engine status at the last change.
    engine_status: EngineStatus,
    /// The status that the main area showed at the last change, and the
    /// number of changes. Each change gets a new animation id, so the main
    /// area fades one time.
    shown_status: EngineStatus,
    engine_changes: usize,
    _status_menu: Entity<StatusMenu>,
    toaster: Entity<Toaster>,
    /// The pointer position at the last scroll event, and the timer that
    /// removes the scroll cover. It is `Some` while a scroll continues, and
    /// each scroll event starts the timer again.
    scroll_cover: Option<(Point<Pixels>, Task<()>)>,
    _subs: Vec<Subscription>,
    _detail_sub: Option<Subscription>,
    _volume_detail_sub: Option<Subscription>,
    _image_detail_sub: Option<Subscription>,
    _network_detail_sub: Option<Subscription>,
}

impl AppView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = cx.new(ContainerStore::new);
        let stats = cx.new(|_| StatsStore::new());
        let machines = cx.new(MachineStore::new);
        let usage = cx.new(|_| UsageStore::default());
        let list = cx.new(|cx| ContainerList::new(store.clone(), stats.clone(), window, cx));
        let volumes = cx.new(|cx| VolumeStore::new(store.clone(), cx));
        let volume_list = cx.new(|cx| VolumeList::new(volumes.clone(), store.clone(), window, cx));
        let images = cx.new(|cx| ImageStore::new(store.clone(), cx));
        let image_list = cx.new(|cx| ImageList::new(images.clone(), store.clone(), window, cx));
        let networks = cx.new(|cx| NetworkStore::new(store.clone(), cx));
        let network_list = cx.new(|cx| NetworkList::new(networks.clone(), store.clone(), window, cx));
        let handle = window.window_handle();
        let status_menu = cx.new(|cx| StatusMenu::new(store.clone(), handle, cx));
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
            cx.observe(&machines, |this, machines, cx| {
                // Load the rows of the other running machines, so a switch
                // to them shows rows at once.
                let sockets: Vec<_> = machines
                    .read(cx)
                    .machines
                    .iter()
                    .filter(|m| m.is_running())
                    .map(|m| crate::docker::profile_socket(&m.name))
                    .collect();
                // The Dock icon is dim when no machine runs.
                super::app_icon::set_running(!sockets.is_empty(), cx);
                this.store.update(cx, |s, cx| s.prefetch(sockets, cx));
                this.sync_usage(cx);
                cx.notify();
            }),
            cx.observe_global::<quit::Quitting>(|_, cx| cx.notify()),
            cx.observe(&usage, |_, _, cx| cx.notify()),
            cx.subscribe_in(&machines, window, |this, _, ev: &MachineEvent, _, cx| match ev {
                MachineEvent::Error(msg) => {
                    toast::show(Toast::error(msg.clone()), cx)
                }
                MachineEvent::Dns { profile, works } => show_dns_toast(profile, *works, &this.machines, cx),
                MachineEvent::Ready(profile) => {
                    if this.store.read(cx).profile() == Some(profile.as_str()) {
                        this.store.update(cx, |s, cx| s.wake(cx));
                    }
                }
            }),
            cx.subscribe_in(&store, window, |this, _, ev: &StoreEvent, _, cx| match ev {
                StoreEvent::Error(msg) => {
                    toast::show(Toast::error(msg.clone()), cx)
                }
                StoreEvent::Dns { profile, works } => show_dns_toast(profile, *works, &this.machines, cx),
                StoreEvent::Compose(ev) => show_compose_toast(ev, cx),
                StoreEvent::VolumesChanged | StoreEvent::ImagesChanged | StoreEvent::NetworksChanged => {}
                StoreEvent::Changed => {
                    let status = this.store.read(cx).status.clone();
                    let shown = this.store.read(cx).shown_status();
                    // Close the detail pane when the engine goes away.
                    if status != EngineStatus::Connected {
                        this.close_pane(Section::Containers, false, cx);
                        this.close_pane(Section::Volumes, false, cx);
                        this.close_pane(Section::Images, false, cx);
                        this.close_pane(Section::Networks, false, cx);
                    }
                    // A switch between two engines that both show rows does
                    // not fade the main area.
                    if std::mem::discriminant(&shown) != std::mem::discriminant(&this.shown_status) {
                        this.shown_status = shown;
                        this.engine_changes += 1;
                    }
                    if std::mem::discriminant(&status) != std::mem::discriminant(&this.engine_status) {
                        this.engine_status = status;
                        // The machine list is out of date when the engine
                        // comes or goes.
                        this.machines.update(cx, |m, cx| m.refresh(cx));
                    }
                    this.sync_usage(cx);
                    cx.notify();
                }
            }),
            cx.subscribe_in(&status_menu, window, |this, _, ev: &StatusMenuEvent, window, cx| match ev {
                StatusMenuEvent::Reveal(id) => this.reveal_container(id, window, cx),
            }),
            cx.subscribe_in(&volumes, window, |this, _, ev: &VolumeEvent, window, cx| match ev {
                VolumeEvent::Created(name) => {
                    this.volume_list.update(cx, |l, cx| l.reveal(name, cx));
                    this.volume_list.focus_handle(cx).focus(window, cx);
                }
                VolumeEvent::Error(msg) => {
                    toast::show(Toast::error(msg.clone()), cx)
                }
                VolumeEvent::Pruned(count, bytes) => {
                    let noun = if *count == 1 { "volume" } else { "volumes" };
                    let msg = if *count == 0 {
                        "No volumes were unused.".to_string()
                    } else {
                        format!(
                            "Removed {count} {noun}. {} is free.",
                            crate::docker::stats::format_bytes(*bytes)
                        )
                    };
                    toast::show(Toast::success(msg), cx)
                }
            }),
            cx.subscribe_in(&volume_list, window, |this, _, ev: &VolumeListEvent, window, cx| match ev {
                VolumeListEvent::RevealContainer(id) => this.reveal_container(id, window, cx),
                VolumeListEvent::Selected { name: Some(name), pointer } => {
                    this.open_volume_detail(name.clone(), *pointer, window, cx)
                }
                VolumeListEvent::Selected { name: None, pointer } => {
                    this.close_pane(Section::Volumes, *pointer, cx)
                }
            }),
            cx.subscribe_in(&networks, window, |this, _, ev: &NetworkEvent, window, cx| match ev {
                NetworkEvent::Created(name) => {
                    // Select the new network only in the network list, so the
                    // selection of a different section stays.
                    if this.section == Section::Networks {
                        this.network_list.update(cx, |l, cx| l.reveal(name, cx));
                        this.network_list.focus_handle(cx).focus(window, cx);
                    }
                }
                NetworkEvent::Error(msg) => {
                    toast::show(Toast::error(msg.clone()), cx)
                }
                NetworkEvent::Pruned(count) => {
                    let msg = match count {
                        0 => "No networks were unused.".to_string(),
                        1 => "Removed 1 network.".to_string(),
                        n => format!("Removed {n} networks."),
                    };
                    toast::show(Toast::success(msg), cx)
                }
            }),
            cx.subscribe_in(&network_list, window, |this, _, ev: &NetworkListEvent, window, cx| match ev {
                NetworkListEvent::RevealContainer(id) => this.reveal_container(id, window, cx),
                NetworkListEvent::Selected { name: Some(name), pointer } => {
                    this.open_network_detail(name.clone(), *pointer, window, cx)
                }
                NetworkListEvent::Selected { name: None, pointer } => {
                    this.close_pane(Section::Networks, *pointer, cx)
                }
            }),
            cx.subscribe_in(&images, window, |this, _, ev: &ImageEvent, window, cx| match ev {
                ImageEvent::Pulled(reference) => {
                    toast::show(Toast::success(format!("Pulled {reference}")), cx);
                    // Select the new image only in the image list, so a pull
                    // does not take the selection of a different section.
                    if this.section == Section::Images {
                        this.image_list.update(cx, |l, cx| l.reveal(reference, cx));
                        this.image_list.focus_handle(cx).focus(window, cx);
                    }
                }
                ImageEvent::Error(msg) => {
                    toast::show(Toast::error(msg.clone()), cx)
                }
                ImageEvent::Pruned(count, bytes) => {
                    let noun = if *count == 1 { "image" } else { "images" };
                    let msg = if *count == 0 {
                        "No images were unused.".to_string()
                    } else {
                        format!(
                            "Removed {count} {noun}. {} is free.",
                            crate::docker::stats::format_bytes(*bytes)
                        )
                    };
                    toast::show(Toast::success(msg), cx)
                }
            }),
            cx.subscribe_in(&image_list, window, |this, _, ev: &ImageListEvent, window, cx| match ev {
                ImageListEvent::RevealContainer(id) => this.reveal_container(id, window, cx),
                ImageListEvent::Selected { key: Some(key), pointer } => {
                    this.open_image_detail(key.clone(), *pointer, window, cx)
                }
                ImageListEvent::Selected { key: None, pointer } => {
                    this.close_pane(Section::Images, *pointer, cx)
                }
            }),
            cx.subscribe_in(&list, window, |this, _, ev: &ListEvent, window, cx| match ev {
                ListEvent::Selected { id: Some(id), pointer } => {
                    this.open_detail(id.clone(), *pointer, window, cx)
                }
                ListEvent::Selected { id: None, pointer } => this.close_pane(Section::Containers, *pointer, cx),
            }),
        ];
        // The close button hides the window. The app, its status bar item,
        // and Colima continue to run. While Colima stops before a quit, the
        // window stays, because it shows the progress.
        window.on_window_should_close(cx, |window, cx| {
            if !quit::is_quitting(cx) {
                native::hide_window(window);
            }
            false
        });
        // The shortcut and the menu item work when no element of the view
        // has the focus, for example when the engine is off. An action
        // handler on the view gets only the actions on the focus path.
        let view = cx.weak_entity();
        let handle = window.window_handle();
        App::on_action(cx, move |_: &NewMachine, cx: &mut App| {
            let view = view.clone();
            // A menu action can come while GPUI updates the window. Open the
            // form after the current update.
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    window.activate_window();
                    let _ = view.update(cx, |this, cx| this.new_machine(false, window, cx));
                });
            });
        });
        list.focus_handle(cx).focus(window, cx);
        AppView {
            store,
            stats,
            machines,
            usage,
            list,
            volumes,
            volume_list,
            images,
            image_list,
            networks,
            network_list,
            detail: None,
            volume_detail: None,
            image_detail: None,
            network_detail: None,
            closing_detail: None,
            detail_spring: SpringState { position: 0., velocity: 0. },
            detail_stepped_at: None,
            detail_follows_pointer: false,
            detail_pulled_at: None,
            detail_drag_start: None,
            detail_restore_width: None,
            section: Section::Containers,
            sidebar_focus: cx.focus_handle().tab_stop(true),
            sidebar_cursor: 0,
            sidebar_bounds: key_nav::BoundsCell::default(),
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
            shown_status: EngineStatus::Connecting,
            engine_changes: 0,
            _status_menu: status_menu,
            toaster: cx.new(|cx| Toaster::new(window, cx)),
            scroll_cover: None,
            _subs: subs,
            _detail_sub: None,
            _volume_detail_sub: None,
            _image_detail_sub: None,
            _network_detail_sub: None,
        }
    }

    /// Moves the focus to the list of the current section.
    fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.section {
            Section::Containers => self.list.focus_handle(cx).focus(window, cx),
            Section::Volumes => self.volume_list.focus_handle(cx).focus(window, cx),
            Section::Images => self.image_list.focus_handle(cx).focus(window, cx),
            Section::Networks => self.network_list.focus_handle(cx).focus(window, cx),
        }
    }

    /// Moves the focus to the sidebar, with the cursor on the current
    /// section. A collapsed sidebar opens.
    fn focus_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sidebar_collapsed(false, cx);
        // The cursor skips the sections when they are disabled.
        self.sidebar_cursor = if self.machines.read(cx).all_off() {
            Section::ALL.len()
        } else {
            Section::ALL.iter().position(|&s| s == self.section).unwrap_or(0)
        };
        self.sidebar_focus.focus(window, cx);
        cx.notify();
    }

    /// Moves the focus into the detail pane of the current section, if a
    /// pane is open.
    fn focus_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.section {
            Section::Containers => {
                if let Some(d) = &self.detail {
                    d.update(cx, |d, cx| d.focus_in(window, cx));
                }
            }
            Section::Volumes => {
                if let Some(d) = &self.volume_detail {
                    d.update(cx, |d, cx| d.focus_in(window, cx));
                }
            }
            Section::Images => {
                if let Some(d) = &self.image_detail {
                    d.update(cx, |d, cx| d.focus_in(window, cx));
                }
            }
            Section::Networks => {
                if let Some(d) = &self.network_detail {
                    d.update(cx, |d, cx| d.focus_in(window, cx));
                }
            }
        }
    }

    /// The number of sidebar rows: the sections, then the machines.
    fn sidebar_rows(&self, cx: &App) -> usize {
        Section::ALL.len() + self.machines.read(cx).machines.len()
    }

    fn move_sidebar(&mut self, m: Move, cx: &mut Context<Self>) {
        // The cursor skips the sections when they are disabled.
        let first = if self.machines.read(cx).all_off() { Section::ALL.len() } else { 0 };
        let current = self.sidebar_cursor.checked_sub(first);
        if let Some(ix) = m.apply(current, self.sidebar_rows(cx) - first) {
            self.sidebar_cursor = first + ix;
            cx.notify();
        }
    }

    /// The name of the machine at the sidebar cursor.
    fn machine_at_cursor(&self, cx: &App) -> Option<String> {
        let ix = self.sidebar_cursor.checked_sub(Section::ALL.len())?;
        self.machines.read(cx).machines.get(ix).map(|m| m.name.clone())
    }

    /// Does the action of a click on the sidebar row at the cursor. A
    /// section shows, and its list gets the focus. A machine shows its
    /// containers.
    fn activate_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(&section) = Section::ALL.get(self.sidebar_cursor) {
            self.set_section(section, window, cx);
        } else if let Some(name) = self.machine_at_cursor(cx) {
            self.open_machine(&name, cx);
        }
    }

    fn show_sidebar_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(name) = self.machine_at_cursor(cx) {
            let at = key_nav::menu_point(self.sidebar_bounds.get());
            self.show_machine_menu(&name, at, window, cx);
        }
    }

    /// Shows the next tab of the detail pane, or the previous tab for a
    /// negative `delta`. The tabs wrap around.
    fn cycle_detail_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        match self.section {
            Section::Containers => {
                if let Some(d) = &self.detail {
                    d.update(cx, |d, cx| d.cycle_tab(delta, window, cx));
                }
            }
            Section::Volumes => {
                if let Some(d) = &self.volume_detail {
                    d.update(cx, |d, cx| d.cycle_tab(delta, cx));
                }
            }
            Section::Images => {
                if let Some(d) = &self.image_detail {
                    d.update(cx, |d, cx| d.cycle_tab(delta, cx));
                }
            }
            Section::Networks => {
                if let Some(d) = &self.network_detail {
                    d.update(cx, |d, cx| d.cycle_tab(delta, cx));
                }
            }
        }
    }

    /// Shows the tab at a position, from 0, in the detail pane of the
    /// current section.
    fn set_detail_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.section {
            Section::Containers => {
                if let Some(d) = &self.detail {
                    d.update(cx, |d, cx| d.set_tab_at(ix, window, cx));
                }
            }
            Section::Volumes => {
                if let Some(d) = &self.volume_detail {
                    d.update(cx, |d, cx| d.set_tab_at(ix, cx));
                }
            }
            Section::Images => {
                if let Some(d) = &self.image_detail {
                    d.update(cx, |d, cx| d.set_tab_at(ix, cx));
                }
            }
            Section::Networks => {
                if let Some(d) = &self.network_detail {
                    d.update(cx, |d, cx| d.set_tab_at(ix, cx));
                }
            }
        }
    }

    /// Shows a section, and moves the focus to its list.
    /// Asks for a Compose file, and creates and starts its services. The
    /// list shows the containers when Compose creates them.
    fn open_compose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Compose Up".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                this.set_section(Section::Containers, window, cx);
                this.store.update(cx, |s, cx| s.compose_up_file(&path, cx));
            });
        })
        .detach();
    }

    fn set_section(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        // The sections are disabled when no machine runs.
        if self.machines.read(cx).all_off() {
            return;
        }
        self.section = section;
        if section == Section::Volumes {
            self.volumes.update(cx, |v, cx| v.activate(cx));
        }
        if section == Section::Images {
            self.images.update(cx, |i, cx| i.activate(cx));
        }
        if section == Section::Networks {
            self.networks.update(cx, |n, cx| n.activate(cx));
        }
        self.focus_list(window, cx);
        if section != Section::Volumes {
            self.volumes.update(cx, |v, _| v.deactivate());
        }
        // Each section keeps its own pane. The pane of the new section shows
        // at once, with no spring, as the list does.
        self.closing_detail = None;
        let position = if self.pane().is_some() { 1. } else { 0. };
        self.detail_spring = SpringState { position, velocity: 0. };
        self.detail_stepped_at = None;
        cx.notify();
    }

    /// The detail pane of the current section.
    fn pane(&self) -> Option<AnyView> {
        match self.section {
            Section::Containers => self.detail.clone().map(Into::into),
            Section::Volumes => self.volume_detail.clone().map(Into::into),
            Section::Images => self.image_detail.clone().map(Into::into),
            Section::Networks => self.network_detail.clone().map(Into::into),
        }
    }

    /// Shows a container in the container list, and selects it.
    fn reveal_container(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_section(Section::Containers, window, cx);
        self.list.update(cx, |l, cx| l.reveal(id, cx));
    }

    /// True when the main area shows the detail pane.
    fn detail_shown(&self, cx: &App) -> bool {
        self.pane().is_some()
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
        if self.volume_list.read(cx).leading_inset() != inset {
            self.volume_list.update(cx, |l, cx| l.set_leading_inset(inset, cx));
        }
        if self.image_list.read(cx).leading_inset() != inset {
            self.image_list.update(cx, |l, cx| l.set_leading_inset(inset, cx));
        }
        if self.network_list.read(cx).leading_inset() != inset {
            self.network_list.update(cx, |l, cx| l.set_leading_inset(inset, cx));
        }
        width
    }

    /// Moves the detail spring one frame toward the target, and returns
    /// the part of the pane width that shows in this frame. At the end of
    /// a close, the pane that goes out is dropped.
    fn step_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) -> f32 {
        let now = Instant::now();
        if self.detail_follows_pointer && !cx.has_active_drag() {
            // The resize drag ended. A pointer that stopped before the
            // release has no speed. A fast release of a pull closes the
            // pane, and a slow release lets the spring bring it back.
            self.detail_follows_pointer = false;
            let still = self.detail_pulled_at.is_none_or(|at| now - at > DETAIL_FLICK_WINDOW);
            if still {
                self.detail_spring.velocity = 0.;
            }
            if self.pane().is_some()
                && self.detail_spring.position < 1.
                && self.detail_spring.velocity < -DETAIL_FLICK_SPEED
            {
                self.close_detail_by_drag(self.detail_drag_start, cx);
            }
        }
        if self.detail_follows_pointer && self.pane().is_some() {
            self.detail_stepped_at = None;
            return self.detail_spring.position.clamp(0., 1.);
        }
        let target = if self.pane().is_some() { 1. } else { 0. };
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
            if let Some(width) = self.detail_restore_width.take() {
                self.detail_width = width;
            }
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
        let pointer_width = f32::from(window_width - edge);
        let width = px(pointer_width)
            .min(Self::detail_max_width(main_width))
            .max(px(DETAIL_MIN_WIDTH));
        self.detail_drag_start = drag.start_width;

        // Past the minimum width, the pane goes out to the right with
        // friction. The more the drag goes, the less the pane moves.
        let past = (DETAIL_MIN_WIDTH - pointer_width).max(0.);
        let pull = past * DETAIL_PULL_LIMIT / (past + DETAIL_PULL_LIMIT);
        let position = 1. - pull / f32::from(width);

        // Measure the speed of the pane, so a close or a release continues
        // the movement of the pointer. The average keeps it smooth when
        // the moves come fast.
        let now = Instant::now();
        let velocity = match self.detail_pulled_at {
            Some(at) if self.detail_follows_pointer => {
                let elapsed = now.duration_since(at).as_secs_f32().max(0.001);
                let measured = (position - self.detail_spring.position) / elapsed;
                0.5 * measured + 0.5 * self.detail_spring.velocity
            }
            _ => 0.,
        };
        self.detail_pulled_at = Some(now);
        self.detail_follows_pointer = true;
        self.detail_spring = SpringState { position, velocity };

        if past >= DETAIL_CLOSE_DISTANCE {
            self.close_detail_by_drag(drag.start_width, cx);
            return;
        }
        self.detail_width = Some(width);
        cx.notify();
    }

    /// Closes the detail pane at the end of a pull. The spring continues
    /// from the position and the speed of the pull, so the pane does not
    /// stop or jump. The pane opens again at `start_width`, the width from
    /// before the drag.
    fn close_detail_by_drag(&mut self, start_width: Option<Pixels>, cx: &mut Context<Self>) {
        self.detail_follows_pointer = false;
        // The pane goes out at least as fast as the pull.
        self.detail_spring.velocity = self.detail_spring.velocity.min(0.);
        self.detail_restore_width = Some(start_width);
        let section = self.section;
        self.close_pane(section, true, cx);
        // The list clears its selection, as after a click on the close
        // button. Its event then finds no pane to close.
        match section {
            Section::Containers => self.list.update(cx, |l, cx| l.clear_selection(cx)),
            Section::Volumes => self.volume_list.update(cx, |l, cx| l.clear_selection(cx)),
            Section::Images => self.image_list.update(cx, |l, cx| l.clear_selection(cx)),
            Section::Networks => self.network_list.update(cx, |l, cx| l.clear_selection(cx)),
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

    /// Reads the resource use of the current machine while it runs. A
    /// machine with a command in progress can stop at any time, so the
    /// reads wait until the command ends.
    fn sync_usage(&mut self, cx: &mut Context<Self>) {
        let profile = self.store.read(cx).profile().map(str::to_string);
        let machines = self.machines.read(cx);
        let running: Vec<String> = machines
            .machines
            .iter()
            .filter(|m| m.is_running() && !machines.busy.contains(&m.name))
            .map(|m| m.name.clone())
            .collect();
        let watched = profile.filter(|p| running.contains(p));
        self.usage.update(cx, |u, cx| {
            u.retain_running(|p| running.iter().any(|r| r == p));
            u.watch(watched, cx)
        });
    }

    /// Opens the form for a new machine. `animate` is false for an open
    /// from the keyboard or the menu bar.
    fn new_machine(&mut self, animate: bool, window: &mut Window, cx: &mut Context<Self>) {
        machine_form::open_new(self.machines.clone(), animate, window, cx);
    }

    /// Shows the actions of a Colima profile. They are the `colima`
    /// commands for one profile.
    fn show_machine_menu(&mut self, name: &str, at: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
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
            entries.push(item("Update Container Runtime", idle, action(MachineAction::Update)));
        } else {
            entries.push(item("Start", idle, action(MachineAction::Start)));
        }
        entries.push(separator());
        let ssh_name = machine.name.clone();
        entries.push(item(
            "Open in Terminal",
            running,
            Box::new(move |_, _, cx| {
                if let Err(e) = colima::ssh_in_terminal(&ssh_name) {
                    toast::show(Toast::error(e.to_string()), cx);
                }
            }),
        ));
        let settings = machine.settings();
        let settings_name = machine.name.clone();
        entries.push(item(
            "Settings…",
            idle && settings.is_some(),
            Box::new(move |this, window, cx| {
                let Some(settings) = settings.clone() else { return };
                machine_form::open_edit(settings_name.clone(), settings, running, this.machines.clone(), window, cx);
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
            Box::new(move |_, _, cx| toast::copy(socket.clone(), socket.clone(), cx)),
        ));
        entries.push(separator());
        entries.push(item("Delete…", idle, action(MachineAction::Delete)));
        entries.push(item("Delete with Data…", idle, action(MachineAction::DeleteWithData)));
        let (entries, handlers): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        native::show_context_menu_at(entries, at, window, cx, move |this, ix, window, cx| {
            if let Some(Some(handler)) = handlers.get(ix) {
                handler(this, window, cx);
            }
        });
    }

    /// Runs an action on a profile. A delete asks for confirmation first,
    /// because it cannot be undone.
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
            MachineAction::DeleteWithData => (
                format!("Delete “{name}” and its data?"),
                "Colima stops the machine and deletes it, its settings, and all its container data: the images, the containers, and the volumes. You cannot undo this.",
                "Delete with Data",
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

    /// Shows the scroll cover, and removes it when no scroll event comes for
    /// a short time. The rows below the pointer then show no hover fill while
    /// they move.
    fn cover_scroll(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.scroll_cover.is_none() {
            cx.notify();
        }
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SCROLL_COVER_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                this.scroll_cover = None;
                cx.notify();
            });
        });
        self.scroll_cover = Some((position, timer));
    }

    /// Removes the scroll cover when the pointer moves. The row below the
    /// pointer then shows its hover fill again. A move event at the position
    /// of the scroll does not remove the cover.
    fn uncover_scroll(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.scroll_cover.as_ref().is_some_and(|(at, _)| *at != position) {
            self.scroll_cover = None;
            cx.notify();
        }
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
        if let Some(width) = self.detail_restore_width.take() {
            self.detail_width = width;
        }
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
                    toast::show(Toast::error(msg.clone()), cx)
                }
                DetailEvent::RevealVolume(name) => {
                    this.set_section(Section::Volumes, window, cx);
                    this.volume_list.update(cx, |l, cx| l.reveal(name, cx));
                }
                DetailEvent::RevealImage(id) => {
                    this.set_section(Section::Images, window, cx);
                    this.image_list.update(cx, |l, cx| l.reveal(id, cx));
                }
                DetailEvent::RevealNetwork(name) => {
                    this.set_section(Section::Networks, window, cx);
                    this.network_list.update(cx, |l, cx| l.reveal(name, cx));
                }
            }
        }));
        self.detail = Some(detail);
        cx.notify();
    }

    /// Closes the detail pane of a section. A close by the pointer lets the
    /// pane go out to the right edge, the same path as its entrance. A key
    /// closes it immediately.
    fn close_pane(&mut self, section: Section, pointer: bool, cx: &mut Context<Self>) {
        let pane: AnyView = match section {
            Section::Containers => match self.detail.take() {
                Some(d) => {
                    self._detail_sub = None;
                    d.into()
                }
                None => return,
            },
            Section::Volumes => match self.volume_detail.take() {
                Some(d) => {
                    self._volume_detail_sub = None;
                    d.into()
                }
                None => return,
            },
            Section::Images => match self.image_detail.take() {
                Some(d) => {
                    self._image_detail_sub = None;
                    d.into()
                }
                None => return,
            },
            Section::Networks => match self.network_detail.take() {
                Some(d) => {
                    self._network_detail_sub = None;
                    d.into()
                }
                None => return,
            },
        };
        // The pane of a hidden section goes away with no exit.
        if section != self.section {
            return;
        }
        if pointer && !cx.reduce_motion() {
            self.closing_detail = Some(pane);
        } else {
            self.closing_detail = None;
            self.detail_spring = SpringState { position: 0., velocity: 0. };
        }
        cx.notify();
    }

    fn open_volume_detail(&mut self, name: String, pointer: bool, window: &mut Window, cx: &mut Context<Self>) {
        // The pane moves as the container pane does. See `open_detail`.
        self.closing_detail = None;
        if let Some(width) = self.detail_restore_width.take() {
            self.detail_width = width;
        }
        if !pointer {
            self.detail_spring = SpringState { position: 1., velocity: 0. };
        }
        let tab = self.volume_detail.as_ref().map(|d| d.read(cx).tab()).unwrap_or_default();
        let (volumes, store) = (self.volumes.clone(), self.store.clone());
        let detail = cx.new(|cx| VolumeDetail::new(name, volumes, store, tab, cx));
        self._volume_detail_sub = Some(cx.subscribe_in(
            &detail,
            window,
            |this, _, ev: &VolumeDetailEvent, window, cx| match ev {
                VolumeDetailEvent::Close => this.volume_list.update(cx, |l, cx| l.clear_selection(cx)),
                VolumeDetailEvent::RevealContainer(id) => this.reveal_container(id, window, cx),
                VolumeDetailEvent::Remove(name) => {
                    this.volume_list.update(cx, |l, cx| l.request_remove(name.clone(), window, cx))
                }
            },
        ));
        self.volume_detail = Some(detail);
        cx.notify();
    }

    fn open_image_detail(&mut self, key: String, pointer: bool, window: &mut Window, cx: &mut Context<Self>) {
        // The pane moves as the container pane does. See `open_detail`.
        self.closing_detail = None;
        if let Some(width) = self.detail_restore_width.take() {
            self.detail_width = width;
        }
        if !pointer {
            self.detail_spring = SpringState { position: 1., velocity: 0. };
        }
        let tab = self.image_detail.as_ref().map(|d| d.read(cx).tab()).unwrap_or_default();
        let (images, store) = (self.images.clone(), self.store.clone());
        let detail = cx.new(|cx| ImageDetail::new(key, images, store, tab, cx));
        self._image_detail_sub = Some(cx.subscribe_in(
            &detail,
            window,
            |this, _, ev: &ImageDetailEvent, window, cx| match ev {
                ImageDetailEvent::Close => this.image_list.update(cx, |l, cx| l.clear_selection(cx)),
                ImageDetailEvent::RevealContainer(id) => this.reveal_container(id, window, cx),
                ImageDetailEvent::Remove(key) => {
                    this.image_list.update(cx, |l, cx| l.request_remove(key.clone(), window, cx))
                }
            },
        ));
        self.image_detail = Some(detail);
        cx.notify();
    }

    fn open_network_detail(&mut self, name: String, pointer: bool, window: &mut Window, cx: &mut Context<Self>) {
        // The pane moves as the container pane does. See `open_detail`.
        self.closing_detail = None;
        if let Some(width) = self.detail_restore_width.take() {
            self.detail_width = width;
        }
        if !pointer {
            self.detail_spring = SpringState { position: 1., velocity: 0. };
        }
        let tab = self.network_detail.as_ref().map(|d| d.read(cx).tab()).unwrap_or_default();
        let (networks, store) = (self.networks.clone(), self.store.clone());
        let detail = cx.new(|cx| NetworkDetail::new(name, networks, store, tab, cx));
        self._network_detail_sub = Some(cx.subscribe_in(
            &detail,
            window,
            |this, _, ev: &NetworkDetailEvent, window, cx| match ev {
                NetworkDetailEvent::Close => this.network_list.update(cx, |l, cx| l.clear_selection(cx)),
                NetworkDetailEvent::RevealContainer(id) => this.reveal_container(id, window, cx),
                NetworkDetailEvent::Remove(name) => {
                    this.network_list.update(cx, |l, cx| l.request_remove(name.clone(), window, cx))
                }
            },
        ));
        self.network_detail = Some(detail);
        cx.notify();
    }

    fn render_engine_state(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let store = self.store.read(cx);
        let el = match &store.shown_status() {
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
                // Other machines can run, so the title names this machine.
                let title = match store.profile() {
                    Some(p) => format!("{p} is off"),
                    None => "Colima is off".into(),
                };
                let t = cx.theme();
                empty_state(t)
                    .child(icon_tile(IconName::Power, t))
                    .child(empty_title(title, t))
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

fn empty_title(text: impl Into<SharedString>, t: &Theme) -> impl IntoElement {
    div()
        .text_size(px(text::TITLE2))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.foreground)
        .child(text.into())
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
        let hover_view = view.clone();
        let new_view = view.clone();
        let sidebar = Sidebar::new(self.section, sidebar_width, move |section, window, cx| {
            let _ = view.update(cx, |this, cx| this.set_section(section, window, cx));
        })
        .machines(
            machines.machines.clone(),
            machines.busy.clone(),
            self.store.read(cx).profile().unwrap_or_default().to_string(),
            move |name, _, cx| {
                let _ = open_view.update(cx, |this, cx| this.open_machine(name, cx));
            },
            move |name, window, cx| {
                let _ = menu_view.update(cx, |this, cx| this.show_machine_menu(name, None, window, cx));
            },
        )
        .on_new_machine({
            let view = new_view.clone();
            move |window, cx| {
                let _ = view.update(cx, |this, cx| this.new_machine(true, window, cx));
            }
        })
        .usage(self.usage.clone())
        .cursor(
            self.sidebar_focus.is_focused(window).then_some(self.sidebar_cursor),
            self.sidebar_bounds.clone(),
            move |ix, _, cx| {
                let _ = hover_view.update(cx, |this, cx| {
                    if this.sidebar_cursor != ix {
                        this.sidebar_cursor = ix;
                        cx.notify();
                    }
                });
            },
        );

        let main = if let Some(state) = self.render_engine_state(cx) {
            with_drag_area(state)
        } else {
            let list: AnyView = match self.section {
                Section::Volumes => self.volume_list.clone().into(),
                Section::Images => self.image_list.clone().into(),
                Section::Networks => self.network_list.clone().into(),
                _ => self.list.clone().into(),
            };
            let pane = self.pane();
            let open = pane.is_some();
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .h_full()
                // The cached views render again only after their own
                // notify, so a log line or a stats tick in the detail pane
                // does not render the list again.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(list.cached(StyleRefinement::default().size_full())),
                )
                .when_some(pane.or_else(|| self.closing_detail.clone()), |d, detail| {
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
                        .child(
                            div()
                                .w(detail_width)
                                .h_full()
                                .child(detail.cached(StyleRefinement::default().size_full())),
                        );
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
                                                DetailResize {
                                                    start_width: self.detail_width,
                                                    grab: Rc::default(),
                                                },
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
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .key_context(key_nav::SIDEBAR)
                    .track_focus(&self.sidebar_focus)
                    .on_action(cx.listener(|this, _: &key_nav::MoveUp, _, cx| this.move_sidebar(Move::By(-1), cx)))
                    .on_action(cx.listener(|this, _: &key_nav::MoveDown, _, cx| this.move_sidebar(Move::By(1), cx)))
                    .on_action(cx.listener(|this, _: &key_nav::MoveFirst, _, cx| this.move_sidebar(Move::First, cx)))
                    .on_action(cx.listener(|this, _: &key_nav::MoveLast, _, cx| this.move_sidebar(Move::Last, cx)))
                    .on_action(cx.listener(|this, _: &key_nav::PageUp, _, cx| this.move_sidebar(Move::First, cx)))
                    .on_action(cx.listener(|this, _: &key_nav::PageDown, _, cx| this.move_sidebar(Move::Last, cx)))
                    .on_action(cx.listener(|this, _: &key_nav::Activate, w, cx| this.activate_sidebar(w, cx)))
                    .on_action(cx.listener(|this, _: &key_nav::MoveRight, w, cx| this.focus_list(w, cx)))
                    .on_action(cx.listener(|this, _: &key_nav::ShowMenu, w, cx| this.show_sidebar_menu(w, cx)))
                    .child(sidebar),
            )
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
            // The window has no focus after some clicks. The context keeps
            // the "!Input" bindings active then, because a binding needs a
            // context to match.
            .key_context("AppView")
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &SelectTab1, w, cx| this.set_detail_tab(0, w, cx)))
            .on_action(cx.listener(|this, _: &SelectTab2, w, cx| this.set_detail_tab(1, w, cx)))
            .on_action(cx.listener(|this, _: &SelectTab3, w, cx| this.set_detail_tab(2, w, cx)))
            .on_action(cx.listener(|this, _: &SelectTab4, w, cx| this.set_detail_tab(3, w, cx)))
            .on_action(cx.listener(|this, _: &key_nav::NextTab, w, cx| this.cycle_detail_tab(1, w, cx)))
            .on_action(cx.listener(|this, _: &key_nav::PrevTab, w, cx| this.cycle_detail_tab(-1, w, cx)))
            .on_action(cx.listener(|_, _: &key_nav::FocusNext, w, cx| w.focus_next(cx)))
            .on_action(cx.listener(|_, _: &key_nav::FocusPrev, w, cx| w.focus_prev(cx)))
            .on_action(cx.listener(|this, _: &key_nav::FocusSidebar, w, cx| this.focus_sidebar(w, cx)))
            .on_action(cx.listener(|this, _: &key_nav::FocusList, w, cx| this.focus_list(w, cx)))
            .on_action(cx.listener(|this, _: &key_nav::FocusDetail, w, cx| this.focus_detail(w, cx)))
            .on_action(cx.listener(|this, _: &ShowContainers, w, cx| this.set_section(Section::Containers, w, cx)))
            .on_action(cx.listener(|this, _: &ShowVolumes, w, cx| this.set_section(Section::Volumes, w, cx)))
            .on_action(cx.listener(|this, _: &ShowImages, w, cx| this.set_section(Section::Images, w, cx)))
            .on_action(cx.listener(|this, _: &ShowNetworks, w, cx| this.set_section(Section::Networks, w, cx)))
            .on_action(cx.listener(|this, _: &OpenComposeFile, w, cx| this.open_compose_file(w, cx)))
            .on_action(cx.listener(|_, _: &native::CloseWindow, _, cx| {
                if !quit::is_quitting(cx) {
                    cx.propagate();
                }
            }))
            // A click on an area that cannot take the focus removes the
            // focus from a search or filter field. The focus goes to the
            // list, so the list shortcuts work. A focusable element below
            // the pointer prevents the default, and keeps its focus.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !window.default_prevented() {
                        this.focus_list(window, cx);
                    }
                }),
            )
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
            // A scroll event goes to the window below the scroll cover, so
            // this listener gets each event of the scroll.
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                this.cover_scroll(ev.position, cx)
            }))
            .child(sidebar)
            .child(main)
            .child(self.render_sidebar_button(window, cx))
            // The scroll cover stops the hover effects and the tooltips
            // below the pointer while the content moves. The scroll events
            // go through the cover to the scroll areas below it.
            .when(self.scroll_cover.is_some(), |d| {
                d.child(
                    div()
                        .id("scroll-cover")
                        .absolute()
                        .inset_0()
                        .block_mouse_except_scroll()
                        .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                            this.uncover_scroll(ev.position, cx)
                        })),
                )
            })
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
            .when(quit::is_quitting(cx), |d| d.child(render_quitting(cx.theme())))
            .child(self.toaster.clone())
    }
}

/// Covers the window while Colima stops before the app quits.
fn render_quitting(t: &Theme) -> impl IntoElement {
    div()
        .id("quitting-cover")
        .absolute()
        .inset_0()
        .occlude()
        .bg(t.background.opacity(0.9))
        .flex()
        .child(
            empty_state(t)
                .pb_0()
                .child(spinner())
                .child(empty_title("Stopping Colima…", t))
                .child(
                    div()
                        .text_size(px(text::BODY))
                        .child(format!("{} quits when Colima stops.", native::APP_NAME)),
                ),
        )
}

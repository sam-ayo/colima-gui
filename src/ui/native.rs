//! The parts of macOS that GPUI does not give: the system colors, the menu
//! bar, the context menus of AppKit, and the saved window frame.
//!
//! On other platforms, the theme keeps its colors, and the context menus
//! do not open.

use gpui::*;
use gpui_component::{Theme, input};
use serde::Deserialize;

use super::text;

actions!(
    native,
    [
        About,
        CheckForUpdates,
        Hide,
        HideOthers,
        ShowAll,
        Quit,
        CloseWindow,
        Minimize,
        Zoom,
        ColimaHelp
    ]
);

/// The name that the menus, the About panel, and the window show. The bold
/// app menu shows the name of the executable, because the app has no
/// bundle.
pub const APP_NAME: &str = "Colima";

/// The accent colors that the View menu gives. `System` follows the accent
/// color in System Settings. The app keeps the choice between launches.
#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum AccentColor {
    System,
    #[default]
    Purple,
    Pink,
    Orange,
    Green,
    Teal,
    Blue,
}

impl AccentColor {
    const COLORS: [AccentColor; 6] = [
        AccentColor::Purple,
        AccentColor::Pink,
        AccentColor::Orange,
        AccentColor::Green,
        AccentColor::Teal,
        AccentColor::Blue,
    ];

    /// The menu label. The saved setting also uses it.
    fn name(self) -> &'static str {
        match self {
            AccentColor::System => "System",
            AccentColor::Purple => "Purple",
            AccentColor::Pink => "Pink",
            AccentColor::Orange => "Orange",
            AccentColor::Green => "Green",
            AccentColor::Teal => "Teal",
            AccentColor::Blue => "Blue",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        std::iter::once(AccentColor::System)
            .chain(Self::COLORS)
            .find(|c| c.name() == name)
    }
}

impl Global for AccentColor {}

/// Changes the accent color and saves the choice.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = native, no_json)]
pub struct SetAccentColor(AccentColor);

/// Sets the theme from the appearance of `window`, then puts the system
/// colors and the chosen accent color on it. Call it again when the
/// appearance or the accent color changes.
pub fn sync_theme(window: &mut Window, cx: &mut App) {
    Theme::sync_system_appearance(Some(window), cx);
    let dark = matches!(
        window.appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
    let choice = cx.try_global::<AccentColor>().copied().unwrap_or_default();
    let Some(c) = platform::system_colors(dark, choice) else {
        return;
    };
    let s = Surfaces::new(c.accent, dark);
    Theme::update(cx, |t| {
        // The components size their text in rems, and most of them use
        // `text_sm`, which is 0.875 rem. This rem size makes that text 13 pt,
        // the body size of macOS.
        t.font_size = px(text::BODY / 0.875);
        t.mono_font_size = px(text::CALLOUT);
        t.background = s.background;
        t.colors.list = s.background;
        t.table = s.background;
        t.title_bar = s.background;
        t.sidebar = s.sidebar;
        t.sidebar_border = s.sidebar_border;
        t.sidebar_accent = s.selection;
        t.muted = s.muted;
        t.popover = s.popover;
        t.foreground = c.label;
        t.muted_foreground = c.secondary_label;
        t.border = c.separator;
        t.input = c.separator;
        t.ring = c.accent;
        t.caret = c.accent;
        t.selection = c.text_selection;
        t.primary = c.accent;
        t.primary_hover = c.accent.opacity(0.9);
        t.primary_active = c.accent.opacity(0.8);
        t.primary_foreground = gpui::white();
        t.button_primary = c.accent;
        t.button_primary_hover = c.accent.opacity(0.9);
        t.button_primary_active = c.accent.opacity(0.8);
        t.button_primary_foreground = gpui::white();
        t.link = c.link;
        t.link_hover = c.link;
        t.link_active = c.link;
        t.blue = c.blue;
        t.green = c.green;
        t.red = c.red;
        t.yellow = c.yellow;
        t.danger = c.red;
        t.sidebar_foreground = c.label;
        t.sidebar_accent_foreground = c.label;
        t.tab_active_foreground = c.label;
        t.tab_foreground = c.secondary_label;
    });
    cx.set_global(SystemAccent {
        accent: c.accent,
        selection: c.selection,
        unemphasized_selection: s.selection,
    });
}

/// The fills of the window, the sidebar, and the tiles. Pure white and pure
/// black look flat, so each fill has a small tint of the accent hue. A gray
/// accent, such as Graphite, gives neutral grays.
struct Surfaces {
    background: Hsla,
    sidebar: Hsla,
    sidebar_border: Hsla,
    muted: Hsla,
    popover: Hsla,
    /// The fill of a selected row when the list has no focus.
    selection: Hsla,
}

impl Surfaces {
    fn new(accent: Hsla, dark: bool) -> Self {
        let tint = |s: f32, l: f32| hsla(accent.h, s * accent.s.min(1.), l, 1.);
        if dark {
            Surfaces {
                background: tint(0.14, 0.135),
                sidebar: tint(0.18, 0.175),
                sidebar_border: tint(0.18, 0.24),
                muted: tint(0.14, 0.22),
                popover: tint(0.14, 0.17),
                selection: tint(0.20, 0.25),
            }
        } else {
            Surfaces {
                background: tint(0.30, 0.975),
                sidebar: tint(0.32, 0.935),
                sidebar_border: tint(0.24, 0.88),
                muted: tint(0.26, 0.915),
                popover: tint(0.30, 0.985),
                selection: tint(0.30, 0.87),
            }
        }
    }
}

/// The colors that follow the accent color and that the theme has no
/// field for.
pub struct SystemAccent {
    pub accent: Hsla,
    /// The fill of a selected row in a list that has the focus.
    pub selection: Hsla,
    /// The fill of a selected row when the list or the window has no focus.
    pub unemphasized_selection: Hsla,
}

impl Global for SystemAccent {}

/// The chosen accent color, or the theme blue on other platforms.
pub fn accent(cx: &App) -> Hsla {
    cx.try_global::<SystemAccent>()
        .map(|a| a.accent)
        .unwrap_or_else(|| gpui_component::ActiveTheme::theme(cx).blue)
}

/// The fill of a selected row. `emphasized` is true when the list has the
/// focus in the active window. The row then has the accent color, and its
/// text is white.
pub fn selection(emphasized: bool, cx: &App) -> Hsla {
    match cx.try_global::<SystemAccent>() {
        Some(a) if emphasized => a.selection,
        Some(a) => a.unemphasized_selection,
        None => {
            let t = gpui_component::ActiveTheme::theme(cx);
            t.blue.opacity(if t.is_dark() { 0.22 } else { 0.1 })
        }
    }
}

/// The fill of a row under the pointer: a lighter fill of the gray
/// selection, so it never looks like a selected row.
pub fn hover(cx: &App) -> Hsla {
    selection(false, cx).opacity(0.5)
}

/// The fill of a toolbar control under the pointer. The detail tabs and the
/// toolbar buttons use it, so the two hover states look the same.
pub fn control_hover(cx: &App) -> Hsla {
    if gpui_component::ActiveTheme::theme(cx).is_dark() {
        white().opacity(0.08)
    } else {
        black().opacity(0.05)
    }
}

/// The style of an icon button in a toolbar. The ghost style of the
/// components has a hover fill that is almost invisible on these surfaces,
/// so this style uses the hover fill of the detail tabs.
pub fn toolbar_button(cx: &App) -> gpui_component::button::ButtonCustomVariant {
    let t = gpui_component::ActiveTheme::theme(cx);
    let active = if t.is_dark() { white().opacity(0.14) } else { black().opacity(0.1) };
    gpui_component::button::ButtonCustomVariant::new(cx)
        .color(transparent_black())
        .foreground(t.secondary_foreground)
        .hover(control_hover(cx))
        .active(active)
        .shadow(false)
}

/// True when a selected row uses the accent fill and white text.
pub fn has_emphasized_selection(cx: &App) -> bool {
    cx.has_global::<SystemAccent>()
}

/// Makes the window keep its size and position between launches.
///
/// The saved frame moves the window, and AppKit then tells GPUI about the
/// new bounds at once. GPUI cannot take that message while it updates the
/// app, so the frame changes in a task, after the current update.
pub fn autosave_frame(window: &Window, cx: &App) {
    platform::autosave_frame(window, "Colima Main Window", cx);
}

/// Sets the Dock icon. A bundled app gets the icon from its Info.plist, but
/// `cargo run` starts the bare binary, which has no bundle.
pub fn set_app_icon() {
    platform::set_app_icon(include_bytes!("../../assets/icon/colima-gui.icns"));
}

/// Adds the standard macOS menus, and the key bindings that go with them.
/// Also reads the saved accent color.
pub fn init_menus(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    cx.on_action(|_: &About, _| platform::show_about_panel(APP_NAME, env!("CARGO_PKG_VERSION")));
    cx.on_action(|_: &CheckForUpdates, cx| crate::updater::check(true, cx));
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &ColimaHelp, cx| cx.open_url("https://github.com/abiosoft/colima#readme"));
    cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, |w, _| w.remove_window()));
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |w, _| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |w, _| w.zoom_window()));
    cx.on_action(|a: &SetAccentColor, cx| {
        platform::save_accent(a.0.name());
        cx.set_global(a.0);
        // The check mark moves to the new color.
        set_menus(cx);
        // A menu action can come while GPUI updates the active window, and
        // that window then cannot update again. Change the theme after the
        // current update.
        cx.defer(|cx| {
            for handle in cx.windows() {
                let _ = handle.update(cx, |_, window, cx| sync_theme(window, cx));
            }
            cx.refresh_windows();
        });
    });

    let saved = platform::saved_accent().and_then(|name| AccentColor::from_name(&name));
    cx.set_global(saved.unwrap_or_default());
    set_menus(cx);
}

fn set_menus(cx: &mut App) {
    use crate::ui::app_view::ToggleSidebar;
    use crate::ui::container_list::{
        FocusSearch, RemoveSelected, RestartSelected, StartSelected, StopSelected, ToggleRunningOnly,
    };

    let current = cx.try_global::<AccentColor>().copied().unwrap_or_default();
    let accent_item = |c: AccentColor| MenuItem::action(c.name(), SetAccentColor(c)).checked(c == current);
    let mut accent_items = vec![accent_item(AccentColor::System), MenuItem::separator()];
    accent_items.extend(AccentColor::COLORS.map(accent_item));

    cx.set_menus([
        Menu::new(APP_NAME).items([
            MenuItem::action(format!("About {APP_NAME}"), About),
            MenuItem::action("Check for Updates…", CheckForUpdates),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(format!("Hide {APP_NAME}"), Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {APP_NAME}"), Quit),
        ]),
        Menu::new("File").items([MenuItem::action("Close Window", CloseWindow)]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
            MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find", FocusSearch),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Show Running Containers Only", ToggleRunningOnly),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Accent Color").items(accent_items)),
            // AppKit adds "Enter Full Screen" to this menu.
        ]),
        Menu::new("Container").items([
            MenuItem::action("Start", StartSelected),
            MenuItem::action("Stop", StopSelected),
            MenuItem::action("Restart", RestartSelected),
            MenuItem::separator(),
            MenuItem::action("Remove…", RemoveSelected),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
        Menu::new("Help").items([MenuItem::action("Colima Help", ColimaHelp)]),
    ]);
}

/// Reads a text setting that the app saved between launches.
pub fn saved_string(key: &str) -> Option<String> {
    platform::saved_string(key)
}

pub fn save_string(key: &str, value: &str) {
    platform::save_string(key, value)
}

/// True when the app is the active app, so that the user uses it now.
pub fn is_active() -> bool {
    platform::is_active()
}

/// Shows a modal alert with the buttons, the first button as the default.
/// Returns the index of the button that the user chose.
///
/// The alert does not activate the app. If the app is not active, the
/// alert does not get the key presses of another app, which could choose a
/// button. Show an alert only when the app is active.
///
/// The alert runs its own event loop until it closes. Call it from a task,
/// not while GPUI updates the app.
pub fn show_alert(message: &str, info: &str, buttons: &[&str]) -> Option<usize> {
    platform::show_alert(message, info, buttons)
}

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut Window, &mut App)) {
    if let Some(handle) = cx.active_window() {
        let _ = handle.update(cx, |_, window, cx| f(window, cx));
    }
}

/// An item of a context menu.
pub enum MenuEntry {
    Item { label: SharedString, enabled: bool },
    Separator,
}

impl MenuEntry {
    pub fn item(label: impl Into<SharedString>) -> Self {
        MenuEntry::Item { label: label.into(), enabled: true }
    }
}

/// Shows an AppKit context menu at the pointer. When the user chooses an
/// item, `on_choose` gets the index of that item in `entries`.
///
/// The menu runs its own event loop until it closes. That loop must not run
/// while GPUI updates the app, so the menu opens from a task. The task
/// starts after the next frame, so the window first shows the changes of
/// the current event, for example the selection of the clicked row.
pub fn show_context_menu<T: 'static>(
    entries: Vec<MenuEntry>,
    window: &mut Window,
    cx: &mut Context<T>,
    on_choose: impl FnOnce(&mut T, usize, &mut Window, &mut Context<T>) + 'static,
) {
    let this = cx.weak_entity();
    cx.notify();
    window.on_next_frame(move |window, cx| {
        window
            .spawn(cx, async move |cx| {
                if let Some(ix) = platform::pop_up_menu(&entries) {
                    let _ = this.update_in(cx, |this, window, cx| on_choose(this, ix, window, cx));
                }
            })
            .detach();
    });
}

struct SystemColors {
    label: Hsla,
    secondary_label: Hsla,
    separator: Hsla,
    accent: Hsla,
    selection: Hsla,
    text_selection: Hsla,
    link: Hsla,
    blue: Hsla,
    green: Hsla,
    red: Hsla,
    yellow: Hsla,
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::Cell;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSAboutPanelOptionApplicationName, NSAboutPanelOptionApplicationVersion, NSAlert,
        NSAlertFirstButtonReturn, NSAppearance,
        NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSColor, NSColorSpace,
        NSEvent, NSImage, NSMenu, NSMenuItem, NSView,
    };
    use objc2_foundation::{NSData, NSDictionary, NSObject, NSString, NSUserDefaults};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::*;

    fn hsla(color: &NSColor) -> Hsla {
        let Some(c) = color.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()) else {
            return gpui::black();
        };
        let (mut r, mut g, mut b, mut a) = (0., 0., 0., 0.);
        // SAFETY: the color is in the sRGB space, which has these components.
        unsafe { c.getRed_green_blue_alpha(&mut r, &mut g, &mut b, &mut a) };
        Rgba { r: r as f32, g: g as f32, b: b as f32, a: a as f32 }.into()
    }

    /// The key of the saved accent color in the user defaults.
    const ACCENT_KEY: &str = "AccentColor";

    pub(super) fn saved_accent() -> Option<String> {
        saved_string(ACCENT_KEY)
    }

    pub(super) fn save_accent(name: &str) {
        save_string(ACCENT_KEY, name)
    }

    pub(super) fn saved_string(key: &str) -> Option<String> {
        let key = NSString::from_str(key);
        Some(NSUserDefaults::standardUserDefaults().stringForKey(&key)?.to_string())
    }

    pub(super) fn save_string(key: &str, value: &str) {
        let key = NSString::from_str(key);
        let value = NSString::from_str(value);
        // SAFETY: a string is a property list value, so the defaults can
        // keep it.
        unsafe { NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&value), &key) };
    }

    pub(super) fn is_active() -> bool {
        MainThreadMarker::new().is_some_and(|mtm| NSApplication::sharedApplication(mtm).isActive())
    }

    pub(super) fn show_alert(message: &str, info: &str, buttons: &[&str]) -> Option<usize> {
        let mtm = MainThreadMarker::new()?;
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(message));
        alert.setInformativeText(&NSString::from_str(info));
        for title in buttons {
            alert.addButtonWithTitle(&NSString::from_str(title));
        }
        usize::try_from(alert.runModal() - NSAlertFirstButtonReturn).ok()
    }

    /// The system color of an accent color choice. `System` has no color
    /// here, because it uses the accent colors of AppKit.
    fn accent_color(choice: AccentColor) -> Option<Retained<NSColor>> {
        Some(match choice {
            AccentColor::System => return None,
            AccentColor::Purple => NSColor::systemPurpleColor(),
            AccentColor::Pink => NSColor::systemPinkColor(),
            AccentColor::Orange => NSColor::systemOrangeColor(),
            AccentColor::Green => NSColor::systemGreenColor(),
            AccentColor::Teal => NSColor::systemTealColor(),
            AccentColor::Blue => NSColor::systemBlueColor(),
        })
    }

    pub(super) fn system_colors(dark: bool, choice: AccentColor) -> Option<SystemColors> {
        let name = unsafe {
            if dark { NSAppearanceNameDarkAqua } else { NSAppearanceNameAqua }
        };
        let appearance = NSAppearance::appearanceNamed(name)?;
        // The dynamic colors resolve for the current drawing appearance.
        let out = Cell::new(None);
        let block = RcBlock::new(|| {
            // The system colors have a light and a dark variant. A chosen
            // color also fills the selected rows, the text selection, and
            // the links.
            let (accent, selection, text_selection, link) = match accent_color(choice) {
                Some(color) => {
                    let accent = hsla(&color);
                    (accent, accent, accent.opacity(if dark { 0.45 } else { 0.3 }), accent)
                }
                None => (
                    hsla(&NSColor::controlAccentColor()),
                    hsla(&NSColor::selectedContentBackgroundColor()),
                    hsla(&NSColor::selectedTextBackgroundColor()),
                    hsla(&NSColor::linkColor()),
                ),
            };
            out.set(Some(SystemColors {
                label: hsla(&NSColor::labelColor()),
                secondary_label: hsla(&NSColor::secondaryLabelColor()),
                separator: hsla(&NSColor::separatorColor()),
                accent,
                selection,
                text_selection,
                link,
                blue: hsla(&NSColor::systemBlueColor()),
                green: hsla(&NSColor::systemGreenColor()),
                red: hsla(&NSColor::systemRedColor()),
                yellow: hsla(&NSColor::systemYellowColor()),
            }));
        });
        appearance.performAsCurrentDrawingAppearance(&block);
        out.take()
    }

    pub(super) fn autosave_frame(window: &Window, name: &'static str, cx: &App) {
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return;
        };
        // SAFETY: the handle points to the GPUI view, which lives as long as
        // the window.
        let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
        let Some(ns_window) = view.window() else {
            return;
        };
        cx.foreground_executor()
            .spawn(async move {
                let name = NSString::from_str(name);
                // Put the saved frame back first. The autosave name then
                // saves each change to the frame.
                ns_window.setFrameUsingName(&name);
                ns_window.setFrameAutosaveName(&name);
            })
            .detach();
    }

    pub(super) fn set_app_icon(icns: &[u8]) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let data = NSData::with_bytes(icns);
        let Some(image) = NSImage::initWithData(mtm.alloc(), &data) else {
            return;
        };
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }

    pub(super) fn show_about_panel(name: &str, version: &str) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let name = NSString::from_str(name);
        let version = NSString::from_str(version);
        let options = unsafe {
            NSDictionary::<NSString, AnyObject>::from_slices(
                &[NSAboutPanelOptionApplicationName, NSAboutPanelOptionApplicationVersion],
                &[&*name as &AnyObject, &*version as &AnyObject],
            )
        };
        let app = NSApplication::sharedApplication(mtm);
        unsafe { app.orderFrontStandardAboutPanelWithOptions(&options) };
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }

    define_class!(
        /// The target of the context menu items. It keeps the tag of the
        /// item that the user chose.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "ColimaMenuTarget"]
        #[ivars = Cell<isize>]
        struct MenuTarget;

        impl MenuTarget {
            #[unsafe(method(choose:))]
            fn choose(&self, item: &NSMenuItem) {
                self.ivars().set(item.tag());
            }
        }
    );

    impl MenuTarget {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(Cell::new(-1));
            unsafe { msg_send![super(this), init] }
        }
    }

    pub(super) fn pop_up_menu(entries: &[MenuEntry]) -> Option<usize> {
        let mtm = MainThreadMarker::new()?;
        let target = MenuTarget::new(mtm);
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        for (ix, entry) in entries.iter().enumerate() {
            let item = match entry {
                MenuEntry::Separator => NSMenuItem::separatorItem(mtm),
                MenuEntry::Item { label, enabled } => {
                    let item = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            mtm.alloc(),
                            &NSString::from_str(label),
                            Some(sel!(choose:)),
                            &NSString::new(),
                        )
                    };
                    unsafe { item.setTarget(Some(&target)) };
                    item.setTag(ix as isize);
                    item.setEnabled(*enabled);
                    item
                }
            };
            menu.addItem(&item);
        }
        // With no view, the location is in screen coordinates. The top left
        // of the menu goes to the pointer.
        menu.popUpMenuPositioningItem_atLocation_inView(None, NSEvent::mouseLocation(), None);
        usize::try_from(target.ivars().get()).ok()
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub(super) fn system_colors(_: bool, _: AccentColor) -> Option<SystemColors> {
        None
    }
    pub(super) fn saved_accent() -> Option<String> {
        None
    }
    pub(super) fn save_accent(_: &str) {}
    pub(super) fn saved_string(_: &str) -> Option<String> {
        None
    }
    pub(super) fn save_string(_: &str, _: &str) {}
    pub(super) fn is_active() -> bool {
        true
    }
    pub(super) fn show_alert(_: &str, _: &str, _: &[&str]) -> Option<usize> {
        None
    }
    pub(super) fn autosave_frame(_: &Window, _: &'static str, _: &App) {}
    pub(super) fn set_app_icon(_: &[u8]) {}
    pub(super) fn show_about_panel(_: &str, _: &str) {}
    pub(super) fn pop_up_menu(_: &[MenuEntry]) -> Option<usize> {
        None
    }
}

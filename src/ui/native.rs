//! The parts of macOS that GPUI does not give: the system colors, the menu
//! bar, the context menus of AppKit, and the saved window frame.
//!
//! On other platforms, the theme keeps its colors, and the context menus
//! do not open.

use gpui::*;
use gpui_component::{Theme, input};

use super::text;

actions!(
    native,
    [
        About,
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

/// Sets the theme from the appearance of `window`, then puts the system
/// colors on it. Call it again when the appearance or the accent color
/// changes.
pub fn sync_theme(window: &mut Window, cx: &mut App) {
    Theme::sync_system_appearance(Some(window), cx);
    let dark = matches!(
        window.appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
    let Some(c) = platform::system_colors(dark) else {
        return;
    };
    Theme::update(cx, |t| {
        // The components size their text in rems, and most of them use
        // `text_sm`, which is 0.875 rem. This rem size makes that text 13 pt,
        // the body size of macOS.
        t.font_size = px(text::BODY / 0.875);
        t.mono_font_size = px(text::CALLOUT);
        t.background = c.content_background;
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
        unemphasized_selection: c.unemphasized_selection,
    });
}

/// The colors that follow the accent color of the system and that the
/// theme has no field for.
pub struct SystemAccent {
    pub accent: Hsla,
    /// The fill of a selected row in a list that has the focus.
    pub selection: Hsla,
    /// The fill of a selected row when the list or the window has no focus.
    pub unemphasized_selection: Hsla,
}

impl Global for SystemAccent {}

/// The accent color of the system, or the theme blue on other platforms.
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

/// Adds the standard macOS menus, and the key bindings that go with them.
pub fn init_menus(cx: &mut App) {
    use crate::ui::app_view::ToggleSidebar;
    use crate::ui::container_list::{
        FocusSearch, RemoveSelected, RestartSelected, StartSelected, StopSelected, ToggleRunningOnly,
    };

    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    cx.on_action(|_: &About, _| platform::show_about_panel(APP_NAME, env!("CARGO_PKG_VERSION")));
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &ColimaHelp, cx| cx.open_url("https://github.com/abiosoft/colima#readme"));
    cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, |w, _| w.remove_window()));
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |w, _| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |w, _| w.zoom_window()));

    cx.set_menus([
        Menu::new(APP_NAME).items([
            MenuItem::action(format!("About {APP_NAME}"), About),
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
    content_background: Hsla,
    accent: Hsla,
    selection: Hsla,
    unemphasized_selection: Hsla,
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
        NSAboutPanelOptionApplicationName, NSAboutPanelOptionApplicationVersion, NSAppearance,
        NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSColor, NSColorSpace,
        NSEvent, NSMenu, NSMenuItem, NSView,
    };
    use objc2_foundation::{NSDictionary, NSObject, NSString};
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

    pub(super) fn system_colors(dark: bool) -> Option<SystemColors> {
        let name = unsafe {
            if dark { NSAppearanceNameDarkAqua } else { NSAppearanceNameAqua }
        };
        let appearance = NSAppearance::appearanceNamed(name)?;
        // The dynamic colors resolve for the current drawing appearance.
        let out = Cell::new(None);
        let block = RcBlock::new(|| {
            out.set(Some(SystemColors {
                label: hsla(&NSColor::labelColor()),
                secondary_label: hsla(&NSColor::secondaryLabelColor()),
                separator: hsla(&NSColor::separatorColor()),
                content_background: hsla(&NSColor::controlBackgroundColor()),
                accent: hsla(&NSColor::controlAccentColor()),
                selection: hsla(&NSColor::selectedContentBackgroundColor()),
                unemphasized_selection: hsla(&NSColor::unemphasizedSelectedContentBackgroundColor()),
                text_selection: hsla(&NSColor::selectedTextBackgroundColor()),
                link: hsla(&NSColor::linkColor()),
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

    pub(super) fn system_colors(_: bool) -> Option<SystemColors> {
        None
    }
    pub(super) fn autosave_frame(_: &Window, _: &'static str, _: &App) {}
    pub(super) fn show_about_panel(_: &str, _: &str) {}
    pub(super) fn pop_up_menu(_: &[MenuEntry]) -> Option<usize> {
        None
    }
}

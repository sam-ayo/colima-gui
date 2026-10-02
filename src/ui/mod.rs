pub mod app_view;
pub mod container_detail;
pub mod container_list;
pub mod image_icon;
pub mod json_view;
pub mod key_nav;
pub mod logs_view;
pub mod native;
pub mod quit;
pub mod sidebar;
pub mod stats_view;
pub mod status_menu;
pub mod toast;
pub mod volume_detail;
pub mod volume_list;

use std::sync::Arc;
use std::time::Duration;

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::spinner::Spinner;

use crate::docker::containers::State;

/// The height of the toolbars. The toolbars fill the area of the title bar,
/// so the traffic lights sit on the same line as the toolbar content.
pub const TOOLBAR_HEIGHT: f32 = 52.;

/// The width of the sidebar panel.
pub const SIDEBAR_WIDTH: f32 = 220.;
/// The space between the sidebar panel and the edges of the window. The
/// panel floats in the window, as the sidebars of macOS 26 do.
pub const SIDEBAR_INSET: f32 = 8.;
/// The corner radius of the sidebar panel. It is concentric with the
/// corners of the window.
pub const SIDEBAR_RADIUS: f32 = 18.;

/// The text sizes of macOS, in points. GPUI sizes text in rems of 16 px,
/// which is larger than the 13 pt body text of the system.
pub mod text {
    /// The value of a statistic and the title of a pane.
    pub const TITLE2: f32 = 17.;
    /// The title of a toolbar.
    pub const TITLE3: f32 = 15.;
    /// The body text: the list rows, the sidebar, and the labels.
    pub const BODY: f32 = 13.;
    /// The secondary lines below a row title.
    pub const CALLOUT: f32 = 12.;
    /// The section headings of the sidebar, and small details.
    pub const SUBHEADLINE: f32 = 11.;
}

/// The height of a push button with a label, as in the AppKit forms.
pub const NATIVE_BUTTON_HEIGHT: f32 = 28.;

/// A load that ends before this time shows no spinner. A spinner that shows
/// for one frame looks like a fault.
const SPINNER_DELAY: Duration = Duration::from_millis(300);

/// The time for a view that comes in or changes. Keep it short, because
/// the user waits for it.
pub const ENTER_DURATION: Duration = Duration::from_millis(180);

/// A critically damped spring that settles in about 200 ms, with no bounce.
pub const SNAPPY_SPRING: SpringConfig = SpringConfig::new(700., 53., 1.);

/// Numbers with equal widths. Values that change each second then do not
/// move the text next to them.
pub fn tabular_nums() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// A spinner that turns at a constant speed. The default easing of
/// `Spinner` slows to a stop at the end of each turn. A loading `Button`
/// also makes a default `Spinner`, unless its icon is already a spinner. Thus
/// set `.icon(spinner())` on a button while it loads.
pub fn spinner() -> Spinner {
    Spinner::new().ease(linear)
}

/// Shows `el` after `SPINNER_DELAY`, then fades it in.
pub fn delayed_reveal(id: impl Into<ElementId>, el: Div) -> impl IntoElement {
    el.with_animations(
        id,
        vec![
            Animation::new(SPINNER_DELAY),
            Animation::new(ENTER_DURATION).with_easing(ease_out_quint()),
        ],
        |el, ix, delta| el.opacity(if ix == 0 { 0. } else { delta }),
    )
}

/// Reads the "Reduce motion" setting of macOS. GPUI does not read it, but
/// all its animations obey `App::reduce_motion`.
pub fn sync_reduce_motion(cx: &mut App) {
    let reduce = std::process::Command::new("defaults")
        .args(["read", "com.apple.universalaccess", "reduceMotion"])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1");
    cx.set_reduce_motion(reduce);
}

pub fn state_color(state: State, cx: &App) -> Hsla {
    let t = cx.theme();
    match state {
        State::Running => t.green,
        State::Paused | State::Restarting => t.yellow,
        State::Dead => t.red,
        State::Created | State::Exited | State::Removing | State::Unknown => t.muted_foreground,
    }
}

/// An empty area that moves the window. A double click zooms or minimizes
/// the window, as the system setting tells. Put controls next to this area,
/// not in it, because a press here starts a window move.
pub fn drag_area(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |ev, window, _| {
            if ev.click_count == 1 {
                window.start_window_move();
            }
        })
        .on_click(|ev, window, _| {
            if ev.click_count() == 2 {
                window.titlebar_double_click();
            }
        })
}

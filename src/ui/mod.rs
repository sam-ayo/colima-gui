pub mod app_view;
pub mod container_detail;
pub mod container_list;
pub mod image_icon;
pub mod logs_view;
pub mod sidebar;
pub mod stats_view;

use std::sync::Arc;
use std::time::Duration;

use gpui::*;
use gpui_component::ActiveTheme;

use crate::docker::containers::State;

/// The height of the toolbars. The toolbars fill the area of the title bar,
/// so the traffic lights sit on the same line as the toolbar content.
pub const TOOLBAR_HEIGHT: f32 = 52.;

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

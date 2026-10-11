//! A modal panel above the main window content, with a dim backdrop. It
//! renders as a plugin of the window root, so the actions of the main view
//! do not apply while it shows.
//!
//! The panel fades in and rises a short distance. GPUI has no scale
//! transform for an element, so a rise takes the place of the usual scale.
//! An open or a close from the keyboard is immediate, because keys are fast
//! and frequent, as with the detail pane.

use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_base::{Root, RootPlugin};
use gpui_component::ActiveTheme;
use gpui_base::actions::{Cancel, Confirm};

use super::ENTER_DURATION;

/// The distance that the panel rises while it comes in.
const RISE: f32 = 8.;

/// A move of the panel between closed (0) and open (1).
#[derive(Clone, Copy)]
struct Motion {
    from: f32,
    to: f32,
    start: Instant,
    animate: bool,
}

impl Motion {
    /// The position at `now`, and true when the move is at its end.
    fn at(&self, now: Instant) -> (f32, bool) {
        if !self.animate {
            return (self.to, true);
        }
        let t = (now.duration_since(self.start).as_secs_f32() / ENTER_DURATION.as_secs_f32()).min(1.);
        (self.from + (self.to - self.from) * ease_out_quint()(t), t >= 1.)
    }
}

struct Open {
    view: AnyView,
    width: Pixels,
    /// The focus of the panel host. The focus trap keeps Tab in the panel.
    focus: FocusHandle,
    /// The focus before the open. A close gives it back.
    restore: Option<WeakFocusHandle>,
    /// The confirm action of the panel, for Return.
    on_confirm: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    motion: Motion,
    closing: bool,
}

pub struct ModalLayer {
    open: Option<Open>,
}

impl RootPlugin for ModalLayer {}

/// Adds the layer to the windows that open after this call.
pub fn init(cx: &mut App) {
    Root::register_plugin::<ModalLayer>(cx, |_, _| ModalLayer { open: None });
}

fn layer(window: &Window, cx: &App) -> Option<Entity<ModalLayer>> {
    Root::read(window, cx).plugin::<ModalLayer>()
}

/// What `open` shows.
pub struct Request {
    pub view: AnyView,
    pub width: f32,
    /// The element that gets the focus, for example the first field.
    pub focus: Option<FocusHandle>,
    /// Return calls this. A Return in a field calls it too.
    pub on_confirm: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    /// False for an open from the keyboard.
    pub animate: bool,
}

/// Shows `request.view` in the modal panel. A panel that shows already
/// stays, and the call has no effect.
pub fn open(request: Request, window: &mut Window, cx: &mut App) {
    let Some(layer) = layer(window, cx) else { return };
    if layer.read(cx).open.as_ref().is_some_and(|o| !o.closing) {
        return;
    }
    let restore = window.focused(cx).map(|f| f.downgrade());
    let focus = cx.focus_handle();
    // A new open during the exit of the last panel keeps the first focus.
    let restore = layer.read(cx).open.as_ref().and_then(|o| o.restore.clone()).or(restore);
    layer.update(cx, |layer, cx| {
        layer.open = Some(Open {
            view: request.view,
            width: px(request.width),
            focus: focus.clone(),
            restore,
            on_confirm: request.on_confirm,
            motion: Motion { from: 0., to: 1., start: Instant::now(), animate: request.animate },
            closing: false,
        });
        cx.notify();
    });
    request.focus.unwrap_or(focus).focus(window, cx);
}

/// Closes the panel. `animate` is false for a close from the keyboard.
pub fn close(animate: bool, window: &mut Window, cx: &mut App) {
    let Some(layer) = layer(window, cx) else { return };
    let restore = layer.update(cx, |layer, cx| {
        let open = layer.open.as_mut().filter(|o| !o.closing)?;
        // An exit in the middle of the entrance turns back from where the
        // panel is.
        let (from, _) = open.motion.at(Instant::now());
        open.motion = Motion { from, to: 0., start: Instant::now(), animate };
        open.closing = true;
        cx.notify();
        open.restore.clone()
    });
    if let Some(handle) = restore.and_then(|h| h.upgrade()) {
        handle.focus(window, cx);
    }
}

impl Render for ModalLayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(open) = &self.open else { return div().into_any_element() };
        let (p, done) = open.motion.at(Instant::now());
        if done && open.closing {
            self.open = None;
            return div().into_any_element();
        }
        if !done {
            window.request_animation_frame();
        }
        // Reduced motion keeps the fade, but not the move.
        let rise = if cx.reduce_motion() { 0. } else { RISE * (1. - p) };
        let mut dim = cx.theme().overlay;
        dim.a *= p;
        let closing = open.closing;
        let this = cx.weak_entity();
        gpui_base::Dialog::new(cx)
            .focus_handle(open.focus.clone())
            .close_on_escape(!closing)
            .close_on_backdrop_press(!closing)
            .backdrop(div().size_full().bg(dim))
            .request_close(|_, window, cx| close(true, window, cx))
            .popup(
                div()
                    .id("modal-panel")
                    .relative()
                    .top(px(rise))
                    .w(open.width)
                    .opacity(p)
                    .occlude()
                    // Escape and Return come from the keyboard, so the panel
                    // closes at once. These handlers come before the ones of
                    // the dialog host.
                    .when(!closing, |d| {
                        d.on_action(|_: &Cancel, window, cx| close(false, window, cx))
                            .on_action(move |_: &Confirm, window, cx| {
                                // The confirm can close the panel, which
                                // updates this layer. Read it first.
                                let confirm = this
                                    .upgrade()
                                    .and_then(|l| l.read(cx).open.as_ref().and_then(|o| o.on_confirm.clone()));
                                if let Some(confirm) = confirm {
                                    confirm(window, cx);
                                }
                            })
                    })
                    .child(open.view.clone()),
            )
            .into_any_element()
    }
}

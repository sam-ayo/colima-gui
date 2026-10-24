//! Toasts in the style of Sonner (https://sonner.emilkowal.ski), the React
//! toast library. GPUI cannot run Sonner, so this module copies its look
//! and its behavior:
//!
//! - The toasts stack at the bottom-right corner, with the newest in front.
//!   The toasts behind show as smaller cards above the front toast.
//! - The pointer on the stack opens it into a list, and stops the timers.
//! - A toast closes after 4 seconds. A drag to the right removes it.
//! - Only 3 toasts show. The others wait behind them.
//!
//! One `Toaster` view shows all the toasts. Call `show` from any place
//! that has an `App`.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme, Icon, Sizable as _};
use gpui_kit_assets::IconName;

use super::{spinner, text};

/// The values of the Sonner style sheet and of its defaults.
const TOAST_WIDTH: f32 = 356.;
/// The distance from the window edges. Sonner uses 32 px on a page, and
/// 16 px on a small screen. A window is nearer to a small screen.
const OFFSET: f32 = 16.;
/// The space between the toasts of an open stack.
const GAP: f32 = 14.;
const VISIBLE_TOASTS: usize = 3;
const DURATION: Duration = Duration::from_millis(4000);
/// Each toast behind the front toast is this much smaller.
const SCALE_STEP: f32 = 0.05;
const PADDING: f32 = 16.;
const RADIUS: f32 = 8.;
const ICON_SIZE: f32 = 16.;
/// The height of a toast with one line of text, until the first layout
/// gives the true height.
const ESTIMATED_HEIGHT: f32 = 54.;
/// A drag this far to the right, or at this speed, removes the toast. The
/// speed is in pixels per millisecond.
const SWIPE_THRESHOLD: f32 = 45.;
const SWIPE_VELOCITY: f32 = 0.11;

/// Sonner moves the toasts with a CSS transition of 400 ms. This spring
/// settles in about the same time, and a change in the middle keeps the
/// speed.
const TOAST_SPRING: SpringConfig = SpringConfig::new(220., 30., 1.);
const PIXEL_EPSILON: f32 = 0.5;
const UNIT_EPSILON: f32 = 0.002;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Message,
    Success,
    Info,
    Warning,
    Error,
    /// A spinner in place of the icon. A loading toast stays until a new
    /// toast with the same id replaces it.
    Loading,
}

#[derive(Clone)]
pub struct Toast {
    kind: ToastKind,
    title: SharedString,
    description: Option<SharedString>,
    id: Option<SharedString>,
    /// `None` keeps the toast until the user removes it.
    duration: Option<Duration>,
    action: Option<ToastAction>,
}

/// A button in the toast. A click runs the handler and closes the toast.
#[derive(Clone)]
struct ToastAction {
    label: SharedString,
    handler: Rc<dyn Fn(&mut App)>,
}

#[allow(dead_code)]
impl Toast {
    pub fn new(kind: ToastKind, title: impl Into<SharedString>) -> Self {
        let duration = (kind != ToastKind::Loading).then_some(DURATION);
        Self { kind, title: title.into(), description: None, id: None, duration, action: None }
    }

    pub fn message(title: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Message, title)
    }

    pub fn success(title: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Success, title)
    }

    pub fn info(title: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Info, title)
    }

    pub fn warning(title: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Warning, title)
    }

    pub fn error(title: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Error, title)
    }

    pub fn loading(title: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Loading, title)
    }

    /// A second line below the title, in the secondary color.
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// A toast with the id of a toast that shows replaces its content, and
    /// starts its timer again. The toast does not move.
    pub fn id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    pub fn action(mut self, label: impl Into<SharedString>, handler: impl Fn(&mut App) + 'static) -> Self {
        self.action = Some(ToastAction { label: label.into(), handler: Rc::new(handler) });
        self
    }

    /// Keeps the toast until the user removes it.
    pub fn persist(mut self) -> Self {
        self.duration = None;
        self
    }
}

struct ToasterHandle(Entity<Toaster>);

impl Global for ToasterHandle {}

/// Shows a toast in the toaster of the main window.
pub fn show(toast: Toast, cx: &mut App) {
    if let Some(handle) = cx.try_global::<ToasterHandle>() {
        handle.0.clone().update(cx, |t, cx| t.push(toast, cx));
    }
}

/// Puts `text` on the clipboard, and shows a toast that tells the user.
/// `description` is the second line of the toast. A new copy replaces the
/// toast of the last copy, so many copies do not fill the stack.
pub fn copy(text: String, description: impl Into<SharedString>, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    show(Toast::success("Copied to the clipboard").description(description).id("copied"), cx);
}

/// Removes the toast with `id`.
pub fn dismiss(id: &str, cx: &mut App) {
    if let Some(handle) = cx.try_global::<ToasterHandle>() {
        handle.0.clone().update(cx, |t, cx| t.dismiss(id, cx));
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Removal {
    Timeout,
    Swipe,
}

/// The values that move on springs. `y` is the distance from the bottom of
/// the stack, `x` is the drag to the right, and `scale` makes a toast
/// behind the front toast smaller.
#[derive(Clone, Copy)]
struct Motion {
    y: SpringState,
    x: SpringState,
    height: SpringState,
    scale: SpringState,
    opacity: SpringState,
    content: SpringState,
}

#[derive(Clone, Copy)]
struct Targets {
    y: f32,
    x: f32,
    height: f32,
    scale: f32,
    opacity: f32,
    content: f32,
}

fn rest(position: f32) -> SpringState {
    SpringState { position, velocity: 0. }
}

impl Motion {
    /// A new toast comes up from below its own height, and fades in.
    fn enter() -> Self {
        Self {
            y: rest(-ESTIMATED_HEIGHT),
            x: rest(0.),
            height: rest(ESTIMATED_HEIGHT),
            scale: rest(1.),
            opacity: rest(0.),
            content: rest(1.),
        }
    }

    fn snap(&mut self, to: Targets) {
        *self = Self {
            y: rest(to.y),
            x: rest(to.x),
            height: rest(to.height),
            scale: rest(to.scale),
            opacity: rest(to.opacity),
            content: rest(to.content),
        };
    }

    /// Moves each spring one step. Returns true while a value moves.
    fn step(&mut self, to: Targets, dt: f32, follow_x: bool) -> bool {
        let mut moving = false;
        let mut go = |s: &mut SpringState, target: f32, epsilon: f32| {
            if s.position == target && s.velocity == 0. {
                return;
            }
            *s = TOAST_SPRING.step(*s, target, dt);
            if TOAST_SPRING.is_settled(*s, target, epsilon) {
                *s = rest(target);
            } else {
                moving = true;
            }
        };
        go(&mut self.y, to.y, PIXEL_EPSILON);
        if !follow_x {
            go(&mut self.x, to.x, PIXEL_EPSILON);
        }
        go(&mut self.height, to.height, PIXEL_EPSILON);
        go(&mut self.scale, to.scale, UNIT_EPSILON);
        go(&mut self.opacity, to.opacity, UNIT_EPSILON);
        go(&mut self.content, to.content, UNIT_EPSILON);
        moving
    }
}

struct Entry {
    key: u64,
    toast: Toast,
    /// The time until the toast closes. It does not go down while the
    /// timers stop.
    remaining: Option<Duration>,
    removed: Option<Removal>,
    /// The height of the content at the toast width, from the last layout.
    height: Rc<Cell<f32>>,
    motion: Motion,
}

impl Entry {
    fn natural_height(&self) -> f32 {
        let h = self.height.get();
        if h > 0. { h } else { ESTIMATED_HEIGHT }
    }
}

/// A drag of a toast to the right.
struct Swipe {
    key: u64,
    press_x: Pixels,
    started_at: Instant,
    offset: f32,
}

/// The value of a swipe drag. GPUI needs a view to follow the pointer. The
/// view shows nothing, because the toast itself moves.
struct SwipeDrag;

impl Render for SwipeDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

pub struct Toaster {
    entries: Vec<Entry>,
    next_key: u64,
    /// True while the pointer is on the stack, in a gap between toasts.
    hovered: bool,
    /// The toast under the pointer. Each toast blocks the mouse for the
    /// stack below it, so the stack is not hovered while the pointer is
    /// on a toast.
    hovered_toast: Option<u64>,
    /// The pointer position at the last press on a toast. A swipe starts
    /// from it.
    press: Option<(u64, Pixels)>,
    swipe: Option<Swipe>,
    /// The time of the last spring step, or `None` when the springs are at
    /// rest.
    stepped_at: Option<Instant>,
    /// The time of the last timer update, and if the timers ran after it.
    ticked_at: Instant,
    running: bool,
    /// The task that wakes the toaster when the next toast closes.
    wake_at: Option<Instant>,
    _timer: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl Toaster {
    /// Makes the toaster of `window`, and makes it the target of `show`.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.set_global(ToasterHandle(cx.entity()));
        // Sonner stops the timers while the page is hidden. Here, the
        // timers stop while the window is not active.
        let subs = vec![cx.observe_window_activation(window, |_, _, cx| cx.notify())];
        #[cfg(debug_assertions)]
        if std::env::var_os("TOAST_DEMO").is_some() {
            demo(cx);
        }
        Self {
            entries: Vec::new(),
            next_key: 0,
            hovered: false,
            hovered_toast: None,
            press: None,
            swipe: None,
            stepped_at: None,
            ticked_at: Instant::now(),
            running: false,
            wake_at: None,
            _timer: None,
            _subs: subs,
        }
    }

    fn push(&mut self, toast: Toast, cx: &mut Context<Self>) {
        let existing = toast.id.as_ref().and_then(|id| {
            self.entries
                .iter_mut()
                .find(|e| e.removed.is_none() && e.toast.id.as_ref() == Some(id))
        });
        if let Some(entry) = existing {
            entry.remaining = toast.duration;
            entry.toast = toast;
        } else {
            self.entries.insert(
                0,
                Entry {
                    key: self.next_key,
                    remaining: toast.duration,
                    toast,
                    removed: None,
                    height: Rc::default(),
                    motion: Motion::enter(),
                },
            );
            self.next_key += 1;
        }
        cx.notify();
    }

    fn dismiss(&mut self, id: &str, cx: &mut Context<Self>) {
        for e in &mut self.entries {
            if e.removed.is_none() && e.toast.id.as_deref() == Some(id) {
                e.removed = Some(Removal::Timeout);
            }
        }
        cx.notify();
    }

    /// Removes a toast after a click on its close button.
    fn close(&mut self, key: u64, cx: &mut Context<Self>) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.key == key) {
            e.removed = Some(Removal::Timeout);
        }
        self.press = None;
        cx.notify();
    }

    /// True for the toasts that show: the first `VISIBLE_TOASTS` toasts
    /// that are not removed. The others wait behind them, with no opacity.
    fn is_visible(&self, key: u64) -> bool {
        self.entries
            .iter()
            .filter(|e| e.removed.is_none())
            .take(VISIBLE_TOASTS)
            .any(|e| e.key == key)
    }

    fn expanded(&self) -> bool {
        self.hovered || self.hovered_toast.is_some() || self.swipe.is_some()
    }

    /// Counts down the timers, and closes the toasts at zero. Then wakes
    /// the toaster when the next toast closes.
    fn tick(&mut self, window: &Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let elapsed = now - self.ticked_at;
        self.ticked_at = now;
        if self.running {
            for e in self.entries.iter_mut().filter(|e| e.removed.is_none()) {
                if let Some(left) = &mut e.remaining {
                    *left = left.saturating_sub(elapsed);
                    if left.is_zero() {
                        e.removed = Some(Removal::Timeout);
                    }
                }
            }
        }
        self.running = !self.expanded() && window.is_window_active();
        let next = self
            .entries
            .iter()
            .filter(|e| e.removed.is_none())
            .filter_map(|e| e.remaining)
            .min()
            .filter(|_| self.running);
        let Some(next) = next else {
            self.wake_at = None;
            self._timer = None;
            return;
        };
        // The deadline stays the same while the timers run. Make a new
        // task only when it changes.
        let at = now + next;
        let same = self
            .wake_at
            .is_some_and(|w| w.max(at) - w.min(at) < Duration::from_millis(5));
        if !same {
            self.wake_at = Some(at);
            self._timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(next).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            }));
        }
    }

    /// Ends a swipe after the release of the pointer. A long or fast drag
    /// removes the toast. A short drag lets it go back.
    fn end_swipe(&mut self, cx: &App) {
        if cx.has_active_drag() {
            return;
        }
        let Some(swipe) = self.swipe.take() else {
            return;
        };
        let ms = swipe.started_at.elapsed().as_secs_f32() * 1000.;
        let velocity = swipe.offset / ms.max(1.);
        if swipe.offset >= SWIPE_THRESHOLD || (swipe.offset > 0. && velocity > SWIPE_VELOCITY) {
            if let Some(e) = self.entries.iter_mut().find(|e| e.key == swipe.key) {
                e.removed = Some(Removal::Swipe);
            }
        }
    }

    fn drag_move(&mut self, position: Pixels, cx: &mut Context<Self>) {
        let Some((key, press_x)) = self.press else {
            return;
        };
        let swipe = self.swipe.get_or_insert_with(|| Swipe {
            key,
            press_x,
            started_at: Instant::now(),
            offset: 0.,
        });
        // A drag to the left has friction, as in Sonner. The toast moves a
        // small distance, and then stops.
        let delta = f32::from(position - swipe.press_x);
        swipe.offset = if delta >= 0. { delta } else { delta / (1.5 + delta.abs() / 20.) };
        let offset = swipe.offset;
        if let Some(e) = self.entries.iter_mut().find(|e| e.key == key) {
            e.motion.x = rest(offset);
        }
        cx.notify();
    }

    /// The place, the size, and the opacity of each toast for this frame.
    fn targets(&self) -> Vec<Targets> {
        let expanded = self.expanded();
        let front = self.entries.iter().find(|e| e.removed.is_none());
        let front_height = front.map_or(ESTIMATED_HEIGHT, |e| e.natural_height());
        let mut index = 0;
        let mut below = 0.;
        self.entries
            .iter()
            .map(|e| {
                let height = e.natural_height();
                let i = index as f32;
                let mut to = if index == 0 {
                    Targets { y: 0., x: 0., height, scale: 1., opacity: 1., content: 1. }
                } else if expanded {
                    Targets { y: below + GAP * i, x: 0., height, scale: 1., opacity: 1., content: 1. }
                } else {
                    // The toasts behind get the height of the front toast,
                    // and get smaller about their center, as a CSS scale
                    // does. Each one shows `GAP` above the toast in front.
                    let scale = 1. - SCALE_STEP * i;
                    Targets {
                        y: GAP * i + front_height * (1. - scale) / 2.,
                        x: 0.,
                        height: front_height,
                        scale,
                        opacity: 1.,
                        content: 0.,
                    }
                };
                if index >= VISIBLE_TOASTS {
                    to.opacity = 0.;
                }
                match e.removed {
                    None => {
                        index += 1;
                        below += height;
                    }
                    // The front toast goes down out of the stack. The
                    // others fade where they are.
                    Some(Removal::Timeout) => {
                        to = Targets { opacity: 0., ..self.current(e) };
                        if e.motion.y.position <= PIXEL_EPSILON {
                            to.y = -e.natural_height();
                        }
                    }
                    Some(Removal::Swipe) => {
                        to = Targets { x: TOAST_WIDTH + OFFSET, opacity: 0., ..self.current(e) };
                    }
                }
                to
            })
            .collect()
    }

    /// The targets that keep a toast where it is.
    fn current(&self, e: &Entry) -> Targets {
        let m = &e.motion;
        Targets {
            y: m.y.position,
            x: m.x.position,
            height: m.height.position,
            scale: m.scale.position,
            opacity: m.opacity.position,
            content: m.content.position,
        }
    }

    /// Moves the toasts one frame, and drops the toasts that faded out.
    fn step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let dt = self.stepped_at.map_or(0., |at| now.duration_since(at).as_secs_f32());
        let targets = self.targets();
        let swiped = self.swipe.as_ref().map(|s| s.key);
        let reduce = cx.reduce_motion();
        let mut moving = false;
        for (e, to) in self.entries.iter_mut().zip(targets) {
            if reduce {
                e.motion.snap(to);
            } else {
                moving |= e.motion.step(to, dt, swiped == Some(e.key));
            }
        }
        self.entries
            .retain(|e| e.removed.is_none() || e.motion.opacity.position > UNIT_EPSILON);
        if self.entries.is_empty() {
            self.hovered = false;
            self.press = None;
        }
        // A toast that goes away or hides under the pointer sends no
        // hover end.
        if let Some(key) = self.hovered_toast
            && !self.is_visible(key)
        {
            self.hovered_toast = None;
        }
        self.stepped_at = moving.then_some(now);
        if moving {
            window.request_animation_frame();
        }
    }

    fn render_toast(&self, e: &Entry, cx: &mut Context<Self>) -> AnyElement {
        // A hidden toast takes no pointer events. In an open stack, it sits
        // above the top toast, and its edge would open and close the stack.
        let interactive = e.removed.is_none() && self.is_visible(e.key);
        let t = cx.theme();
        let m = &e.motion;
        let scale = m.scale.position;
        let width = TOAST_WIDTH * scale;
        let height = m.height.position * scale;
        let key = e.key;
        let measured = e.height.clone();
        let icon = match e.toast.kind {
            ToastKind::Message => None,
            ToastKind::Success => Some(Icon::new(IconName::CircleCheck).into_any_element()),
            ToastKind::Info => Some(Icon::new(IconName::Info).into_any_element()),
            ToastKind::Warning => Some(Icon::new(IconName::TriangleAlert).into_any_element()),
            ToastKind::Error => Some(Icon::new(IconName::CircleX).into_any_element()),
            ToastKind::Loading => {
                Some(spinner().with_size(px(ICON_SIZE)).into_any_element())
            }
        };

        // The content keeps the full width, so its text wraps the same way
        // in a small toast behind the stack. The toast cuts off the rest.
        let content = div()
            .absolute()
            .top_0()
            .left(px((width - TOAST_WIDTH) / 2.))
            .w(px(TOAST_WIDTH))
            .p(px(PADDING))
            .flex()
            .items_center()
            .gap(px(6.))
            .opacity(m.content.position)
            .when_some(icon, |d, icon| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .size(px(ICON_SIZE))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(ICON_SIZE))
                        .child(icon),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .line_height(relative(1.5))
                            .child(e.toast.title.clone()),
                    )
                    .when_some(e.toast.description.clone(), |d, description| {
                        d.child(
                            div()
                                .text_color(t.muted_foreground)
                                .line_height(relative(1.4))
                                .child(description),
                        )
                    }),
            )
            .when_some(e.toast.action.clone().filter(|_| interactive), |d, action| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            Button::new(("toast-action", key))
                                .label(action.label.clone())
                                .outline()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    (action.handler)(cx);
                                    this.close(key, cx);
                                })),
                        ),
                )
            })
            .when(interactive, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .self_start()
                        // A press on the button does not start a swipe of
                        // the toast.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            Button::new(("close-toast", key))
                                .icon(Icon::new(IconName::X))
                                .ghost()
                                .xsmall()
                                .tooltip("Close")
                                .on_click(cx.listener(move |this, _, _, cx| this.close(key, cx))),
                        ),
                )
            })
            // Reads the height of the content after each layout. A change
            // moves the toasts again in the next frame.
            .child(
                canvas(
                    move |bounds, window, _| {
                        let h = f32::from(bounds.size.height);
                        if (measured.get() - h).abs() > 0.5 {
                            measured.set(h);
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );

        div()
            .id(("toast", key))
            .absolute()
            .bottom(px(m.y.position))
            .right(px((TOAST_WIDTH - width) / 2. - m.x.position))
            .w(px(width))
            // The border is outside the content.
            .h(px(height + 2.))
            .rounded(px(RADIUS))
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow(vec![BoxShadow {
                color: hsla(0., 0., 0., 0.1),
                offset: point(px(0.), px(4.)),
                blur_radius: px(12.),
                spread_radius: px(0.),
                inset: false,
            }])
            .overflow_hidden()
            .opacity(m.opacity.position)
            .when(interactive, |d| {
                d.occlude()
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.hovered_toast = Some(key);
                        } else if this.hovered_toast == Some(key) {
                            this.hovered_toast = None;
                        }
                        cx.notify();
                    }))
                    // A click on a toast behind opens the stack, also after a
                    // key press, when GPUI shows no hover.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            this.press = Some((key, ev.position.x));
                            this.hovered_toast = Some(key);
                            cx.notify();
                        }),
                    )
                    .on_drag(SwipeDrag, |_, _, _, cx| cx.new(|_| SwipeDrag))
            })
            .child(content)
            .into_any_element()
    }
}

/// Shows a set of sample toasts, for a manual test of the toaster. Start
/// a debug build with `TOAST_DEMO=1` to use it.
#[cfg(debug_assertions)]
fn demo(cx: &mut Context<Toaster>) {
    cx.spawn(async move |this, cx| {
        let samples = [
            Toast::success("Removed 3 volumes. 1.2 GB is free."),
            Toast::error("The container could not start.")
                .description("Bind for 0.0.0.0:5432 failed: port is already allocated."),
            Toast::info("Colima is up to date."),
            Toast::warning("The disk of the default machine is almost full."),
            Toast::loading("Pulling postgres:16…").id("demo-pull"),
        ];
        for toast in samples {
            cx.background_executor().timer(Duration::from_millis(700)).await;
            let _ = this.update(cx, |t, cx| t.push(toast, cx));
        }
        // The loading toast changes in place to a success toast.
        cx.background_executor().timer(Duration::from_millis(2000)).await;
        let _ = this.update(cx, |t, cx| t.push(Toast::success("Pulled postgres:16.").id("demo-pull"), cx));
    })
    .detach();
}

impl Render for Toaster {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.end_swipe(cx);
        self.tick(window, cx);
        self.step(window, cx);
        if self.entries.is_empty() {
            return div().into_any_element();
        }
        // The stack takes the pointer over the area of all the toasts, so
        // the pointer in a gap of an open stack keeps it open.
        let height = self
            .entries
            .iter()
            .filter(|e| e.removed.is_none())
            .take(VISIBLE_TOASTS)
            .map(|e| {
                let m = &e.motion;
                m.y.position + m.height.position * m.scale.position + 2.
            })
            .fold(0., f32::max);
        // Paint the front toast last, so it is on top.
        let toasts: Vec<_> = self.entries.iter().rev().map(|e| self.render_toast(e, cx)).collect();
        div()
            .id("toaster")
            .absolute()
            .right(px(OFFSET))
            .bottom(px(OFFSET))
            .w(px(TOAST_WIDTH))
            .h(px(height))
            .text_size(px(text::BODY))
            .text_color(cx.theme().foreground)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.hovered = *hovered;
                cx.notify();
            }))
            .on_drag_move(cx.listener(|this, ev: &DragMoveEvent<SwipeDrag>, _, cx| {
                this.drag_move(ev.event.position.x, cx);
            }))
            .children(toasts)
            .into_any_element()
    }
}

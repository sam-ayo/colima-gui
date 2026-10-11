//! The left navigation: the Docker sections, the Colima
//! machines, and the settings button.

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::key_nav::{self, BoundsCell};
use super::{SIDEBAR_INSET, SIDEBAR_RADIUS, TOOLBAR_HEIGHT, drag_area, native, settings, spinner, tabular_nums, text};
use crate::colima::Machine;
use crate::docker::stats::format_bytes;
use crate::state::usage_store::Usage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Containers,
    Volumes,
    Images,
    Networks,
}

impl Section {
    /// All the sections, in the order of the sidebar.
    pub const ALL: [Section; 4] = [
        Section::Containers,
        Section::Volumes,
        Section::Images,
        Section::Networks,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Containers => "Containers",
            Section::Volumes => "Volumes",
            Section::Images => "Images",
            Section::Networks => "Networks",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Section::Containers => IconName::Box,
            Section::Volumes => IconName::Database,
            Section::Images => IconName::Layers,
            Section::Networks => IconName::Globe,
        }
    }
}

type SelectHandler = Rc<dyn Fn(Section, &mut Window, &mut App)>;
type MachineHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;
type HoverHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
type ClickHandler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Sidebar {
    active: Section,
    width: Pixels,
    machines: Vec<Machine>,
    busy: HashSet<String>,
    /// The profile that the app connects to.
    current: String,
    /// True when no machine runs. The sections then have no data to show.
    sections_disabled: bool,
    /// The row at the keyboard cursor: the sections in the order of
    /// `Section::ALL`, then the machines. It is `None` when the sidebar does
    /// not have the focus.
    cursor: Option<usize>,
    cursor_bounds: BoundsCell,
    on_hover: HoverHandler,
    on_select: SelectHandler,
    on_open: MachineHandler,
    on_menu: MachineHandler,
    on_new_machine: ClickHandler,
    /// The resource use of the VM of the current machine, if it runs. The
    /// use is `None` until the first read ends.
    usage: Option<(String, Option<Usage>)>,
}

impl Sidebar {
    pub fn new(
        active: Section,
        width: Pixels,
        on_select: impl Fn(Section, &mut Window, &mut App) + 'static,
    ) -> Self {
        Sidebar {
            active,
            width,
            machines: Vec::new(),
            busy: HashSet::new(),
            current: String::new(),
            sections_disabled: false,
            cursor: None,
            cursor_bounds: BoundsCell::default(),
            on_hover: Rc::new(|_, _, _| {}),
            on_select: Rc::new(on_select),
            on_open: Rc::new(|_, _, _| {}),
            on_menu: Rc::new(|_, _, _| {}),
            on_new_machine: Rc::new(|_, _| {}),
            usage: None,
        }
    }

    pub fn machines(
        mut self,
        machines: Vec<Machine>,
        busy: HashSet<String>,
        current: String,
        on_open: impl Fn(&str, &mut Window, &mut App) + 'static,
        on_menu: impl Fn(&str, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.machines = machines;
        self.busy = busy;
        self.current = current;
        self.sections_disabled = !self.machines.is_empty() && !self.machines.iter().any(Machine::is_running);
        self.on_open = Rc::new(on_open);
        self.on_menu = Rc::new(on_menu);
        self
    }

    /// The button next to the "Machines" heading calls `on_new`.
    pub fn on_new_machine(mut self, on_new: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_new_machine = Rc::new(on_new);
        self
    }

    /// Shows the gauges of the resource use of the machine `name`. A `None`
    /// use shows placeholders until the first read ends.
    pub fn usage(mut self, usage: Option<(String, Option<Usage>)>) -> Self {
        self.usage = usage;
        self
    }

    /// Shows the keyboard cursor on a row, and records the bounds of that
    /// row in `bounds`.
    /// `on_hover` gets the position of a row that the pointer enters, so
    /// the arrow keys start from that row.
    pub fn cursor(
        mut self,
        cursor: Option<usize>,
        bounds: BoundsCell,
        on_hover: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.cursor = cursor;
        self.cursor_bounds = bounds;
        self.on_hover = Rc::new(on_hover);
        self
    }

    fn heading(label: &'static str, cx: &App) -> Div {
        let t = cx.theme();
        div()
            .px_2()
            .pt_4()
            .pb_1()
            .text_size(px(text::SUBHEADLINE))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.muted_foreground)
            .child(label)
    }

    /// The row style that the sections and the machines share. The row at
    /// position `ix` gets the hover fill when the keyboard cursor is on it.
    fn item(&self, id: impl Into<ElementId>, ix: usize, active: bool, cx: &App) -> Stateful<Div> {
        let t = cx.theme();
        let accent = t.sidebar_accent;
        let cursor = self.cursor == Some(ix);
        let on_hover = self.on_hover.clone();
        h_flex()
            .id(id)
            .relative()
            .h(px(28.))
            .gap_2p5()
            .px_2()
            .rounded(px(7.))
            .text_size(px(text::BODY))
            .text_color(t.sidebar_foreground)
            // As in the system sidebars, the selected row has a fill and no
            // bold text. A hovered row has a lighter fill of the same color.
            .map(|d| {
                if active {
                    d.bg(accent).text_color(t.sidebar_accent_foreground)
                } else if cursor {
                    d.bg(accent.opacity(0.5))
                } else if self.cursor.is_none() {
                    d.hover(|s| s.bg(accent.opacity(0.5)))
                } else {
                    // With the focus, only the cursor row has the hover fill.
                    d
                }
            })
            .on_hover(move |hovered, window, cx| {
                if *hovered {
                    on_hover(ix, window, cx)
                }
            })
            .when(cursor, |d| d.child(key_nav::track_bounds(self.cursor_bounds.clone())))
    }

    fn render_section(&self, section: Section, cx: &App) -> AnyElement {
        let t = cx.theme();
        let ix = Section::ALL.iter().position(|&s| s == section).unwrap_or_default();
        if self.sections_disabled {
            // A disabled row is gray, as a stopped machine is. It has no
            // fill and does not answer the pointer.
            return h_flex()
                .id(section.label())
                .h(px(28.))
                .gap_2p5()
                .px_2()
                .text_size(px(text::BODY))
                .text_color(t.muted_foreground)
                .child(Icon::new(section.icon()).small().text_color(t.muted_foreground))
                .child(div().flex_1().child(section.label()))
                .into_any_element();
        }
        let active = section == self.active;
        let on_select = self.on_select.clone();
        self.item(section.label(), ix, active, cx)
            .child(Icon::new(section.icon()).small().text_color(native::accent(cx)))
            .child(div().flex_1().child(section.label()))
            // Select on the press, so the answer is immediate.
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                on_select(section, window, cx)
            })
            .into_any_element()
    }

    /// The "Machines" heading, with a button that makes a new machine.
    fn machines_heading(&self, cx: &App) -> impl IntoElement {
        let on_new = self.on_new_machine.clone();
        h_flex()
            .justify_between()
            .child(Self::heading("Machines", cx))
            .child(
                div().pt_3().child(
                    Button::new("new-machine")
                        .icon(Icon::new(IconName::Plus))
                        .custom(native::toolbar_button(cx))
                        .xsmall()
                        .tooltip("New Machine (⇧⌘N)")
                        .on_click(move |_, window, cx| on_new(window, cx)),
                ),
            )
    }

    /// The CPU, memory, and disk use of the VM of the current machine.
    /// Before the first read ends, the card shows placeholders.
    fn render_usage(&self, cx: &App) -> Option<impl IntoElement> {
        let (name, u) = self.usage.as_ref()?;
        let t = cx.theme();
        let fraction = |used: u64, total: u64| if total == 0 { 0. } else { used as f32 / total as f32 };
        let of = |used: u64, total: u64| format!("{} of {}", format_bytes(used), format_bytes(total));
        let cpu = u.map(|u| {
            let cpus = if u.cpus == 1 { "1 CPU".to_string() } else { format!("{} CPUs", u.cpus) };
            (u.cpu_percent / 100., format!("{:.0}% of {cpus}", u.cpu_percent))
        });
        let mem = u.map(|u| (fraction(u.mem_used, u.mem_total), of(u.mem_used, u.mem_total)));
        let disk = u.map(|u| (fraction(u.disk_used, u.disk_total), of(u.disk_used, u.disk_total)));
        Some(
            v_flex()
                .mx_2p5()
                .mb_2()
                .p_2p5()
                .gap_2()
                .rounded(px(10.))
                .bg(t.sidebar_accent.opacity(0.35))
                .child(
                    h_flex()
                        .gap_1p5()
                        .text_size(px(text::SUBHEADLINE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.muted_foreground)
                        .child(Icon::new(IconName::Server).xsmall())
                        .child(div().min_w_0().truncate().child(name.clone())),
                )
                .child(gauge("CPU", cpu, cx))
                .child(gauge("Memory", mem, cx))
                .child(gauge("Disk", disk, cx)),
        )
    }

    fn render_machine(&self, ix: usize, machine: &Machine, cx: &App) -> impl IntoElement {
        let t = cx.theme();
        let running = machine.is_running();
        let busy = self.busy.contains(&machine.name);
        let current = machine.name == self.current;
        // A running machine has the accent color, as the section icons
        // have. A stopped machine is gray.
        let color = if running { native::accent(cx) } else { t.muted_foreground };
        let on_open = self.on_open.clone();
        let on_menu = self.on_menu.clone();
        let open_name = machine.name.clone();
        let menu_name = machine.name.clone();
        self.item(("machine", ix), Section::ALL.len() + ix, current, cx)
            // A click on the row shows the containers of this machine. A
            // right click shows the machine actions.
            .on_click(move |_, window, cx| on_open(&open_name, window, cx))
            .on_mouse_down(MouseButton::Right, move |_, window, cx| on_menu(&menu_name, window, cx))
            .child(Icon::new(IconName::Server).small().text_color(color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(!running, |d| d.text_color(t.muted_foreground))
                    .child(machine.name.clone()),
            )
            .when(busy, |d| d.child(spinner().xsmall().color(t.muted_foreground)))
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme();
        let panel = v_flex()
            .size_full()
            .rounded(px(SIDEBAR_RADIUS))
            .bg(t.sidebar)
            .border_1()
            .border_color(t.sidebar_border)
            // The traffic lights sit in this area.
            .child(drag_area("sidebar-drag").w_full().h(px(TOOLBAR_HEIGHT - SIDEBAR_INSET)).flex_shrink_0())
            .child(
                v_flex()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2p5()
                    .pb_2()
                    .gap_px()
                    .child(Self::heading("Docker", cx).pt_0())
                    .children(Section::ALL.map(|s| self.render_section(s, cx)))
                    .child(self.machines_heading(cx))
                    .children(
                        self.machines
                            .iter()
                            .enumerate()
                            .map(|(ix, m)| self.render_machine(ix, m, cx)),
                    ),
            )
            .children(self.render_usage(cx))
            .child(h_flex().flex_shrink_0().px_2p5().pb_2p5().child(settings::settings_button(cx)));
        div()
            .relative()
            .w(self.width + px(2. * SIDEBAR_INSET))
            .h_full()
            .flex_shrink_0()
            .p(px(SIDEBAR_INSET))
            .bg(t.background)
            .child(panel)
    }
}

/// A gauge: a label and a value, with a bar below. The bar turns yellow
/// above 75% and red above 90%. A `None` reading shows placeholders of the
/// same size, so the card does not change height when the reading arrives.
fn gauge(label: &'static str, reading: Option<(f32, String)>, cx: &App) -> Div {
    let t = cx.theme();
    let value = match &reading {
        Some((_, value)) => div()
            .min_w_0()
            .truncate()
            .font_features(tabular_nums())
            .text_color(t.muted_foreground)
            .child(value.clone())
            .into_any_element(),
        None => placeholder("value", div().w(px(72.)).h(px(10.)).rounded_sm(), cx).into_any_element(),
    };
    let bar = match reading {
        Some((fraction, _)) => {
            let fraction = fraction.clamp(0., 1.);
            let color = if fraction > 0.9 {
                t.red
            } else if fraction > 0.75 {
                t.yellow
            } else {
                native::accent(cx)
            };
            div()
                .h(px(4.))
                .w_full()
                .rounded_full()
                .bg(gauge_track(cx))
                .child(div().h_full().w(relative(fraction)).rounded_full().bg(color))
                .into_any_element()
        }
        None => placeholder("bar", div().w_full().h(px(4.)).rounded_full(), cx).into_any_element(),
    };
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .gap_2()
                .text_size(px(text::SUBHEADLINE))
                .child(div().text_color(t.sidebar_foreground).child(label))
                .child(value),
        )
        .child(bar)
}

/// The color of the empty part of a gauge bar.
fn gauge_track(cx: &App) -> Hsla {
    cx.theme().muted_foreground.opacity(0.2)
}

/// Makes `shape` pulse in the color of the gauge track, in place of a
/// value that has not arrived. The theme skeleton color is too dark on the
/// sidebar card.
fn placeholder(id: &'static str, shape: Div, cx: &App) -> AnimationElement<Div> {
    shape.bg(gauge_track(cx)).with_animation(
        id,
        Animation::new(Duration::from_secs(2)).repeat().with_easing(bounce(ease_in_out)),
        |d, delta| d.opacity(1. - delta * 0.5),
    )
}

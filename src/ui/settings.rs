//! The settings button at the bottom of the sidebar. It opens a popover
//! that shows the resource use of the current machine, and changes the
//! appearance and the accent color, as the View menu does.

use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::native::{self, AccentColor, Appearance, SetAccentColor, SetAppearance};
use super::{tabular_nums, text};
use crate::docker::stats::format_bytes;
use crate::state::usage_store::{Usage, UsageStore};

/// The width of the popover. The three appearance cards fill it.
const POPOVER_WIDTH: f32 = 252.;
/// The diameter of an accent color swatch, as in System Settings.
const SWATCH: f32 = 18.;

/// A button that opens the settings popover above it. The popover shows
/// the resource use in `usage` while a machine runs.
pub fn settings_button(usage: Option<Entity<UsageStore>>, cx: &App) -> impl IntoElement {
    Popover::new("settings")
        .anchor(Anchor::BottomLeft)
        .trigger(
            Button::new("settings-button")
                .icon(Icon::new(IconName::Settings))
                .custom(native::toolbar_button(cx))
                .small()
                .tooltip("Settings"),
        )
        .content(move |_, _, cx| {
            // The store is read at each render, so the gauges change while
            // the popover is open.
            let usage = usage.as_ref().and_then(|u| {
                let u = u.read(cx);
                u.profile().map(|p| (p.to_string(), u.usage))
            });
            content(usage, cx)
        })
}

fn content(usage: Option<(String, Option<Usage>)>, cx: &App) -> Div {
    let appearance = cx.try_global::<Appearance>().copied().unwrap_or_default();
    let accent = cx.try_global::<AccentColor>().copied().unwrap_or_default();
    v_flex()
        .w(px(POPOVER_WIDTH))
        .gap_3()
        .text_size(px(text::BODY))
        .when_some(usage, |d, (name, u)| {
            d.child(render_usage(&name, u, cx))
                .child(div().h(px(1.)).w_full().bg(cx.theme().border))
        })
        .child(
            h_flex()
                .justify_between()
                .children(Appearance::ALL.map(|a| card(a, a == appearance, cx))),
        )
        .child(swatches(accent, cx))
}

/// The CPU, memory, and disk use of the VM of the machine `name`. Before
/// the first read ends, the gauges show placeholders.
fn render_usage(name: &str, u: Option<Usage>, cx: &App) -> Div {
    let t = cx.theme();
    let fraction = |used: u64, total: u64| if total == 0 { 0. } else { used as f32 / total as f32 };
    let of = |used: u64, total: u64| format!("{} of {}", format_bytes(used), format_bytes(total));
    let cpu = u.map(|u| {
        let cpus = if u.cpus == 1 { "1 CPU".to_string() } else { format!("{} CPUs", u.cpus) };
        (u.cpu_percent / 100., format!("{:.0}% of {cpus}", u.cpu_percent))
    });
    let mem = u.map(|u| (fraction(u.mem_used, u.mem_total), of(u.mem_used, u.mem_total)));
    let disk = u.map(|u| (fraction(u.disk_used, u.disk_total), of(u.disk_used, u.disk_total)));
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .gap_1p5()
                .text_size(px(text::SUBHEADLINE))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.muted_foreground)
                .child(Icon::new(IconName::Server).xsmall())
                .child(div().min_w_0().truncate().child(name.to_string())),
        )
        .child(gauge("CPU", cpu, cx))
        .child(gauge("Memory", mem, cx))
        .child(gauge("Disk", disk, cx))
}

/// A gauge: a label and a value, with a bar below. The bar turns yellow
/// above 75% and red above 90%. A `None` reading shows placeholders of the
/// same size, so the popover does not change height when the reading arrives.
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
                .child(div().text_color(t.foreground).child(label))
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
/// popover.
fn placeholder(id: &'static str, shape: Div, cx: &App) -> AnimationElement<Div> {
    shape.bg(gauge_track(cx)).with_animation(
        id,
        Animation::new(Duration::from_secs(2)).repeat().with_easing(bounce(ease_in_out)),
        |d, delta| d.opacity(1. - delta * 0.5),
    )
}

/// The click comes while GPUI updates this window, and
/// `App::dispatch_action` cannot update it again. Send the action through
/// the window.
fn set_appearance(choice: Appearance) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_, window, cx| window.dispatch_action(Box::new(SetAppearance(choice)), cx)
}

fn set_accent(choice: AccentColor) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_, window, cx| window.dispatch_action(Box::new(SetAccentColor(choice)), cx)
}

/// The colors of a small window picture in one appearance.
struct Palette {
    window: Hsla,
    sidebar: Hsla,
    line: Hsla,
}

const LIGHT: Palette = Palette {
    window: Hsla { h: 0., s: 0., l: 0.98, a: 1. },
    sidebar: Hsla { h: 0., s: 0., l: 0.89, a: 1. },
    line: Hsla { h: 0., s: 0., l: 0.8, a: 1. },
};

const DARK: Palette = Palette {
    window: Hsla { h: 0., s: 0., l: 0.17, a: 1. },
    sidebar: Hsla { h: 0., s: 0., l: 0.25, a: 1. },
    line: Hsla { h: 0., s: 0., l: 0.36, a: 1. },
};

/// A card of the appearance: a small window picture with the name below.
/// The current choice has a ring in the accent color.
fn card(choice: Appearance, selected: bool, cx: &App) -> Stateful<Div> {
    let t = cx.theme();
    let accent = native::accent(cx);
    let picture = match choice {
        Appearance::Light => window_picture(&LIGHT, true, accent),
        Appearance::Dark => window_picture(&DARK, true, accent),
        // The left half is light and the right half is dark.
        Appearance::System => h_flex()
            .size_full()
            .child(window_picture(&LIGHT, true, accent))
            .child(window_picture(&DARK, false, accent)),
    };
    v_flex()
        .id(("appearance", choice as usize))
        .items_center()
        .gap_1()
        .cursor_pointer()
        .child(
            div()
                .w(px(74.))
                .h(px(50.))
                .p(px(2.))
                .rounded(px(8.))
                .border_2()
                .border_color(if selected { accent } else { transparent_black() })
                .child(
                    div()
                        .size_full()
                        .rounded(px(4.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(black().opacity(0.15))
                        .child(picture),
                ),
        )
        .child(
            div()
                .text_size(px(text::CALLOUT))
                .text_color(if selected { t.foreground } else { t.muted_foreground })
                .when(selected, |d| d.font_weight(FontWeight::MEDIUM))
                .child(choice.name()),
        )
        .on_click(set_appearance(choice))
}

/// A small window with a sidebar, a bar in the accent color, and lines of
/// text.
fn window_picture(p: &Palette, sidebar: bool, accent: Hsla) -> Div {
    let line = |w: f32| div().w(px(w)).h(px(3.)).rounded_full().bg(p.line);
    h_flex()
        .flex_1()
        .size_full()
        .bg(p.window)
        .when(sidebar, |d| d.child(div().w(px(16.)).h_full().bg(p.sidebar)))
        .child(
            v_flex()
                .flex_1()
                .p(px(5.))
                .gap(px(4.))
                .child(div().w(px(18.)).h(px(4.)).rounded_full().bg(accent))
                .child(line(28.))
                .child(line(22.)),
        )
}

/// The swatches of all the accent colors, in one row, with the same gap
/// between each.
fn swatches(current: AccentColor, cx: &App) -> Div {
    h_flex()
        .gap(px(SWATCH / 3.))
        .child(swatch(AccentColor::System, current, cx))
        .children(AccentColor::COLORS.map(|c| swatch(c, current, cx)))
}

/// A round swatch of `choice`. The current choice has a ring in its color,
/// with a gap, as the current appearance card has.
fn swatch(choice: AccentColor, current: AccentColor, cx: &App) -> Stateful<Div> {
    let color = native::swatch(choice, cx);
    let selected = choice == current;
    let name = choice.name();
    div()
        .id(("accent", choice as usize))
        .size(px(SWATCH + 6.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_2()
        .border_color(if selected { color } else { transparent_black() })
        .cursor_pointer()
        .child(
            div()
                .size(px(SWATCH))
                .rounded_full()
                .bg(color)
                .border_1()
                .border_color(black().opacity(0.12)),
        )
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(name).build(window, cx))
        .on_click(set_accent(choice))
}

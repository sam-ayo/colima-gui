//! The settings button at the bottom of the sidebar. It opens a popover
//! that changes the appearance and the accent color, as the View menu does.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::native::{self, AccentColor, Appearance, SetAccentColor, SetAppearance};
use super::text;

/// The width of the popover. The three appearance cards fill it.
const POPOVER_WIDTH: f32 = 252.;
/// The diameter of an accent color swatch, as in System Settings.
const SWATCH: f32 = 18.;

/// A button that opens the settings popover above it.
pub fn settings_button(cx: &App) -> impl IntoElement {
    Popover::new("settings")
        .anchor(Anchor::BottomLeft)
        .trigger(
            Button::new("settings-button")
                .icon(Icon::new(IconName::Settings))
                .custom(native::toolbar_button(cx))
                .small()
                .tooltip("Appearance"),
        )
        .content(|_, _, cx| content(cx))
}

fn content(cx: &App) -> Div {
    let appearance = cx.try_global::<Appearance>().copied().unwrap_or_default();
    let accent = cx.try_global::<AccentColor>().copied().unwrap_or_default();
    v_flex()
        .w(px(POPOVER_WIDTH))
        .gap_3()
        .text_size(px(text::BODY))
        .child(
            h_flex()
                .justify_between()
                .children(Appearance::ALL.map(|a| card(a, a == appearance, cx))),
        )
        .child(swatches(accent, cx))
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

/// The swatches of all the accent colors. A gap keeps `System` apart from
/// the fixed colors, as the separator does in the menu.
fn swatches(current: AccentColor, cx: &App) -> Div {
    h_flex()
        .gap(px(SWATCH / 3.))
        .child(swatch(AccentColor::System, current, cx))
        .child(div().w(px(2.)))
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

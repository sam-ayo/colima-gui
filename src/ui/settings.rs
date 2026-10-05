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

/// The width of the popover. The three appearance segments fill it.
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
    let t = cx.theme();
    v_flex()
        .w(px(POPOVER_WIDTH))
        .gap_2()
        .text_size(px(text::BODY))
        .child(heading("Appearance", cx))
        .child(
            h_flex()
                .p(px(2.))
                .gap(px(2.))
                .rounded(px(7.))
                .bg(native::overlay(cx).opacity(if t.is_dark() { 0.08 } else { 0.06 }))
                .children(Appearance::ALL.map(|a| segment(a, a == appearance, cx))),
        )
        .child(heading("Accent Color", cx).pt_2())
        .child(
            h_flex()
                .gap_2()
                .child(swatch(AccentColor::System, accent, cx))
                // A gap keeps `System` apart from the fixed colors, as the
                // separator does in the menu.
                .child(div().w(px(2.)))
                .children(AccentColor::COLORS.map(|c| swatch(c, accent, cx))),
        )
        .child(
            div()
                .text_size(px(text::CALLOUT))
                .text_color(t.muted_foreground)
                .child(accent.name()),
        )
}

/// A segment of the appearance control. As in the segmented controls of
/// macOS, the selected segment has a raised fill on a sunken track.
fn segment(choice: Appearance, selected: bool, cx: &App) -> Stateful<Div> {
    let t = cx.theme();
    let raised = if t.is_dark() { native::paper(cx).opacity(0.2) } else { native::paper(cx) };
    div()
        .id(choice.name())
        .flex_1()
        .h(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .text_color(t.foreground)
        .map(|d| {
            if selected {
                d.bg(raised).shadow_xs().font_weight(FontWeight::MEDIUM)
            } else {
                d.cursor_pointer().hover(|s| s.bg(native::control_hover(cx)))
            }
        })
        .child(choice.name())
        .on_click(move |_, _, cx| cx.dispatch_action(&SetAppearance(choice)))
}

fn heading(label: &'static str, cx: &App) -> Div {
    div()
        .text_size(px(text::SUBHEADLINE))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

/// A round swatch of `choice`. The current choice has a ring and a white
/// dot, as in System Settings.
fn swatch(choice: AccentColor, current: AccentColor, cx: &App) -> Stateful<Div> {
    let color = native::swatch(choice, cx);
    let selected = choice == current;
    let name = choice.name();
    div()
        .id(name)
        .size(px(SWATCH + 6.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_2()
        .border_color(if selected { color.opacity(0.5) } else { transparent_black() })
        .cursor_pointer()
        .child(
            div()
                .size(px(SWATCH))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(color)
                .border_1()
                .border_color(black().opacity(0.12))
                .when(selected, |d| d.child(div().size(px(6.)).rounded_full().bg(native::paper(cx)))),
        )
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(name).build(window, cx))
        .on_click(move |_, _, cx| cx.dispatch_action(&SetAccentColor(choice)))
}

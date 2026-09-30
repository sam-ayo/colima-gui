//! The left navigation.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::{TOOLBAR_HEIGHT, drag_area, tabular_nums};
use crate::docker::EngineStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Containers,
    Images,
    Volumes,
    Networks,
}

impl Section {
    pub const ALL: [Section; 4] = [
        Section::Containers,
        Section::Images,
        Section::Volumes,
        Section::Networks,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Containers => "Containers",
            Section::Images => "Images",
            Section::Volumes => "Volumes",
            Section::Networks => "Networks",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Section::Containers => IconName::Container,
            Section::Images => IconName::Layers,
            Section::Volumes => IconName::HardDrive,
            Section::Networks => IconName::Network,
        }
    }

    /// Only the containers section exists in the first version.
    pub fn is_available(self) -> bool {
        self == Section::Containers
    }
}

type SelectHandler = std::rc::Rc<dyn Fn(Section, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Sidebar {
    active: Section,
    running: usize,
    engine: EngineStatus,
    on_select: SelectHandler,
}

impl Sidebar {
    pub fn new(
        active: Section,
        running: usize,
        engine: EngineStatus,
        on_select: impl Fn(Section, &mut Window, &mut App) + 'static,
    ) -> Self {
        Sidebar {
            active,
            running,
            engine,
            on_select: std::rc::Rc::new(on_select),
        }
    }

    /// The engine state as a colored dot and a short label.
    fn render_engine(&self, cx: &App) -> impl IntoElement {
        let t = cx.theme();
        let (color, label) = match self.engine {
            EngineStatus::Connected => (t.green, "Colima is running"),
            EngineStatus::Connecting => (t.yellow, "Connecting…"),
            EngineStatus::NotRunning => (t.muted_foreground, "Colima is stopped"),
            EngineStatus::Error(_) => (t.red, "Engine error"),
        };
        h_flex()
            .gap_2()
            .px_2()
            .py_2()
            .text_xs()
            .text_color(t.muted_foreground)
            .child(div().size(px(7.)).rounded_full().bg(color))
            .child(label)
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme();
        let engine = self.render_engine(cx);
        let on_select = self.on_select.clone();
        v_flex()
            .w(px(212.))
            .h_full()
            .flex_shrink_0()
            .bg(t.sidebar)
            .border_r_1()
            .border_color(t.sidebar_border)
            // The traffic lights sit in this area.
            .child(drag_area("sidebar-drag").w_full().h(px(TOOLBAR_HEIGHT)).flex_shrink_0())
            .child(
                v_flex()
                    .flex_1()
                    .px_2p5()
                    .gap_px()
                    .child(
                        div()
                            .px_2()
                            .pb_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.muted_foreground)
                            .child("Docker"),
                    )
                    .children(Section::ALL.into_iter().map(|section| {
                        let active = section == self.active;
                        let on_select = on_select.clone();
                        h_flex()
                            .id(section.label())
                            .h(px(30.))
                            .gap_2p5()
                            .px_2()
                            .rounded(px(7.))
                            .text_sm()
                            .text_color(t.sidebar_foreground)
                            .when(active, |d| {
                                d.bg(t.sidebar_accent)
                                    .text_color(t.sidebar_accent_foreground)
                                    .font_weight(FontWeight::MEDIUM)
                            })
                            .when(!active, |d| {
                                d.hover(|d| d.bg(t.sidebar_accent.opacity(0.5)))
                                    .active(|d| d.bg(t.sidebar_accent.opacity(0.8)))
                            })
                            .child(
                                Icon::new(section.icon())
                                    .small()
                                    .text_color(if active { t.primary } else { t.muted_foreground }),
                            )
                            .child(div().flex_1().child(section.label()))
                            .when(section == Section::Containers && self.running > 0, |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .font_features(tabular_nums())
                                        .text_color(t.muted_foreground)
                                        .child(self.running.to_string()),
                                )
                            })
                            // Select on the press, so the answer is immediate.
                            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                on_select(section, window, cx)
                            })
                    })),
            )
            .child(div().px_2p5().pb_2().child(engine))
    }
}

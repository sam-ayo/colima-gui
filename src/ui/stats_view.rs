//! CPU and memory line graphs from the 60-sample history.

use gpui::*;
use gpui_component::{ActiveTheme, v_flex};

use crate::docker::stats::{Sample, format_bytes};
use crate::state::stats_store::HISTORY_LEN;
use crate::ui::{tabular_nums, text};

#[derive(IntoElement)]
pub struct StatsView {
    samples: Vec<Sample>,
    running: bool,
}

impl StatsView {
    pub fn new(samples: Vec<Sample>, running: bool) -> Self {
        StatsView { samples, running }
    }
}

/// Draws `values` as a filled line graph. The newest value is at the right
/// edge, and the graph always has room for `HISTORY_LEN` values.
fn graph(values: Vec<f32>, max: f32, line: Hsla, grid: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let w = bounds.size.width;
            let h = bounds.size.height;
            let origin = bounds.origin;
            // Grid lines at 50% and 100%.
            for frac in [0.0f32, 0.5] {
                let y = origin.y + h * frac;
                let mut b = PathBuilder::stroke(px(1.));
                b.move_to(point(origin.x, y));
                b.line_to(point(origin.x + w, y));
                if let Ok(p) = b.build() {
                    window.paint_path(p, grid);
                }
            }
            if values.len() < 2 || max <= 0.0 {
                return;
            }
            let step = w / (HISTORY_LEN - 1) as f32;
            let start = HISTORY_LEN - values.len();
            let pt = |i: usize, v: f32| {
                let x = origin.x + step * (start + i) as f32;
                let y = origin.y + h * (1.0 - (v / max).clamp(0.0, 1.0));
                point(x, y)
            };
            let mut fill = PathBuilder::fill();
            fill.move_to(point(pt(0, 0.0).x, origin.y + h));
            for (i, v) in values.iter().enumerate() {
                fill.line_to(pt(i, *v));
            }
            fill.line_to(point(pt(values.len() - 1, 0.0).x, origin.y + h));
            fill.close();
            if let Ok(p) = fill.build() {
                window.paint_path(p, line.opacity(0.15));
            }
            let mut stroke = PathBuilder::stroke(px(1.5));
            stroke.move_to(pt(0, values[0]));
            for (i, v) in values.iter().enumerate().skip(1) {
                stroke.line_to(pt(i, *v));
            }
            if let Ok(p) = stroke.build() {
                window.paint_path(p, line);
            }
        },
    )
    .w_full()
    .h(px(120.))
}

fn card(title: &str, value: String, sub: String, body: impl IntoElement, cx: &App) -> Div {
    let t = cx.theme();
    v_flex()
        .p_4()
        .gap_3()
        .rounded(px(12.))
        .bg(t.muted.opacity(0.5))
        .child(
            v_flex()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(text::SUBHEADLINE))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.muted_foreground)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .font_features(tabular_nums())
                        .child(div().text_size(px(text::TITLE2)).font_weight(FontWeight::SEMIBOLD).child(value))
                        .child(div().text_size(px(text::CALLOUT)).text_color(t.muted_foreground).child(sub)),
                ),
        )
        .child(body)
}

impl RenderOnce for StatsView {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme();
        if !self.running {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(t.muted_foreground)
                .child("The container is not running.");
        }
        let latest = self.samples.last().copied().unwrap_or_default();
        let cpu: Vec<f32> = self.samples.iter().map(|s| s.cpu_percent as f32).collect();
        let mem: Vec<f32> = self.samples.iter().map(|s| s.mem_bytes as f32).collect();
        let cpu_peak = cpu.iter().copied().fold(0.0, f32::max);
        // Scale to 100% (one CPU) until the use goes higher.
        let cpu_max = if cpu_peak <= 100.0 {
            100.0
        } else {
            (cpu_peak / 100.0).ceil() * 100.0
        };
        let mem_peak = mem.iter().copied().fold(0.0, f32::max);
        let mem_max = (mem_peak * 1.25).max(1.0);
        let limit = if latest.mem_limit > 0 {
            format!("of {}", format_bytes(latest.mem_limit))
        } else {
            String::new()
        };
        v_flex()
            .size_full()
            .px_4()
            .py_2()
            .gap_3()
            .child(card(
                "CPU",
                format!("{:.1}%", latest.cpu_percent),
                format!("scale {cpu_max:.0}%"),
                graph(cpu, cpu_max, t.blue, t.border.opacity(0.6)),
                cx,
            ))
            .child(card(
                "Memory",
                format_bytes(latest.mem_bytes),
                limit,
                graph(mem, mem_max, t.green, t.border.opacity(0.6)),
                cx,
            ))
            .child(
                div()
                    .text_size(px(text::SUBHEADLINE))
                    .text_color(t.muted_foreground)
                    .child("The last 60 seconds. One sample each second."),
            )
    }
}

//! A live, virtualized log view for one container.

use std::ops::Range;
use std::time::Duration;

use bollard::Docker;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::logs::{self, LogLine, LogStream};
use crate::runtime::{Backend, recv_batch};
use crate::state::log_buffer::{LogBuffer, MAX_LINES};
use crate::ui::{tabular_nums, text};

const LINE_HEIGHT: f32 = 18.;
/// Very long lines are cut for display, so text layout stays fast.
const MAX_DISPLAY_CHARS: usize = 2_000;

pub struct LogsView {
    buffer: LogBuffer,
    scroll: UniformListScrollHandle,
    /// True while the view shows the end of the log.
    follow: bool,
    ended: bool,
    filter: Entity<InputState>,
    _task: Task<()>,
    _subs: Vec<Subscription>,
}

impl LogsView {
    pub fn new(docker: &Docker, id: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let rx = Backend::global(cx).stream(logs::follow(docker, id));
        let task = cx.spawn(async move |this, cx| {
            while let Some(batch) = recv_batch(&rx, 8192).await {
                let ok = this.update(cx, |this, cx| {
                    this.buffer.extend(batch);
                    if this.follow {
                        this.scroll.scroll_to_bottom();
                    }
                    cx.notify();
                });
                if ok.is_err() {
                    return;
                }
                // Wait for one frame, so a fast log gives one render per frame.
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
            }
            let _ = this.update(cx, |this, cx| {
                this.ended = true;
                cx.notify();
            });
        });
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter logs"));
        let subs = vec![cx.subscribe(&filter, |this, state, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                let value = state.read(cx).value();
                this.buffer.set_filter(&value);
                if this.follow {
                    this.scroll.scroll_to_bottom();
                }
                cx.notify();
            }
        })];
        LogsView {
            buffer: LogBuffer::new(),
            scroll: UniformListScrollHandle::new(),
            follow: true,
            ended: false,
            filter,
            _task: task,
            _subs: subs,
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let dy = ev.delta.pixel_delta(window.line_height()).y;
        if dy > px(0.) {
            // A scroll up stops the follow mode.
            if self.follow {
                self.follow = false;
                cx.notify();
            }
        } else if dy < px(0.) && !self.follow && self.scroll.is_scrolled_to_end() == Some(true) {
            self.follow = true;
            cx.notify();
        }
    }

    fn jump_to_end(&mut self, cx: &mut Context<Self>) {
        self.follow = true;
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn render_line(line: &LogLine, cx: &App) -> AnyElement {
        let t = cx.theme();
        // RFC 3339: keep HH:MM:SS.mmm.
        let time = line.timestamp.get(11..23).unwrap_or("");
        let text: SharedString = if line.text.len() > MAX_DISPLAY_CHARS {
            let mut end = MAX_DISPLAY_CHARS;
            while !line.text.is_char_boundary(end) {
                end -= 1;
            }
            format!("{} …", &line.text[..end]).into()
        } else {
            line.text.clone().into()
        };
        h_flex()
            .w_full()
            .h(px(LINE_HEIGHT))
            .px_4()
            .gap_3()
            .whitespace_nowrap()
            .overflow_hidden()
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(t.muted_foreground)
                    .child(time.to_string()),
            )
            .child(
                div()
                    .when(line.stream == LogStream::Stderr, |d| d.text_color(t.red))
                    .child(text),
            )
            .into_any_element()
    }
}

impl Render for LogsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let count = self.buffer.visible_len();
        let status = if self.buffer.is_filtered() {
            format!("{count} of {} lines", self.buffer.len())
        } else if self.buffer.len() >= MAX_LINES {
            format!("{count} lines (limit {MAX_LINES})")
        } else {
            format!("{count} lines")
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .px_4()
                    .pb_2()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.filter).small().cleanable(true)),
                    )
                    .child(
                        div()
                            .text_size(px(text::SUBHEADLINE))
                            .font_features(tabular_nums())
                            .text_color(t.muted_foreground)
                            .child(status),
                    )
                    .when(self.ended, |d| {
                        d.child(div().text_size(px(text::SUBHEADLINE)).text_color(t.muted_foreground).child("· ended"))
                    })
                    .child(
                        Button::new("follow")
                            .icon(Icon::new(IconName::ArrowDownToLine))
                            .xsmall()
                            .when(self.follow, |b| b.primary())
                            .when(!self.follow, |b| b.ghost())
                            .tooltip("Follow the end of the log")
                            .on_click(cx.listener(|this, _, _, cx| this.jump_to_end(cx))),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .font_family(t.mono_font_family.clone())
                    .text_size(px(text::SUBHEADLINE))
                    .on_scroll_wheel(cx.listener(Self::on_scroll))
                    .child(
                        uniform_list(
                            "log-lines",
                            count,
                            cx.processor(|this, range: Range<usize>, _, cx| {
                                range
                                    .filter_map(|ix| this.buffer.visible_line(ix))
                                    .map(|line| Self::render_line(line, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .size_full()
                        .track_scroll(&self.scroll),
                    )
                    .child(Scrollbar::vertical(&self.scroll)),
            )
    }
}

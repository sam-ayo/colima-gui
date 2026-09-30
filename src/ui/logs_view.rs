//! A live, virtualized log view for one container. Long lines wrap.

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
use crate::ui::{code_line, tabular_nums, text};

const LINE_HEIGHT: f32 = 18.;
/// The space above the first line and below the last line.
const EDGE_PADDING: f32 = 8.;
/// Very long lines are cut for display, so text layout stays fast.
const MAX_DISPLAY_CHARS: usize = 2_000;

pub struct LogsView {
    buffer: LogBuffer,
    /// The list follows the end of the log until the user scrolls up.
    list: ListState,
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
                    let old = this.buffer.visible_len();
                    let removed = this.buffer.extend(batch);
                    let kept = old - removed;
                    this.list.splice(0..removed, 0);
                    this.list.splice(kept..kept, this.buffer.visible_len() - kept);
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
                this.list
                    .reset_with_uniform_height(this.buffer.visible_len(), px(LINE_HEIGHT));
                cx.notify();
            }
        })];
        let list = ListState::new(0, ListAlignment::Top, px(200.));
        list.set_follow_mode(FollowMode::Tail);
        LogsView {
            buffer: LogBuffer::new(),
            list,
            ended: false,
            filter,
            _task: task,
            _subs: subs,
        }
    }

    /// True while the list shows the end of the log. The list follows
    /// again when the user scrolls back to the end.
    fn following(&self) -> bool {
        self.list.is_following_tail() || self.list.is_scrolled_to_end() == Some(true)
    }

    fn jump_to_end(&mut self, cx: &mut Context<Self>) {
        self.list.set_follow_mode(FollowMode::Tail);
        cx.notify();
    }

    fn render_line(line: &LogLine, ix: usize, count: usize, cx: &App) -> AnyElement {
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
            .items_start()
            .px_4()
            .gap_3()
            .when(ix == 0, |d| d.pt(px(EDGE_PADDING)))
            .when(ix + 1 == count, |d| d.pb(px(EDGE_PADDING)))
            .child(
                div()
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .text_color(t.muted_foreground)
                    .child(time.to_string()),
            )
            .child(code_line(text).when(line.stream == LogStream::Stderr, |d| d.text_color(t.red)))
            .into_any_element()
    }
}

impl Render for LogsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let count = self.buffer.visible_len();
        let following = self.following();
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
                            .min_w(px(80.))
                            .child(Input::new(&self.filter).small().cleanable(true)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .whitespace_nowrap()
                            .text_size(px(text::SUBHEADLINE))
                            .font_features(tabular_nums())
                            .text_color(t.muted_foreground)
                            .child(status),
                    )
                    .when(self.ended, |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .whitespace_nowrap()
                                .text_size(px(text::SUBHEADLINE))
                                .text_color(t.muted_foreground)
                                .child("· ended"),
                        )
                    })
                    .child(
                        Button::new("follow")
                            .icon(Icon::new(IconName::ArrowDownToLine))
                            .xsmall()
                            .when(following, |b| b.primary())
                            .when(!following, |b| b.ghost())
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
                    .line_height(px(LINE_HEIGHT))
                    .child(
                        list(
                            self.list.clone(),
                            cx.processor(|this, ix: usize, _, cx| match this.buffer.visible_line(ix) {
                                Some(line) => {
                                    Self::render_line(line, ix, this.buffer.visible_len(), cx)
                                }
                                None => div().into_any_element(),
                            }),
                        )
                        .size_full(),
                    )
                    .child(Scrollbar::vertical(&self.list)),
            )
    }
}

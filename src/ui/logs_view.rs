//! A live, virtualized log view for one container. Long lines wrap. The
//! view keeps the colors of the program, marks warnings and errors, and
//! shows a heading for each day.

use std::ops::Range;
use std::time::Duration;

use bollard::Docker;
use chrono::{Datelike as _, Local, NaiveDate};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::ansi::Color;
use crate::docker::logs::{self, Level, LogStream, Paint};
use crate::runtime::{Backend, recv_batch};
use crate::state::log_buffer::{LogBuffer, MAX_LINES};
use crate::ui::{delayed_reveal, tabular_nums, text};

const LINE_HEIGHT: f32 = 18.;
/// The space above the first line and below the last line.
const EDGE_PADDING: f32 = 8.;
/// The width of the overlay scroll bar. The rows stop before it.
const SCROLLBAR_WIDTH: f32 = 16.;
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
                    if removed > 0 {
                        // The new first line gets the heading of the day.
                        this.list.remeasure_items(0..1);
                    }
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

    fn render_line(buffer: &LogBuffer, ix: usize, cx: &App) -> AnyElement {
        let Some(line) = buffer.visible_line(ix) else {
            return div().into_any_element();
        };
        let t = cx.theme();
        let count = buffer.visible_len();

        // A heading above the first line and above each change of day.
        let day = line.time.map(|time| time.date_naive());
        let prev_day = ix
            .checked_sub(1)
            .and_then(|i| buffer.visible_line(i))
            .and_then(|l| l.time)
            .map(|time| time.date_naive());
        let heading = day.filter(|d| ix == 0 || prev_day.is_some_and(|p| p != *d));

        // Very long lines are cut for display, so text layout stays fast.
        let (text, spans) = if line.text.len() > MAX_DISPLAY_CHARS {
            let mut end = MAX_DISPLAY_CHARS;
            while !line.text.is_char_boundary(end) {
                end -= 1;
            }
            let spans = line
                .spans
                .iter()
                .filter(|(r, _)| r.start < end)
                .map(|(r, p)| (r.start..r.end.min(end), *p))
                .collect();
            (SharedString::from(format!("{} …", &line.text[..end])), spans)
        } else {
            (SharedString::from(line.text.clone()), line.spans.clone())
        };
        let painted = spans.into_iter().map(|(r, p)| (r, paint_style(p, cx)));
        let matches = filter_ranges(&text, buffer.filter()).into_iter().map(|r| {
            let style = HighlightStyle {
                background_color: Some(t.yellow.opacity(0.35)),
                ..Default::default()
            };
            (r, style)
        });
        let highlights: Vec<_> = combine_highlights(painted, matches).collect();

        let tint = match line.level {
            Some(Level::Error) => Some(t.red.opacity(0.09)),
            Some(Level::Warn) => Some(t.yellow.opacity(0.10)),
            _ if line.stream == LogStream::Stderr => Some(t.red.opacity(0.04)),
            _ => None,
        };
        let quiet = matches!(line.level, Some(Level::Trace | Level::Debug));

        // A line with no time gets an invisible time, so all text starts at
        // the same column.
        let (clock, millis) = match line.time {
            Some(time) => (time.format("%H:%M:%S").to_string(), time.format("%.3f").to_string()),
            None => ("00:00:00".into(), ".000".into()),
        };
        let time = h_flex()
            .flex_shrink_0()
            .whitespace_nowrap()
            .font_features(tabular_nums())
            .when(line.time.is_none(), |d| d.invisible())
            .child(div().text_color(t.muted_foreground).child(clock))
            .child(div().text_color(t.muted_foreground.opacity(0.55)).child(millis));

        // The row is inset from the pane edges with round corners. The text
        // starts at the same column as the day heading. The right edge stops
        // before the scroll bar. The id keeps the hover state, so the view
        // draws again when the hover changes.
        let row = h_flex()
            .id(("log-line", ix))
            .items_start()
            .ml_2()
            .mr(px(SCROLLBAR_WIDTH))
            .px_2()
            .py(px(1.))
            .gap_3()
            .rounded(px(5.))
            .when_some(tint, |d, c| d.bg(c))
            .hover(|s| s.bg(t.list_hover))
            .child(time)
            .child(
                styled_line(text, highlights)
                    .when(quiet, |d| d.text_color(t.muted_foreground)),
            );

        v_flex()
            .w_full()
            .when(ix == 0 && heading.is_none(), |d| d.pt(px(EDGE_PADDING)))
            .when(ix + 1 == count, |d| d.pb(px(EDGE_PADDING)))
            .when_some(heading, |d, day| {
                d.child(
                    h_flex()
                        .px_4()
                        .pt(px(if ix == 0 { EDGE_PADDING } else { 12. }))
                        .pb(px(4.))
                        .gap_2()
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(t.font_family.clone())
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.muted_foreground)
                                .child(day_label(day, Local::now().date_naive())),
                        )
                        .child(div().flex_1().h(px(1.)).bg(t.border)),
                )
            })
            .child(row)
            .into_any_element()
    }
}

/// "Today", "Yesterday", or the date.
fn day_label(day: NaiveDate, today: NaiveDate) -> String {
    if day == today {
        "Today".into()
    } else if today.pred_opt() == Some(day) {
        "Yesterday".into()
    } else if day.year() == today.year() {
        day.format("%A, %-d %B").to_string()
    } else {
        day.format("%A, %-d %B %Y").to_string()
    }
}

fn level_color(level: Level, cx: &App) -> Hsla {
    let t = cx.theme();
    match level {
        Level::Trace => t.muted_foreground,
        Level::Debug => t.blue,
        Level::Info => t.green,
        Level::Warn => t.yellow,
        Level::Error => t.red,
    }
}

fn paint_style(paint: Paint, cx: &App) -> HighlightStyle {
    let t = cx.theme();
    match paint {
        Paint::Key => HighlightStyle {
            color: Some(t.muted_foreground),
            ..Default::default()
        },
        Paint::Level(level) => HighlightStyle {
            color: Some(level_color(level, cx)),
            font_weight: Some(FontWeight::SEMIBOLD),
            ..Default::default()
        },
        Paint::Ansi(sgr) => {
            let on_bg = sgr.bg.is_some();
            HighlightStyle {
                color: sgr.fg.map(|c| ansi_color(c, on_bg, cx)),
                background_color: sgr.bg.map(|c| ansi_color(c, true, cx)),
                font_weight: sgr.bold.then_some(FontWeight::BOLD),
                font_style: sgr.italic.then_some(FontStyle::Italic),
                underline: sgr.underline.then(|| UnderlineStyle {
                    thickness: px(1.),
                    color: None,
                    wavy: false,
                }),
                fade_out: sgr.dim.then_some(0.4),
                ..Default::default()
            }
        }
    }
}

/// Maps the terminal palette to the theme colors, so that the colors suit
/// the light and the dark appearance. Black and white text on the window
/// background becomes the muted and the normal text color.
fn ansi_color(color: Color, on_bg: bool, cx: &App) -> Hsla {
    let t = cx.theme();
    match color {
        Color::Index(0 | 8) if on_bg => black(),
        Color::Index(7 | 15) if on_bg => white(),
        Color::Index(0 | 8) => t.muted_foreground,
        Color::Index(7 | 15) => t.foreground,
        Color::Index(1 | 9) => t.red,
        Color::Index(2 | 10) => t.green,
        Color::Index(3 | 11) => t.yellow,
        Color::Index(4 | 12) => t.blue,
        Color::Index(5 | 13) => t.magenta,
        Color::Index(6 | 14) => t.cyan,
        Color::Index(n @ 16..=231) => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            rgb_color(level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        Color::Index(n) => {
            let v = 8 + (n - 232) * 10;
            rgb_color(v, v, v)
        }
        Color::Rgb(r, g, b) => rgb_color(r, g, b),
    }
}

fn rgb_color(r: u8, g: u8, b: u8) -> Hsla {
    rgb(u32::from_be_bytes([0, r, g, b])).into()
}

/// The ranges of `text` that match `filter`, with no case. `filter` is in
/// lower case.
fn filter_ranges(text: &str, filter: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if filter.is_empty() {
        return out;
    }
    let mut starts = text.char_indices().map(|(i, _)| i).peekable();
    while let Some(start) = starts.next() {
        let mut want = filter.chars().peekable();
        let mut end = start;
        for c in text[start..].chars() {
            if want.peek().is_none() {
                break;
            }
            if !c.to_lowercase().all(|l| want.next() == Some(l)) {
                break;
            }
            end += c.len_utf8();
        }
        if want.peek().is_none() && end > start {
            out.push(start..end);
            // The next match starts after this one.
            while starts.peek().is_some_and(|i| *i < end) {
                starts.next();
            }
        }
    }
    out
}

/// A line of monospace text with highlights, which wraps in the space it
/// gets. The wrapped rows start at the indent of the first row, as in
/// `code_line`.
fn styled_line(text: SharedString, highlights: Vec<(Range<usize>, HighlightStyle)>) -> Div {
    let body = text.trim_start_matches([' ', '\t']);
    let indent = text.len() - body.len();
    let row = div().flex().flex_1().min_w_0().items_start();
    if indent == 0 {
        return row.child(div().flex_1().min_w_0().child(StyledText::new(text).with_highlights(highlights)));
    }
    let shifted: Vec<_> = highlights
        .into_iter()
        .filter(|(r, _)| r.end > indent)
        .map(|(r, s)| (r.start.max(indent) - indent..r.end - indent, s))
        .collect();
    row.child(
        div()
            .flex_shrink_0()
            .whitespace_nowrap()
            .child(SharedString::from(text[..indent].to_string())),
    )
    .child(
        div()
            .flex_1()
            .min_w_0()
            .child(StyledText::new(body.to_string()).with_highlights(shifted)),
    )
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
                            cx.processor(|this, ix: usize, _, cx| {
                                Self::render_line(&this.buffer, ix, cx)
                            }),
                        )
                        .size_full(),
                    )
                    .child(Scrollbar::vertical(&self.list))
                    .when(count == 0, |d| {
                        let message = if self.buffer.is_filtered() {
                            "No lines match the filter"
                        } else if self.ended {
                            "The container wrote no logs"
                        } else {
                            "Waiting for logs…"
                        };
                        d.child(
                            delayed_reveal(
                                "logs-empty",
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .font_family(t.font_family.clone())
                                    .text_size(px(text::BODY))
                                    .text_color(t.muted_foreground)
                                    .child(message),
                            ),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::filter_ranges;

    #[test]
    fn filter_ranges_ignore_case() {
        assert_eq!(filter_ranges("Error and ERROR", "error"), [0..5, 10..15]);
        assert_eq!(filter_ranges("aaaa", "aa"), [0..2, 2..4]);
        assert_eq!(filter_ranges("Über über", "über"), [0..5, 6..11]);
        assert!(filter_ranges("abc", "").is_empty());
    }
}

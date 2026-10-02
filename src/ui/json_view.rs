//! The Inspect tab of a container, an image, or a volume: the inspect data
//! as JSON, with colors, line numbers, and indent guides.

use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::ui::{native, tabular_nums, text, toast};

pub const LINE_HEIGHT: f32 = 18.;
/// The space above the first line and below the last line.
const EDGE_PADDING: f32 = 8.;
/// The width of the overlay scroll bar. The rows stop before it.
const SCROLLBAR_WIDTH: f32 = 16.;
/// The spaces for each level in the output of `serde_json::to_string_pretty`.
const INDENT: usize = 2;
/// The distance from the start of an indent level to its guide. The guide
/// then sits below the middle of the bracket on the line above.
const GUIDE_OFFSET: f32 = 3.;

/// The kind of a range of JSON text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Token {
    Key,
    String,
    Number,
    Bool,
    Null,
    Punct,
}

pub struct JsonLine {
    /// The indent level.
    pub depth: usize,
    /// The text after the indent.
    pub body: SharedString,
    /// The ranges of `body` with their kinds.
    pub spans: Vec<(Range<usize>, Token)>,
}

/// Pretty JSON text, cut into lines with tokens.
#[derive(Default)]
pub struct Json {
    pub text: String,
    pub lines: Vec<JsonLine>,
}

impl Json {
    pub fn new(text: String) -> Self {
        let lines = text
            .lines()
            .map(|line| {
                let indent = line.len() - line.trim_start_matches(' ').len();
                let depth = indent / INDENT;
                let body = &line[depth * INDENT..];
                JsonLine { depth, body: SharedString::from(body.to_string()), spans: tokens(body) }
            })
            .collect();
        Self { text, lines }
    }
}

/// Finds the tokens in one line of pretty JSON. A string that a colon
/// follows is a key.
fn tokens(body: &str) -> Vec<(Range<usize>, Token)> {
    let bytes = body.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let token = match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(bytes.len());
                if body[i..].trim_start().starts_with(':') { Token::Key } else { Token::String }
            }
            b'-' | b'0'..=b'9' => {
                while i < bytes.len() && matches!(bytes[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    i += 1;
                }
                Token::Number
            }
            b'{' | b'}' | b'[' | b']' | b',' | b':' => {
                i += 1;
                Token::Punct
            }
            b't' if body[i..].starts_with("true") => {
                i += 4;
                Token::Bool
            }
            b'f' if body[i..].starts_with("false") => {
                i += 5;
                Token::Bool
            }
            b'n' if body[i..].starts_with("null") => {
                i += 4;
                Token::Null
            }
            _ => {
                i += 1;
                continue;
            }
        };
        spans.push((start..i, token));
    }
    spans
}

fn token_style(token: Token, cx: &App) -> HighlightStyle {
    let t = cx.theme();
    match token {
        Token::Key => HighlightStyle {
            font_weight: Some(FontWeight::MEDIUM),
            ..Default::default()
        },
        Token::String => HighlightStyle {
            color: Some(t.green),
            ..Default::default()
        },
        Token::Number => HighlightStyle {
            color: Some(t.blue),
            ..Default::default()
        },
        Token::Bool => HighlightStyle {
            color: Some(t.magenta),
            ..Default::default()
        },
        Token::Null | Token::Punct => HighlightStyle {
            color: Some(t.muted_foreground),
            ..Default::default()
        },
    }
}

/// A row: the line number, one guide for each indent level, and the text.
/// Long lines wrap, and the wrapped rows start at the indent.
fn render_line(json: &Json, ix: usize, cx: &App) -> AnyElement {
    let t = cx.theme();
    let Some(line) = json.lines.get(ix) else {
        return div().into_any_element();
    };
    let count = json.lines.len();
    let width = count.to_string().len();
    let guides = (0..line.depth).map(|_| {
        div()
            .relative()
            .flex_shrink_0()
            .whitespace_nowrap()
            .child(" ".repeat(INDENT))
            .child(div().absolute().top_0().bottom_0().left(px(GUIDE_OFFSET)).w(px(1.)).bg(t.border))
    });
    let highlights: Vec<_> = line.spans.iter().map(|(r, token)| (r.clone(), token_style(*token, cx))).collect();
    let row = h_flex()
        .id(("json-line", ix))
        .items_start()
        .ml_2()
        .mr(px(SCROLLBAR_WIDTH))
        .px_2()
        .gap_3()
        .rounded(px(5.))
        .hover(|s| s.bg(native::hover(cx)))
        .child(
            div()
                .flex_shrink_0()
                .whitespace_nowrap()
                .font_features(tabular_nums())
                .text_color(t.muted_foreground.opacity(0.6))
                .child(format!("{:>width$}", ix + 1)),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .children(guides)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(StyledText::new(line.body.clone()).with_highlights(highlights)),
                ),
        );
    div()
        .w_full()
        .when(ix == 0, |d| d.pt(px(EDGE_PADDING)))
        .when(ix + 1 == count, |d| d.pb(px(EDGE_PADDING)))
        .child(row)
        .into_any_element()
}

/// The Inspect tab body: a bar with the line count and a copy button, and
/// the JSON lines in `list`.
pub fn render(json: &Rc<Json>, list_state: &ListState, cx: &App) -> AnyElement {
    let t = cx.theme();
    let count = json.lines.len();
    let copy = json.clone();
    let rows = json.clone();
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
                        .whitespace_nowrap()
                        .text_size(px(text::SUBHEADLINE))
                        .font_features(tabular_nums())
                        .text_color(t.muted_foreground)
                        .child(format!("{count} lines")),
                )
                .child(
                    Button::new("copy-json")
                        .icon(Icon::new(IconName::Copy))
                        .ghost()
                        .xsmall()
                        .tooltip("Copy the JSON")
                        .on_click(move |_, _, cx| {
                            toast::copy(copy.text.clone(), format!("{} lines of JSON", copy.lines.len()), cx)
                        }),
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
                .child(list(list_state.clone(), move |ix, _, cx| render_line(&rows, ix, cx)).size_full())
                .child(Scrollbar::vertical(list_state)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Json, Token, tokens};

    fn kinds(body: &str) -> Vec<(&str, Token)> {
        tokens(body).into_iter().map(|(r, k)| (&body[r], k)).collect()
    }

    #[test]
    fn lines_get_tokens() {
        assert_eq!(
            kinds(r#""Name": "a \"b\" c","#),
            [("\"Name\"", Token::Key), (":", Token::Punct), (r#""a \"b\" c""#, Token::String), (",", Token::Punct)]
        );
        assert_eq!(
            kinds(r#""Size": -1.5e3, "On": true, "X": null"#),
            [
                ("\"Size\"", Token::Key),
                (":", Token::Punct),
                ("-1.5e3", Token::Number),
                (",", Token::Punct),
                ("\"On\"", Token::Key),
                (":", Token::Punct),
                ("true", Token::Bool),
                (",", Token::Punct),
                ("\"X\"", Token::Key),
                (":", Token::Punct),
                ("null", Token::Null),
            ]
        );
        assert_eq!(kinds(r#""ünï""#), [(r#""ünï""#, Token::String)]);
        assert_eq!(kinds(r#""open \"#), [(r#""open \"#, Token::String)]);
    }

    #[test]
    fn indent_becomes_depth() {
        let json = Json::new("{\n  \"A\": [\n    1\n  ]\n}".into());
        let depths: Vec<_> = json.lines.iter().map(|l| (l.depth, l.body.as_ref())).collect();
        assert_eq!(depths, [(0, "{"), (1, "\"A\": ["), (2, "1"), (1, "]"), (0, "}")]);
    }
}

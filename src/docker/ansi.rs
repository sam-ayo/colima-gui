//! ANSI escape codes in log text. The parser keeps the SGR styles (colors,
//! bold, and so on) as spans and removes all other escape sequences.

use std::ops::Range;

/// A color of the terminal palette, or a 24-bit color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    /// 0..=7 are the normal colors, 8..=15 the bright colors, and
    /// 16..=255 the 256-color cube and grays.
    Index(u8),
    Rgb(u8, u8, u8),
}

/// The text style that SGR ("Select Graphic Rendition") codes set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sgr {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Sgr {
    fn is_plain(&self) -> bool {
        *self == Sgr::default()
    }

    fn apply(&mut self, params: &[u16]) {
        if params.is_empty() {
            *self = Sgr::default();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => *self = Sgr::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                21 | 22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                n @ 30..=37 => self.fg = Some(Color::Index((n - 30) as u8)),
                n @ 90..=97 => self.fg = Some(Color::Index((n - 90 + 8) as u8)),
                39 => self.fg = None,
                n @ 40..=47 => self.bg = Some(Color::Index((n - 40) as u8)),
                n @ 100..=107 => self.bg = Some(Color::Index((n - 100 + 8) as u8)),
                49 => self.bg = None,
                n @ (38 | 48) => {
                    let (color, used) = extended_color(&params[i + 1..]);
                    if n == 38 {
                        self.fg = color;
                    } else {
                        self.bg = color;
                    }
                    i += used;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// Reads the arguments of 38 or 48: `5;n` or `2;r;g;b`. Returns the color
/// and the number of parameters that it used.
fn extended_color(p: &[u16]) -> (Option<Color>, usize) {
    let byte = |i: usize| p.get(i).map(|v| (*v).min(255) as u8);
    match p.first() {
        Some(5) => (byte(1).map(Color::Index), 2),
        Some(2) => match (byte(1), byte(2), byte(3)) {
            (Some(r), Some(g), Some(b)) => (Some(Color::Rgb(r, g, b)), 4),
            _ => (None, p.len()),
        },
        _ => (None, 0),
    }
}

/// Removes the escape sequences (CSI, OSC, and 2-byte forms) and `\r` from
/// `s`. Returns the plain text and the styled ranges of it, in order.
pub fn parse(s: &str) -> (String, Vec<(Range<usize>, Sgr)>) {
    let mut spans: Vec<(Range<usize>, Sgr)> = Vec::new();
    if !s.contains(['\x1b', '\r']) {
        return (s.to_string(), spans);
    }
    let mut out = String::with_capacity(s.len());
    let mut style = Sgr::default();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {}
            '\x1b' => match chars.next() {
                // CSI: parameters, then one final byte in 0x40..=0x7E.
                Some('[') => {
                    let mut params = Vec::new();
                    let mut current: Option<u16> = None;
                    let mut fin = None;
                    for c in chars.by_ref() {
                        match c {
                            '0'..='9' => {
                                let d = c as u16 - '0' as u16;
                                current = Some(current.unwrap_or(0).saturating_mul(10).saturating_add(d));
                            }
                            ';' | ':' => params.push(current.take().unwrap_or(0)),
                            '\x40'..='\x7e' => {
                                fin = Some(c);
                                break;
                            }
                            _ => {}
                        }
                    }
                    if fin == Some('m') {
                        if let Some(v) = current {
                            params.push(v);
                        }
                        style.apply(&params);
                    }
                }
                // OSC: ends with BEL or ESC '\'.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            c => {
                let start = out.len();
                out.push(c);
                if style.is_plain() {
                    continue;
                }
                match spans.last_mut() {
                    Some((range, s)) if *s == style && range.end == start => range.end = out.len(),
                    _ => spans.push((start..out.len(), style)),
                }
            }
        }
    }
    (out, spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_escape_codes() {
        assert_eq!(parse("\x1b]0;title\x07done").0, "done");
        assert_eq!(parse("plain").0, "plain");
        assert_eq!(parse("crlf\r").0, "crlf");
        assert_eq!(parse("\x1bc\x1b[2Kclear").0, "clear");
    }

    #[test]
    fn keeps_colors_as_spans() {
        let (text, spans) = parse("\x1b[32mINFO\x1b[39m: \x1b[1;36mup\x1b[0m");
        assert_eq!(text, "INFO: up");
        let green = Sgr { fg: Some(Color::Index(2)), ..Default::default() };
        let cyan = Sgr { fg: Some(Color::Index(6)), bold: true, ..Default::default() };
        assert_eq!(spans, [(0..4, green), (6..8, cyan)]);
    }

    #[test]
    fn reads_extended_colors() {
        let (_, spans) = parse("\x1b[38;5;208ma\x1b[38;2;1;2;3;48;5;1mb\x1b[m");
        assert_eq!(spans[0].1.fg, Some(Color::Index(208)));
        assert_eq!(spans[1].1.fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(spans[1].1.bg, Some(Color::Index(1)));
    }
}

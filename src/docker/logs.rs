//! Log stream for one container.

use std::ops::Range;

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::query_parameters::LogsOptions;
use chrono::{DateTime, Local};
use futures::{Stream, StreamExt};

use crate::docker::ansi::{self, Sgr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// The severity of a log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn from_name(name: &str) -> Option<Level> {
        if name.len() > 11 {
            return None;
        }
        Some(match name.to_ascii_lowercase().as_str() {
            "trace" | "trc" | "verbose" => Level::Trace,
            "debug" | "dbg" => Level::Debug,
            "info" | "inf" | "information" | "notice" => Level::Info,
            "warn" | "warning" | "wrn" => Level::Warn,
            "error" | "err" | "fatal" | "crit" | "critical" | "panic" | "alert" | "emerg"
            | "severe" => Level::Error,
            _ => return None,
        })
    }

    /// The numeric levels of pino and bunyan.
    fn from_number(n: u32) -> Option<Level> {
        Some(match n {
            10 => Level::Trace,
            20 => Level::Debug,
            30 => Level::Info,
            40 => Level::Warn,
            50 | 60 => Level::Error,
            _ => return None,
        })
    }
}

/// How the view paints a range of the text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    /// A style from ANSI escape codes.
    Ansi(Sgr),
    /// A key of a JSON object or of `key=value` pairs.
    Key,
    /// The word or value that gives the level of the line.
    Level(Level),
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogLine {
    pub stream: LogStream,
    /// The time that the engine recorded for the line, in local time.
    pub time: Option<DateTime<Local>>,
    pub text: String,
    pub level: Option<Level>,
    /// The painted ranges of `text`, in order and with no overlap.
    pub spans: Vec<(Range<usize>, Paint)>,
}

pub const TAIL_LINES: usize = 1000;

/// Follows the log of a container. It starts with the last `TAIL_LINES` lines.
pub fn follow(docker: &Docker, id: &str) -> impl Stream<Item = LogLine> + Send + 'static {
    let opts = LogsOptions {
        follow: true,
        stdout: true,
        stderr: true,
        timestamps: true,
        tail: TAIL_LINES.to_string(),
        ..Default::default()
    };
    let mut splitter = LineSplitter::default();
    docker
        .logs(id, Some(opts))
        .take_while(|r| futures::future::ready(r.is_ok()))
        .flat_map(move |r| {
            let lines = match r {
                Ok(LogOutput::StdErr { message }) => splitter.push(LogStream::Stderr, &message),
                Ok(LogOutput::StdOut { message }) | Ok(LogOutput::Console { message }) => {
                    splitter.push(LogStream::Stdout, &message)
                }
                _ => Vec::new(),
            };
            futures::stream::iter(lines)
        })
}

/// Splits frames into lines. A TTY container sends frames that can end in the
/// middle of a line, so the rest of the line waits for the next frame.
#[derive(Default)]
pub struct LineSplitter {
    partial: [Vec<u8>; 2],
    /// The level of the last line of each stream.
    last_level: [Option<Level>; 2],
}

impl LineSplitter {
    pub fn push(&mut self, stream: LogStream, bytes: &[u8]) -> Vec<LogLine> {
        let i = stream as usize;
        let buf = &mut self.partial[i];
        buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = buf.drain(..=pos).collect();
            let mut line = parse_line(stream, &raw[..raw.len() - 1]);
            // The lines of a stack trace or of a pretty-printed object
            // get the level of the line that they continue.
            if line.level.is_none() && is_continuation(&line.text) {
                line.level = self.last_level[i];
            }
            self.last_level[i] = line.level;
            out.push(line);
        }
        out
    }
}

fn is_continuation(text: &str) -> bool {
    text.starts_with([' ', '\t', '}', ']', ')']) || text.starts_with("Caused by")
}

/// Parses `<timestamp> <text>`, removes the escape codes, and finds the level
/// and the parts to paint.
pub fn parse_line(stream: LogStream, raw: &[u8]) -> LogLine {
    let s = String::from_utf8_lossy(raw);
    let (time, rest) = match s.split_once(' ') {
        Some((ts, rest)) if looks_like_timestamp(ts) => (
            DateTime::parse_from_rfc3339(ts).ok().map(|t| t.with_timezone(&Local)),
            rest,
        ),
        _ => (None, s.as_ref()),
    };
    let (text, ansi) = ansi::parse(rest);
    let found = detect_level(&text);
    let mut spans: Vec<(Range<usize>, Paint)> = Vec::new();
    if ansi.is_empty() {
        // The colors of the program have priority. Paint only plain text.
        spans.extend(key_ranges(&text).into_iter().map(|r| (r, Paint::Key)));
        if let Some((level, Some(range))) = found.clone() {
            spans.retain(|(r, _)| r.end <= range.start || r.start >= range.end);
            spans.push((range, Paint::Level(level)));
            spans.sort_by_key(|(r, _)| r.start);
        }
    } else {
        spans.extend(ansi.into_iter().map(|(r, sgr)| (r, Paint::Ansi(sgr))));
    }
    LogLine {
        stream,
        time,
        text,
        level: found.map(|(level, _)| level),
        spans,
    }
}

fn looks_like_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 20 && b[4] == b'-' && b[7] == b'-' && b[10] == b'T'
}

/// The start of `s`, at most `max` bytes, cut at a character boundary.
fn head(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Finds the level of a line and the range of the text that tells it.
fn detect_level(text: &str) -> Option<(Level, Option<Range<usize>>)> {
    let h = head(text, 2048);
    structured_level(h)
        .or_else(|| klog_level(h))
        .or_else(|| keyword_level(head(h, 160)))
        .or_else(|| http_status_level(h))
}

/// A level in a JSON object (`"level":"warn"`, `"level":40`, and the
/// `"s":"W"` of MongoDB) or in `key=value` pairs (`level=warn`).
fn structured_level(h: &str) -> Option<(Level, Option<Range<usize>>)> {
    for key in ["level", "severity", "lvl", "loglevel"] {
        let range = json_value(h, key).or_else(|| logfmt_value(h, key));
        if let Some(range) = range {
            let v = &h[range.clone()];
            let level = match v.parse::<u32>() {
                Ok(n) => Level::from_number(n),
                Err(_) => Level::from_name(v),
            };
            if let Some(level) = level {
                return Some((level, Some(range)));
            }
        }
    }
    let range = json_value(h, "s")?;
    let level = match h[range.clone()].as_bytes() {
        [b'F'] | [b'E'] => Level::Error,
        [b'W'] => Level::Warn,
        [b'I'] => Level::Info,
        [b'D', ..] => Level::Debug,
        _ => return None,
    };
    Some((level, Some(range)))
}

/// The range of the value of `"key": value`, without the quotes.
fn json_value(h: &str, key: &str) -> Option<Range<usize>> {
    let needle = format!("\"{key}\"");
    for (i, _) in h.match_indices(&needle) {
        let after = &h[i + needle.len()..];
        let Some(after) = after.trim_start().strip_prefix(':') else {
            continue;
        };
        let value = after.trim_start();
        let start = h.len() - value.len();
        if let Some(inner) = value.strip_prefix('"') {
            let len = inner.find('"')?;
            return Some(start + 1..start + 1 + len);
        }
        let len = value.bytes().take_while(u8::is_ascii_digit).count();
        if len > 0 {
            return Some(start..start + len);
        }
    }
    None
}

/// The range of the value of `key=value` or `key="value"`.
fn logfmt_value(h: &str, key: &str) -> Option<Range<usize>> {
    let needle = format!("{key}=");
    for (i, _) in h.match_indices(&needle) {
        if i > 0 && h.as_bytes()[i - 1] != b' ' {
            continue;
        }
        let start = i + needle.len();
        let value = &h[start..];
        if let Some(inner) = value.strip_prefix('"') {
            let len = inner.find('"')?;
            return Some(start + 1..start + 1 + len);
        }
        let len = value.find(' ').unwrap_or(value.len());
        if len > 0 {
            return Some(start..start + len);
        }
    }
    None
}

/// The prefix of klog and glog: `I0102 15:04:05.000000 ...`.
fn klog_level(h: &str) -> Option<(Level, Option<Range<usize>>)> {
    let b = h.as_bytes();
    if b.len() < 6 || !b[1..5].iter().all(u8::is_ascii_digit) || b[5] != b' ' {
        return None;
    }
    let level = match b[0] {
        b'I' => Level::Info,
        b'W' => Level::Warn,
        b'E' | b'F' => Level::Error,
        _ => return None,
    };
    Some((level, Some(0..1)))
}

/// A level word near the start of the line. An upper-case word (`WARN`)
/// is sufficient. Other words must be in brackets (`[warn]`) or come before
/// a colon (`warning:`), because they are also normal words.
fn keyword_level(h: &str) -> Option<(Level, Option<Range<usize>>)> {
    let b = h.as_bytes();
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < b.len() {
        if !is_word(b[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && is_word(b[i]) {
            i += 1;
        }
        let word = &h[start..i];
        let Some(level) = Level::from_name(word) else {
            continue;
        };
        let upper = word.bytes().all(|c| c.is_ascii_uppercase());
        let before = start.checked_sub(1).map(|j| b[j]);
        let after = b.get(i).copied();
        let bracketed = matches!(
            (before, after),
            (Some(b'['), Some(b']')) | (Some(b'<'), Some(b'>')) | (Some(b'('), Some(b')'))
        );
        if upper || bracketed || after == Some(b':') {
            return Some((level, Some(start..i)));
        }
    }
    None
}

/// The status code of an access log line: `"GET / HTTP/1.1" 404 ...`.
fn http_status_level(h: &str) -> Option<(Level, Option<Range<usize>>)> {
    let i = h.find(" HTTP/")?;
    let quote = i + h[i..].find("\" ")?;
    let start = quote + 2;
    let code = h.get(start..start + 3)?;
    if !code.bytes().all(|c| c.is_ascii_digit())
        || h.as_bytes().get(start + 3).is_some_and(|c| *c != b' ')
    {
        return None;
    }
    let level = match code.as_bytes()[0] {
        b'5' => Level::Error,
        b'4' => Level::Warn,
        b'1'..=b'3' => Level::Info,
        _ => return None,
    };
    Some((level, Some(start..start + 3)))
}

/// The keys of a JSON object, with their quotes, or of a line of
/// `key=value` pairs.
fn key_ranges(text: &str) -> Vec<Range<usize>> {
    let h = head(text, 8192);
    if h.trim_start().starts_with('{') {
        json_keys(h)
    } else {
        logfmt_keys(h)
    }
}

fn json_keys(h: &str) -> Vec<Range<usize>> {
    let b = h.as_bytes();
    let mut keys = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < b.len() && b[i] != b'"' {
            i += if b[i] == b'\\' { 2 } else { 1 };
        }
        if i >= b.len() {
            break;
        }
        i += 1;
        let rest = &h[i..];
        if rest.trim_start().starts_with(':') {
            keys.push(start..i);
        }
    }
    keys
}

fn logfmt_keys(h: &str) -> Vec<Range<usize>> {
    let b = h.as_bytes();
    let mut keys = Vec::new();
    let mut i = 0;
    let mut in_quote = false;
    while i < b.len() {
        if b[i] == b'"' {
            in_quote = !in_quote;
        }
        let at_start = i == 0 || b[i - 1] == b' ';
        if in_quote || !at_start || !b[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'.' | b'-')) {
            i += 1;
        }
        if b.get(i) == Some(&b'=') {
            keys.push(start..i);
        }
    }
    // One `a=b` can be part of a normal sentence or a URL.
    if keys.len() < 2 {
        keys.clear();
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(text: &str) -> Option<Level> {
        parse_line(LogStream::Stdout, text.as_bytes()).level
    }

    #[test]
    fn parses_timestamp_prefix() {
        let l = parse_line(LogStream::Stdout, b"2026-09-30T12:00:00.123456789Z hello world");
        let utc = l.time.unwrap().with_timezone(&chrono::Utc);
        assert_eq!(utc.to_rfc3339(), "2026-09-30T12:00:00.123456789+00:00");
        assert_eq!(l.text, "hello world");
        let l = parse_line(LogStream::Stderr, b"no timestamp here");
        assert_eq!(l.time, None);
        assert_eq!(l.text, "no timestamp here");
    }

    #[test]
    fn splitter_keeps_partial_lines() {
        let mut s = LineSplitter::default();
        assert!(s.push(LogStream::Stdout, b"2026-09-30T12:00:00Z par").is_empty());
        let lines = s.push(LogStream::Stdout, b"tial\n2026-09-30T12:00:01Z b\n");
        let texts: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["partial", "b"]);
    }

    #[test]
    fn keeps_ansi_colors() {
        let l = parse_line(LogStream::Stdout, b"\x1b[32mINFO\x1b[39m: up");
        assert_eq!(l.text, "INFO: up");
        assert_eq!(l.level, Some(Level::Info));
        assert!(matches!(l.spans[..], [(ref r, Paint::Ansi(_))] if *r == (0..4)));
    }

    #[test]
    fn finds_levels() {
        assert_eq!(level(r#"{"level":"warn","msg":"x"}"#), Some(Level::Warn));
        assert_eq!(level(r#"{"level":50,"msg":"x"}"#), Some(Level::Error));
        assert_eq!(level(r#"{"t":{},"s":"E",  "c":"NETWORK"}"#), Some(Level::Error));
        assert_eq!(level(r#"time=1 level=debug msg="a b""#), Some(Level::Debug));
        assert_eq!(level("2026/10/02 [error] 7#7: open() failed"), Some(Level::Error));
        assert_eq!(level("WARNING:root:disk is full"), Some(Level::Warn));
        assert_eq!(level("E1002 15:04:05.000000 1 main.go:1] boom"), Some(Level::Error));
        assert_eq!(level(r#"1.2.3.4 - - [x] "GET / HTTP/1.1" 503 12"#), Some(Level::Error));
        assert_eq!(level(r#"1.2.3.4 - - [x] "GET /a HTTP/1.1" 206 1"#), Some(Level::Info));
        assert_eq!(level("an error happened in the info desk"), None);
        assert_eq!(level("Server listening at http://127.0.0.1:3001"), None);
    }

    #[test]
    fn continuation_lines_get_the_level() {
        let mut s = LineSplitter::default();
        let lines = s.push(LogStream::Stderr, b"ERROR boom\n    at main (a.js:1)\nnext\n");
        let levels: Vec<_> = lines.iter().map(|l| l.level).collect();
        assert_eq!(levels, [Some(Level::Error), Some(Level::Error), None]);
    }

    #[test]
    fn paints_keys_and_level() {
        let l = parse_line(LogStream::Stdout, br#"{"msg":"hi","level":"info"}"#);
        let painted: Vec<_> = l.spans.iter().map(|(r, p)| (&l.text[r.clone()], *p)).collect();
        assert_eq!(
            painted,
            [
                ("\"msg\"", Paint::Key),
                ("\"level\"", Paint::Key),
                ("info", Paint::Level(Level::Info)),
            ]
        );
        let l = parse_line(LogStream::Stdout, b"see https://x.io/?a=b now");
        assert!(l.spans.is_empty());
    }
}

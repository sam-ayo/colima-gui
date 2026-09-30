//! Log stream for one container.

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::query_parameters::LogsOptions;
use futures::{Stream, StreamExt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogLine {
    pub stream: LogStream,
    /// The timestamp as the engine sends it (RFC 3339), or empty.
    pub timestamp: String,
    pub text: String,
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
}

impl LineSplitter {
    pub fn push(&mut self, stream: LogStream, bytes: &[u8]) -> Vec<LogLine> {
        let buf = &mut self.partial[stream as usize];
        buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            out.push(parse_line(stream, &line[..line.len() - 1]));
        }
        out
    }
}

/// Parses `<timestamp> <text>` and removes ANSI escape codes.
pub fn parse_line(stream: LogStream, raw: &[u8]) -> LogLine {
    let s = String::from_utf8_lossy(raw);
    let (timestamp, text) = match s.split_once(' ') {
        Some((ts, rest)) if looks_like_timestamp(ts) => (ts.to_string(), rest),
        _ => (String::new(), s.as_ref()),
    };
    LogLine {
        stream,
        timestamp,
        text: strip_ansi(text),
    }
}

fn looks_like_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 20 && b[4] == b'-' && b[7] == b'-' && b[10] == b'T'
}

/// Removes ANSI escape sequences (CSI, OSC, and 2-byte forms) and `\r`.
pub fn strip_ansi(s: &str) -> String {
    if !s.contains(['\x1b', '\r']) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {}
            '\x1b' => match chars.next() {
                // CSI: parameters, then one final byte in 0x40..=0x7E.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
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
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_color_codes() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m text"), "red text");
        assert_eq!(strip_ansi("\x1b]0;title\x07done"), "done");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("crlf\r"), "crlf");
    }

    #[test]
    fn parses_timestamp_prefix() {
        let l = parse_line(LogStream::Stdout, b"2026-09-30T12:00:00.123456789Z hello world");
        assert_eq!(l.timestamp, "2026-09-30T12:00:00.123456789Z");
        assert_eq!(l.text, "hello world");
        let l = parse_line(LogStream::Stderr, b"no timestamp here");
        assert_eq!(l.timestamp, "");
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
}

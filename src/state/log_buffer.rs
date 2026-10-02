//! A ring buffer of log lines with an optional text filter.

use std::collections::VecDeque;

use crate::docker::logs::LogLine;

pub const MAX_LINES: usize = 50_000;

/// Each line has a sequence number. The number of the line at index `i` is
/// `first_seq + i`. The filter keeps sequence numbers, so eviction at the
/// front does not change the numbers of the other lines.
pub struct LogBuffer {
    lines: VecDeque<LogLine>,
    first_seq: u64,
    capacity: usize,
    filter: String,
    /// Sequence numbers of the lines that match `filter`, in order.
    matches: VecDeque<u64>,
    /// The number of lines that the buffer removed because it was full.
    pub evicted: u64,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self::with_capacity(MAX_LINES)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        LogBuffer {
            lines: VecDeque::with_capacity(capacity.min(4096)),
            first_seq: 0,
            capacity,
            filter: String::new(),
            matches: VecDeque::new(),
            evicted: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// The filter text, in lower case.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn is_filtered(&self) -> bool {
        !self.filter.is_empty()
    }

    /// The number of lines that the view shows.
    pub fn visible_len(&self) -> usize {
        if self.is_filtered() {
            self.matches.len()
        } else {
            self.lines.len()
        }
    }

    /// The line at a position of the view.
    pub fn visible_line(&self, ix: usize) -> Option<&LogLine> {
        if self.is_filtered() {
            let seq = *self.matches.get(ix)?;
            self.lines.get((seq - self.first_seq) as usize)
        } else {
            self.lines.get(ix)
        }
    }

    fn line_matches(filter: &str, line: &LogLine) -> bool {
        // `filter` is already lower case. An ASCII filter does not need a
        // lower-case copy of each line, so a filter of many lines stays fast.
        if filter.is_ascii() {
            contains_ignore_ascii_case(line.text.as_bytes(), filter.as_bytes())
        } else {
            line.text.to_lowercase().contains(filter)
        }
    }

    /// Adds lines at the end. Returns the number of visible lines that the
    /// buffer removed from the front to make space.
    pub fn extend(&mut self, batch: impl IntoIterator<Item = LogLine>) -> usize {
        let mut removed = 0;
        for line in batch {
            if self.lines.len() == self.capacity {
                self.lines.pop_front();
                self.first_seq += 1;
                self.evicted += 1;
                if !self.is_filtered() {
                    removed += 1;
                }
                while self.matches.front().is_some_and(|s| *s < self.first_seq) {
                    self.matches.pop_front();
                    removed += 1;
                }
            }
            let seq = self.first_seq + self.lines.len() as u64;
            if self.is_filtered() && Self::line_matches(&self.filter, &line) {
                self.matches.push_back(seq);
            }
            self.lines.push_back(line);
        }
        removed
    }

    pub fn set_filter(&mut self, filter: &str) {
        let filter = filter.trim().to_lowercase();
        if filter == self.filter {
            return;
        }
        self.filter = filter;
        self.matches.clear();
        if self.filter.is_empty() {
            return;
        }
        let first = self.first_seq;
        let f = &self.filter;
        self.matches.extend(
            self.lines
                .iter()
                .enumerate()
                .filter(|(_, l)| Self::line_matches(f, l))
                .map(|(i, _)| first + i as u64),
        );
    }
}

/// True if `haystack` contains `needle`, with no case for the ASCII letters.
/// `needle` must be lower case.
fn contains_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|w| w.iter().zip(needle).all(|(a, b)| a.to_ascii_lowercase() == *b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::logs::LogStream;

    fn line(text: &str) -> LogLine {
        LogLine {
            stream: LogStream::Stdout,
            time: None,
            text: text.into(),
            level: None,
            spans: Vec::new(),
        }
    }

    #[test]
    fn stops_at_capacity_and_evicts_oldest() {
        let mut b = LogBuffer::new();
        let removed = b.extend((1..=200_000).map(|i| line(&i.to_string())));
        assert_eq!(removed, 150_000);
        assert_eq!(b.len(), MAX_LINES);
        assert_eq!(b.evicted, 150_000);
        assert_eq!(b.visible_line(0).unwrap().text, "150001");
        assert_eq!(b.visible_line(MAX_LINES - 1).unwrap().text, "200000");
        assert!(b.visible_line(MAX_LINES).is_none());
    }

    #[test]
    fn filter_follows_eviction() {
        let mut b = LogBuffer::with_capacity(4);
        b.extend(["a err", "b", "c err", "d"].map(line));
        b.set_filter("ERR");
        assert_eq!(b.visible_len(), 2);
        assert_eq!(b.visible_line(1).unwrap().text, "c err");
        // Two new lines remove "a err" and "b" from the front. Only "a err"
        // was visible.
        assert_eq!(b.extend(["e err", "f"].map(line)), 1);
        assert_eq!(b.len(), 4);
        let texts: Vec<_> = (0..b.visible_len())
            .map(|i| b.visible_line(i).unwrap().text.clone())
            .collect();
        assert_eq!(texts, ["c err", "e err"]);
        b.set_filter("");
        assert_eq!(b.visible_len(), 4);
        assert_eq!(b.visible_line(0).unwrap().text, "c err");
    }

    #[test]
    fn filter_ignores_case() {
        let mut b = LogBuffer::new();
        b.extend(["An ERROR here", "fine", "Über Fehler", "über"].map(line));
        b.set_filter("Error");
        assert_eq!(b.visible_len(), 1);
        b.set_filter("ÜBER");
        assert_eq!(b.visible_len(), 2);
    }
}

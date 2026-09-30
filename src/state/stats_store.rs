//! Live stats for the containers that are visible or selected.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use bollard::Docker;
use gpui::{Context, Task};

use crate::docker::stats::{self, Sample};
use crate::runtime::Backend;

pub const HISTORY_LEN: usize = 60;
/// The maximum number of streams for the visible rows. The selected
/// container always gets a stream, in addition to this limit.
pub const MAX_VISIBLE_STREAMS: usize = 30;

#[derive(Default)]
pub struct History {
    pub samples: VecDeque<Sample>,
}

impl History {
    pub fn push(&mut self, s: Sample) {
        if self.samples.len() == HISTORY_LEN {
            self.samples.pop_front();
        }
        self.samples.push_back(s);
    }

    pub fn latest(&self) -> Option<&Sample> {
        self.samples.back()
    }
}

pub struct StatsStore {
    pub history: HashMap<String, History>,
    streams: HashMap<String, Task<()>>,
    /// True when a sample arrived after the last UI update.
    dirty: bool,
    ticker: Option<Task<()>>,
}

/// Selects the ids that get a stream: the selected id, then the visible
/// ids in order, up to the limit.
pub fn select_wanted(visible: &[String], selected: Option<&str>, limit: usize) -> HashSet<String> {
    let mut wanted: HashSet<String> = visible.iter().take(limit).cloned().collect();
    if let Some(sel) = selected {
        wanted.insert(sel.to_string());
    }
    wanted
}

impl StatsStore {
    pub fn new() -> Self {
        StatsStore {
            history: HashMap::new(),
            streams: HashMap::new(),
            dirty: false,
            ticker: None,
        }
    }

    pub fn latest(&self, id: &str) -> Option<&Sample> {
        self.history.get(id).and_then(|h| h.latest())
    }

    /// Opens and closes streams so that only the `wanted` ids have one.
    /// The caller passes only running containers.
    pub fn set_wanted(&mut self, docker: Option<&Docker>, wanted: HashSet<String>, cx: &mut Context<Self>) {
        // A dropped task drops its channel, and then the Tokio side stops.
        self.streams.retain(|id, _| wanted.contains(id));
        self.history.retain(|id, _| wanted.contains(id));
        let Some(docker) = docker else {
            self.streams.clear();
            return;
        };
        let backend = Backend::global(cx);
        for id in wanted {
            if self.streams.contains_key(&id) {
                continue;
            }
            let rx = backend.stream(stats::stream(docker, &id));
            let key = id.clone();
            let task = cx.spawn(async move |this, cx| {
                while let Ok(sample) = rx.recv().await {
                    let ok = this.update(cx, |this, _| {
                        this.history.entry(key.clone()).or_default().push(sample);
                        this.dirty = true;
                    });
                    if ok.is_err() {
                        break;
                    }
                }
                // The stream ended (the container stopped). Forget the task.
                let _ = this.update(cx, |this, _| {
                    this.streams.remove(&key);
                });
            });
            self.streams.insert(id, task);
        }
        self.update_ticker(cx);
    }

    /// Streams send about one sample each second each. One ticker at 1 Hz
    /// updates the UI, so there is at most one render each second.
    fn update_ticker(&mut self, cx: &mut Context<Self>) {
        if self.streams.is_empty() {
            self.ticker = None;
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep = this.update(cx, |this, cx| {
                    if this.dirty {
                        this.dirty = false;
                        cx.notify();
                    }
                    !this.streams.is_empty()
                });
                if !matches!(keep, Ok(true)) {
                    let _ = this.update(cx, |this, _| this.ticker = None);
                    break;
                }
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_keeps_last_60() {
        let mut h = History::default();
        for i in 0..100 {
            h.push(Sample {
                cpu_percent: i as f64,
                ..Default::default()
            });
        }
        assert_eq!(h.samples.len(), HISTORY_LEN);
        assert_eq!(h.samples.front().unwrap().cpu_percent, 40.0);
        assert_eq!(h.latest().unwrap().cpu_percent, 99.0);
    }

    #[test]
    fn wanted_respects_limit_and_selection() {
        let visible: Vec<String> = (0..50).map(|i| i.to_string()).collect();
        let w = select_wanted(&visible, Some("99"), MAX_VISIBLE_STREAMS);
        assert_eq!(w.len(), MAX_VISIBLE_STREAMS + 1);
        assert!(w.contains("99"));
        assert!(w.contains("0"));
        assert!(!w.contains("45"));
    }
}

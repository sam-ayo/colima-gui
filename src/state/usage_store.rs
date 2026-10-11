//! The resource use of the VM of one Colima profile, for the gauges in the
//! sidebar.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{Context, Task};

use crate::colima::{self, UsageCounters};
use crate::runtime::Backend;

/// Each read starts `colima ssh`, which takes about 0.3 s. A slow poll
/// keeps that cost small.
const POLL: Duration = Duration::from_secs(3);
/// The time between the first two reads. The CPU use needs two reads, and
/// the gauges stay empty until the second read.
const FIRST_POLL: Duration = Duration::from_millis(500);

/// The resource use of a VM at one time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// The CPU use of all the processors, from 0 to 100.
    pub cpu_percent: f32,
    pub cpus: u32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub disk_used: u64,
    pub disk_total: u64,
}

/// The CPU use between two reads. The first read has no CPU use.
fn usage(prev: Option<UsageCounters>, now: UsageCounters) -> Usage {
    let cpu_percent = prev
        .and_then(|p| {
            let total = now.cpu_total.checked_sub(p.cpu_total)?;
            let idle = now.cpu_idle.checked_sub(p.cpu_idle)?;
            (total > 0).then(|| 100. * total.saturating_sub(idle) as f32 / total as f32)
        })
        .unwrap_or(0.);
    Usage {
        cpu_percent,
        cpus: now.cpus,
        mem_used: now.mem_used,
        mem_total: now.mem_total,
        disk_used: now.disk_used,
        disk_total: now.disk_total,
    }
}

#[derive(Default)]
pub struct UsageStore {
    /// The profile that the store reads.
    profile: Option<String>,
    /// The last use of `profile`. It is `None` until the first read ends.
    pub usage: Option<Usage>,
    /// The last counters of `profile`, for the CPU use of the next read.
    counters: Option<UsageCounters>,
    /// The last use and counters of the other running profiles. A switch
    /// back to one of them shows its use at once.
    saved: HashMap<String, (Usage, UsageCounters)>,
    _poll: Option<Task<()>>,
}

impl UsageStore {
    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// Reads the use of `profile` until the next call. `None` stops the
    /// reads.
    pub fn watch(&mut self, profile: Option<String>, cx: &mut Context<Self>) {
        if self.profile == profile {
            return;
        }
        if let (Some(old), Some(usage), Some(counters)) = (self.profile.take(), self.usage, self.counters) {
            self.saved.insert(old, (usage, counters));
        }
        let saved = profile.as_ref().and_then(|p| self.saved.remove(p));
        self.profile = profile.clone();
        self.usage = saved.map(|(usage, _)| usage);
        self.counters = saved.map(|(_, counters)| counters);
        cx.notify();
        let Some(profile) = profile else {
            self._poll = None;
            return;
        };
        let backend = Backend::global(cx);
        self._poll = Some(cx.spawn(async move |this, cx| {
            // With saved counters, the first read has a CPU use.
            let mut prev = saved.map(|(_, counters)| counters);
            let mut shown = false;
            loop {
                let rx = backend.spawn({
                    let profile = profile.clone();
                    async move { tokio::task::spawn_blocking(move || colima::usage(&profile)).await }
                });
                if let Ok(Ok(Ok(now))) = rx.await {
                    // The first read has no CPU use. Show the second read.
                    if let Some(prev) = prev {
                        let updated = this.update(cx, |this, cx| {
                            this.usage = Some(usage(Some(prev), now));
                            this.counters = Some(now);
                            cx.notify();
                        });
                        if updated.is_err() {
                            break;
                        }
                        shown = true;
                    }
                    prev = Some(now);
                }
                // Until a use shows, read again soon.
                cx.background_executor().timer(if shown { POLL } else { FIRST_POLL }).await;
            }
        }));
    }

    /// Forgets the saved use of the profiles that do not run. A machine
    /// that starts again has a different use.
    pub fn retain_running(&mut self, running: impl Fn(&str) -> bool) {
        self.saved.retain(|p, _| running(p));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_use_is_the_busy_part_of_the_time_between_reads() {
        let a = UsageCounters { cpu_total: 1000, cpu_idle: 900, ..Default::default() };
        let b = UsageCounters { cpu_total: 1200, cpu_idle: 1050, ..Default::default() };
        assert_eq!(usage(None, b).cpu_percent, 0.);
        assert_eq!(usage(Some(a), b).cpu_percent, 25.);
        // A restart of the VM sets the counters back to zero.
        assert_eq!(usage(Some(b), a).cpu_percent, 0.);
    }
}

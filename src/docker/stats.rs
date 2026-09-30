//! Live CPU and memory stats per container.

use bollard::Docker;
use bollard::models::ContainerStatsResponse;
use bollard::query_parameters::StatsOptions;
use futures::{Stream, StreamExt};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    /// CPU use in percent. 100% is one full CPU.
    pub cpu_percent: f64,
    /// Memory use in bytes, without the page cache.
    pub mem_bytes: u64,
    pub mem_limit: u64,
}

/// The raw counters that the calculation needs. This keeps the
/// calculation free of bollard types, so that unit tests are simple.
#[derive(Clone, Copy, Debug, Default)]
pub struct RawCounters {
    pub cpu_total: u64,
    pub precpu_total: u64,
    pub system: u64,
    pub presystem: u64,
    pub online_cpus: u32,
    pub mem_usage: u64,
    pub mem_inactive_file: u64,
    pub mem_limit: u64,
}

/// Calculates a sample with the same formula as `docker stats`.
pub fn compute(raw: RawCounters) -> Sample {
    let cpu_delta = raw.cpu_total.saturating_sub(raw.precpu_total) as f64;
    let system_delta = raw.system.saturating_sub(raw.presystem) as f64;
    let cpus = raw.online_cpus.max(1) as f64;
    let cpu_percent = if system_delta > 0.0 && cpu_delta > 0.0 {
        cpu_delta / system_delta * cpus * 100.0
    } else {
        0.0
    };
    Sample {
        cpu_percent,
        mem_bytes: raw.mem_usage.saturating_sub(raw.mem_inactive_file),
        mem_limit: raw.mem_limit,
    }
}

pub fn raw_counters(s: &ContainerStatsResponse) -> RawCounters {
    let cpu = s.cpu_stats.as_ref();
    let pre = s.precpu_stats.as_ref();
    let mem = s.memory_stats.as_ref();
    let online_cpus = cpu
        .and_then(|c| c.online_cpus)
        .filter(|n| *n > 0)
        .or_else(|| {
            cpu.and_then(|c| c.cpu_usage.as_ref())
                .and_then(|u| u.percpu_usage.as_ref())
                .map(|v| v.len() as u32)
        })
        .unwrap_or(1);
    // cgroup v2 uses `inactive_file`. cgroup v1 uses `total_inactive_file`.
    let inactive = mem
        .and_then(|m| m.stats.as_ref())
        .and_then(|st| st.get("inactive_file").or_else(|| st.get("total_inactive_file")))
        .copied()
        .unwrap_or(0);
    RawCounters {
        cpu_total: cpu
            .and_then(|c| c.cpu_usage.as_ref())
            .and_then(|u| u.total_usage)
            .unwrap_or(0),
        precpu_total: pre
            .and_then(|c| c.cpu_usage.as_ref())
            .and_then(|u| u.total_usage)
            .unwrap_or(0),
        system: cpu.and_then(|c| c.system_cpu_usage).unwrap_or(0),
        presystem: pre.and_then(|c| c.system_cpu_usage).unwrap_or(0),
        online_cpus,
        mem_usage: mem.and_then(|m| m.usage).unwrap_or(0),
        mem_inactive_file: inactive,
        mem_limit: mem.and_then(|m| m.limit).unwrap_or(0),
    }
}

/// Opens a stats stream. The engine sends one sample each second.
pub fn stream(docker: &Docker, id: &str) -> impl Stream<Item = Sample> + Send + 'static {
    let opts = StatsOptions {
        stream: true,
        one_shot: false,
    };
    docker
        .stats(id, Some(opts))
        .take_while(|r| futures::future::ready(r.is_ok()))
        .filter_map(|r| futures::future::ready(r.ok().map(|s| compute(raw_counters(&s)))))
}

pub fn format_bytes(b: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = b as f64;
    if b < KIB {
        format!("{b:.0} B")
    } else if b < KIB * KIB {
        format!("{:.1} KiB", b / KIB)
    } else if b < KIB * KIB * KIB {
        format!("{:.1} MiB", b / KIB / KIB)
    } else {
        format!("{:.2} GiB", b / KIB / KIB / KIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_percent_uses_deltas_and_cpu_count() {
        let s = compute(RawCounters {
            cpu_total: 2_000,
            precpu_total: 1_000,
            system: 20_000,
            presystem: 10_000,
            online_cpus: 4,
            ..Default::default()
        });
        // 1000 / 10000 * 4 * 100 = 40%
        assert!((s.cpu_percent - 40.0).abs() < 1e-9);
    }

    #[test]
    fn cpu_is_zero_when_counters_do_not_change() {
        let s = compute(RawCounters {
            cpu_total: 5_000,
            precpu_total: 5_000,
            system: 50_000,
            presystem: 50_000,
            online_cpus: 2,
            ..Default::default()
        });
        assert_eq!(s.cpu_percent, 0.0);
    }

    #[test]
    fn memory_excludes_inactive_file_cache() {
        let s = compute(RawCounters {
            mem_usage: 100 * 1024 * 1024,
            mem_inactive_file: 30 * 1024 * 1024,
            mem_limit: 1024 * 1024 * 1024,
            ..Default::default()
        });
        assert_eq!(s.mem_bytes, 70 * 1024 * 1024);
        assert_eq!(s.mem_limit, 1024 * 1024 * 1024);
    }

    #[test]
    fn memory_does_not_underflow() {
        let s = compute(RawCounters {
            mem_usage: 10,
            mem_inactive_file: 20,
            ..Default::default()
        });
        assert_eq!(s.mem_bytes, 0);
    }

    #[test]
    fn bytes_format() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(70 * 1024 * 1024), "70.0 MiB");
    }
}

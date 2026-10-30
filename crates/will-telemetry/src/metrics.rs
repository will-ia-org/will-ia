//! Contadores de uso, baratos (atômicos / um mutex), lidos pelo `heartbeat`.

use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct Metrics {
    /// error, warn, info, debug, trace
    by_level: [AtomicU64; 5],
    by_module: Mutex<BTreeMap<String, ModuleCounters>>,
    /// Spans fechados por nome (`turn`, `tool_call`, `inbound`…), e por nome+módulo.
    spans: Mutex<BTreeMap<String, u64>>,
    dropped: AtomicU64,
    last_snapshot: Mutex<Option<std::time::Instant>>,
}

#[derive(Default, Clone, serde::Serialize)]
pub struct ModuleCounters {
    pub events: u64,
    pub warn: u64,
    pub error: u64,
}

#[derive(Clone, serde::Serialize)]
pub struct Snapshot {
    pub interval_s: u64,
    pub by_level: BTreeMap<&'static str, u64>,
    pub by_module: BTreeMap<String, ModuleCounters>,
    pub spans: BTreeMap<String, u64>,
    pub dropped: u64,
}

const LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];

impl Metrics {
    pub fn record_event(&self, level: tracing::Level, module: &str) {
        let i = level_index(level);
        self.by_level[i].fetch_add(1, Ordering::Relaxed);
        let mut m = self.by_module.lock();
        let c = m.entry(module.to_string()).or_default();
        c.events += 1;
        match i {
            0 => c.error += 1,
            1 => c.warn += 1,
            _ => {}
        }
    }

    pub fn record_span(&self, name: &str, module: &str) {
        let mut s = self.spans.lock();
        *s.entry(name.to_string()).or_default() += 1;
        *s.entry(format!("{name}:{module}")).or_default() += 1;
    }

    pub fn record_dropped(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Devolve os contadores desde o último snapshot e zera (delta).
    pub fn take_snapshot(&self) -> Snapshot {
        let now = std::time::Instant::now();
        let interval_s = {
            let mut last = self.last_snapshot.lock();
            let d = last.map(|t| now.duration_since(t).as_secs()).unwrap_or(0);
            *last = Some(now);
            d
        };
        let by_level = LEVELS
            .iter()
            .enumerate()
            .map(|(i, n)| (*n, self.by_level[i].swap(0, Ordering::Relaxed)))
            .collect();
        let by_module = std::mem::take(&mut *self.by_module.lock());
        let spans = std::mem::take(&mut *self.spans.lock());
        Snapshot {
            interval_s,
            by_level,
            by_module,
            spans,
            dropped: self.dropped.swap(0, Ordering::Relaxed),
        }
    }
}

fn level_index(level: tracing::Level) -> usize {
    match level {
        tracing::Level::ERROR => 0,
        tracing::Level::WARN => 1,
        tracing::Level::INFO => 2,
        tracing::Level::DEBUG => 3,
        tracing::Level::TRACE => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_a_delta() {
        let m = Metrics::default();
        m.record_event(tracing::Level::WARN, "telegram");
        m.record_event(tracing::Level::INFO, "telegram");
        m.record_span("turn", "server");
        let s = m.take_snapshot();
        assert_eq!(s.by_level["warn"], 1);
        assert_eq!(s.by_module["telegram"].events, 2);
        assert_eq!(s.by_module["telegram"].warn, 1);
        assert_eq!(s.spans["turn"], 1);
        assert_eq!(s.spans["turn:server"], 1);
        let s2 = m.take_snapshot();
        assert_eq!(s2.by_level["warn"], 0);
        assert!(s2.by_module.is_empty());
    }
}

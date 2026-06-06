//! Lightweight in-process counters (SPEC §13.2). These are cheap atomic
//! counters surfaced in `/v1/status.features` and logs; no external metrics
//! backend is required for the MVP.

use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use serde_json::json;
use serde_json::Value;

#[derive(Debug, Default)]
pub struct Metrics {
    pub recall_requests: AtomicU64,
    pub search_requests: AtomicU64,
    pub writeback_accepted: AtomicU64,
    pub writeback_rejected: AtomicU64,
    pub sync_scanned: AtomicU64,
    pub sync_created: AtomicU64,
    pub sync_skipped: AtomicU64,
    pub sync_rejected: AtomicU64,
    pub policy_denials: AtomicU64,
    pub storage_errors: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn incr(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }

    fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    /// Snapshot the counters as a JSON object for status/observability.
    pub fn snapshot(&self) -> Value {
        json!({
            "recall_requests": Self::get(&self.recall_requests),
            "search_requests": Self::get(&self.search_requests),
            "writeback_accepted": Self::get(&self.writeback_accepted),
            "writeback_rejected": Self::get(&self.writeback_rejected),
            "sync_scanned": Self::get(&self.sync_scanned),
            "sync_created": Self::get(&self.sync_created),
            "sync_skipped": Self::get(&self.sync_skipped),
            "sync_rejected": Self::get(&self.sync_rejected),
            "policy_denials": Self::get(&self.policy_denials),
            "storage_errors": Self::get(&self.storage_errors),
        })
    }
}

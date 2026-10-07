//! Run counters and human-readable byte sizes.

use std::sync::atomic::{AtomicU64, Ordering};

/// Byte count formatted as B/KiB/MiB/GiB.
pub struct Bytes(pub u64);

impl std::fmt::Display for Bytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.0;
        let (div, unit) = if n >= 1 << 30 {
            (1u64 << 30, "GiB")
        } else if n >= 1 << 20 {
            (1u64 << 20, "MiB")
        } else if n >= 1 << 10 {
            (1u64 << 10, "KiB")
        } else {
            return write!(f, "{n} B");
        };
        write!(f, "{:.2} {unit}", n as f64 / div as f64)
    }
}

/// Aggregated per-run counters, shared across worker tasks.
#[derive(Default)]
pub struct Stats {
    ok: AtomicU64,
    failed: AtomicU64,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
}

/// Point-in-time copy of the counters.
pub struct Snapshot {
    pub ok: u64,
    pub failed: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

impl Stats {
    pub fn record_ok(&self, n_in: u64, n_out: u64) {
        self.ok.fetch_add(1, Ordering::Relaxed);
        self.bytes_in.fetch_add(n_in, Ordering::Relaxed);
        self.bytes_out.fetch_add(n_out, Ordering::Relaxed);
    }

    pub fn record_fail(&self) {
        self.failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Failures so far.
    pub fn failed(&self) -> u64 {
        self.failed.load(Ordering::Relaxed)
    }

    /// Completions so far (successes + failures).
    pub fn done(&self) -> u64 {
        self.ok.load(Ordering::Relaxed) + self.failed.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            ok: self.ok.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
        }
    }
}

//! Fixed-bucket latency histogram, log-scale in nanoseconds.
use neuroos_common::sync::lock;
use std::sync::Mutex;
use std::time::Duration;

/// Upper bounds in ns: 1us .. ~10s, one decade apart plus a 5x step each decade.
const BUCKET_UPPER_BOUNDS_NS: &[u64] = &[
    1_000,
    5_000,
    10_000,
    50_000,
    100_000,
    500_000,
    1_000_000,
    5_000_000,
    10_000_000,
    50_000_000,
    100_000_000,
    500_000_000,
    1_000_000_000,
    10_000_000_000,
    u64::MAX, // overflow bucket
];

/// L7: the three fields are one value under one lock. They used to have a
/// mutex each, so `to_proto` could be scheduled between `record`'s three
/// updates and report a `count` that the `bucket_counts` and `sum_ns` did
/// not yet account for -- a snapshot that never existed. Every scrape of
/// every component's health endpoint reads this, so the inconsistency was
/// visible in `neuroosctl status` output rather than theoretical.
#[derive(Debug)]
struct Counters {
    counts: Vec<u64>,
    count: u64,
    sum_ns: u64,
}

#[derive(Debug)]
pub struct Histogram {
    counters: Mutex<Counters>,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            counters: Mutex::new(Counters {
                counts: vec![0; BUCKET_UPPER_BOUNDS_NS.len()],
                count: 0,
                sum_ns: 0,
            }),
        }
    }
}

impl Histogram {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, d: Duration) {
        let ns = d.as_nanos().min(u64::MAX as u128) as u64;
        let idx = BUCKET_UPPER_BOUNDS_NS
            .iter()
            .position(|&b| ns <= b)
            .unwrap_or(BUCKET_UPPER_BOUNDS_NS.len() - 1);
        let mut counters = lock(&self.counters);
        counters.counts[idx] = counters.counts[idx].saturating_add(1);
        counters.count = counters.count.saturating_add(1);
        // L7: `+=` here could overflow -- a `Duration` past ~584 years
        // clamps `ns` to `u64::MAX` (see `record`'s first line), so two
        // such records wrapped the sum in release and panicked in debug,
        // which rules.md §5.1 forbids outright in non-test code. A
        // saturating sum reports "at least this much" instead.
        counters.sum_ns = counters.sum_ns.saturating_add(ns);
    }

    pub fn to_proto(&self) -> neuroos_proto::v1::LatencyHistogram {
        // L7: one lock, so the three numbers in the response are a
        // consistent snapshot of the same instant.
        let counters = lock(&self.counters);
        neuroos_proto::v1::LatencyHistogram {
            bucket_upper_bound_ns: BUCKET_UPPER_BOUNDS_NS.to_vec(),
            bucket_counts: counters.counts.clone(),
            count: counters.count,
            sum_ns: counters.sum_ns,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn records_into_correct_bucket() {
        let h = Histogram::new();
        h.record(Duration::from_micros(1)); // 1000ns -> bucket 0 (<=1000)
        h.record(Duration::from_millis(1)); // 1_000_000ns -> bucket for 1ms
        let p = h.to_proto();
        assert_eq!(p.count, 2);
        assert_eq!(p.sum_ns, 1_000 + 1_000_000);
        assert_eq!(p.bucket_counts[0], 1);
        let ms_idx = BUCKET_UPPER_BOUNDS_NS
            .iter()
            .position(|&b| b == 1_000_000)
            .unwrap();
        assert_eq!(p.bucket_counts[ms_idx], 1);
    }

    #[test]
    fn huge_duration_lands_in_overflow_bucket() {
        let h = Histogram::new();
        h.record(Duration::from_secs(3600));
        let p = h.to_proto();
        assert_eq!(*p.bucket_counts.last().unwrap(), 1);
    }

    /// L7: `sum_ns` used to be a plain `+=`, so a second saturating
    /// record panicked in a debug build (rules.md §5.1) and silently
    /// wrapped to a tiny number in a release one.
    #[test]
    fn sum_ns_saturates_instead_of_overflowing() {
        let h = Histogram::new();
        // `as_nanos()` for this is far past u64::MAX, so `ns` clamps to
        // u64::MAX and one more record would overflow the sum.
        h.record(Duration::from_secs(u64::MAX));
        h.record(Duration::from_secs(u64::MAX));
        let p = h.to_proto();
        assert_eq!(p.count, 2);
        assert_eq!(p.sum_ns, u64::MAX, "the sum must saturate, not wrap");
    }

    /// L7: `count`, `bucket_counts` and `sum_ns` are read under one lock,
    /// so a snapshot taken while another thread is recording is always
    /// self-consistent: the buckets sum to `count`, and `sum_ns` is at
    /// least `count` nanoseconds (each record here is >= 1ns).
    #[test]
    fn a_snapshot_is_consistent_under_concurrent_records() {
        use std::sync::Arc;

        let h = Arc::new(Histogram::new());
        let writers: Vec<_> = (0..4)
            .map(|_| {
                let h = Arc::clone(&h);
                std::thread::spawn(move || {
                    for _ in 0..2_000 {
                        h.record(Duration::from_micros(7));
                    }
                })
            })
            .collect();

        // Snapshot repeatedly while the writers run: before the fix each
        // field had its own lock, so `count` could already include a
        // record whose bucket and sum had not landed yet.
        for _ in 0..500 {
            let p = h.to_proto();
            let bucket_total: u64 = p.bucket_counts.iter().sum();
            assert_eq!(
                bucket_total, p.count,
                "bucket counts must sum to count in every snapshot"
            );
            assert_eq!(
                p.sum_ns,
                p.count * 7_000,
                "sum_ns must match the records counted"
            );
        }

        for w in writers {
            w.join().expect("writer thread must not panic");
        }
        let p = h.to_proto();
        assert_eq!(p.count, 8_000);
        assert_eq!(p.bucket_counts.iter().sum::<u64>(), 8_000);
        assert_eq!(p.sum_ns, 8_000 * 7_000);
    }

    #[test]
    fn empty_histogram_has_zero_count() {
        let h = Histogram::new();
        let p = h.to_proto();
        assert_eq!(p.count, 0);
        assert_eq!(p.sum_ns, 0);
        assert!(p.bucket_counts.iter().all(|&c| c == 0));
    }
}

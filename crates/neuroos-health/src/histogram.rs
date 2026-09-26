//! Fixed-bucket latency histogram, log-scale in nanoseconds.
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

#[derive(Debug)]
pub struct Histogram {
    counts: Mutex<Vec<u64>>,
    count: Mutex<u64>,
    sum_ns: Mutex<u64>,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            counts: Mutex::new(vec![0; BUCKET_UPPER_BOUNDS_NS.len()]),
            count: Mutex::new(0),
            sum_ns: Mutex::new(0),
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
        self.counts.lock().unwrap()[idx] += 1;
        *self.count.lock().unwrap() += 1;
        *self.sum_ns.lock().unwrap() += ns;
    }

    pub fn to_proto(&self) -> neuroos_proto::v1::LatencyHistogram {
        neuroos_proto::v1::LatencyHistogram {
            bucket_upper_bound_ns: BUCKET_UPPER_BOUNDS_NS.to_vec(),
            bucket_counts: self.counts.lock().unwrap().clone(),
            count: *self.count.lock().unwrap(),
            sum_ns: *self.sum_ns.lock().unwrap(),
        }
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn empty_histogram_has_zero_count() {
        let h = Histogram::new();
        let p = h.to_proto();
        assert_eq!(p.count, 0);
        assert_eq!(p.sum_ns, 0);
        assert!(p.bucket_counts.iter().all(|&c| c == 0));
    }
}

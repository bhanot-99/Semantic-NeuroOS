// moved from neuroos-healthd: shared with neuroosctl for status display (P1-S04)
//! Percentile estimation from a bucketed `LatencyHistogram` (cumulative
//! count until the target fraction is reached — the same technique
//! Prometheus/OpenTelemetry histograms use for `histogram_quantile`).
use neuroos_proto::v1::LatencyHistogram;

/// Estimates the `p`-th percentile (`p` in `0.0..=1.0`) in nanoseconds.
/// Returns `None` for an empty histogram or an out-of-range `p`.
pub fn percentile_ns(hist: &LatencyHistogram, p: f64) -> Option<u64> {
    if hist.count == 0 || !(0.0..=1.0).contains(&p) {
        return None;
    }
    let target = (hist.count as f64 * p).ceil() as u64;
    let mut cumulative = 0u64;
    for (bound, count) in hist.bucket_upper_bound_ns.iter().zip(hist.bucket_counts.iter()) {
        cumulative += count;
        if cumulative >= target.max(1) {
            return Some(*bound);
        }
    }
    hist.bucket_upper_bound_ns.last().copied()
}

pub fn p50_ns(hist: &LatencyHistogram) -> Option<u64> {
    percentile_ns(hist, 0.50)
}

pub fn p99_ns(hist: &LatencyHistogram) -> Option<u64> {
    percentile_ns(hist, 0.99)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    fn hist(bounds: &[u64], counts: &[u64]) -> LatencyHistogram {
        let count = counts.iter().sum();
        LatencyHistogram {
            bucket_upper_bound_ns: bounds.to_vec(),
            bucket_counts: counts.to_vec(),
            count,
            sum_ns: 0,
        }
    }

    #[test]
    fn empty_histogram_has_no_percentile() {
        let h = hist(&[100, 200], &[0, 0]);
        assert_eq!(percentile_ns(&h, 0.5), None);
    }

    #[test]
    fn all_in_one_bucket() {
        let h = hist(&[100, 200, 300], &[0, 10, 0]);
        assert_eq!(p50_ns(&h), Some(200));
        assert_eq!(p99_ns(&h), Some(200));
    }

    #[test]
    fn p50_and_p99_from_a_spread_distribution() {
        // 100 samples: 90 in the <=10ns bucket, 9 in <=20ns, 1 in <=1000ns.
        let h = hist(&[10, 20, 1000], &[90, 9, 1]);
        assert_eq!(p50_ns(&h), Some(10));
        assert_eq!(p99_ns(&h), Some(20));
    }

    #[test]
    fn out_of_range_percentile_is_none() {
        let h = hist(&[100], &[5]);
        assert_eq!(percentile_ns(&h, 1.5), None);
        assert_eq!(percentile_ns(&h, -0.1), None);
    }
}

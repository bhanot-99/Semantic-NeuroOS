//! Soak engine (PRD FR-HLT-04): capture a baseline (RSS, p99) per
//! component, then flag RSS growth > 5% or p99 drift > 10% against it.
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
#[cfg(test)]
use std::path::Path;
use std::sync::Mutex;

const RSS_GROWTH_BREACH_PCT: f64 = 5.0;
const P99_DRIFT_BREACH_PCT: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Baseline {
    pub rss_bytes: u64,
    pub p99_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreachKind {
    RssGrowth,
    P99Drift,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Breach {
    pub component: String,
    pub kind: BreachKind,
    pub baseline: u64,
    pub current: u64,
    pub pct: f64,
}

/// Pure drift math, fixed-vector-testable independent of any I/O: `None`
/// baseline values (component never captured) never breach.
pub fn check_breaches(component: &str, baseline: Option<Baseline>, rss_bytes: u64, p99_ns: u64) -> Vec<Breach> {
    let Some(baseline) = baseline else {
        return Vec::new();
    };
    let mut breaches = Vec::new();

    if baseline.rss_bytes > 0 {
        let growth_pct = ((rss_bytes as f64 - baseline.rss_bytes as f64) / baseline.rss_bytes as f64) * 100.0;
        if growth_pct > RSS_GROWTH_BREACH_PCT {
            breaches.push(Breach {
                component: component.to_string(),
                kind: BreachKind::RssGrowth,
                baseline: baseline.rss_bytes,
                current: rss_bytes,
                pct: growth_pct,
            });
        }
    }

    if baseline.p99_ns > 0 {
        let drift_pct = ((p99_ns as f64 - baseline.p99_ns as f64) / baseline.p99_ns as f64) * 100.0;
        if drift_pct > P99_DRIFT_BREACH_PCT {
            breaches.push(Breach {
                component: component.to_string(),
                kind: BreachKind::P99Drift,
                baseline: baseline.p99_ns,
                current: p99_ns,
                pct: drift_pct,
            });
        }
    }

    breaches
}

pub struct SoakEngine {
    baselines: Mutex<HashMap<String, Baseline>>,
    csv_path: PathBuf,
}

impl SoakEngine {
    pub fn new(csv_path: impl Into<PathBuf>) -> Self {
        Self { baselines: Mutex::new(HashMap::new()), csv_path: csv_path.into() }
    }

    pub fn set_baseline(&self, component: &str, baseline: Baseline) {
        self.baselines.lock().unwrap_or_else(|p| p.into_inner()).insert(component.to_string(), baseline);
    }

    pub fn baseline_for(&self, component: &str) -> Option<Baseline> {
        self.baselines.lock().unwrap_or_else(|p| p.into_inner()).get(component).copied()
    }

    /// Checks `component` against its baseline and appends one CSV row
    /// (creating the file with a header if it doesn't exist yet). Returns
    /// any breaches found.
    pub fn record(&self, component: &str, rss_bytes: u64, p99_ns: u64) -> std::io::Result<Vec<Breach>> {
        let baseline = self.baseline_for(component);
        let breaches = check_breaches(component, baseline, rss_bytes, p99_ns);
        self.append_csv_row(component, rss_bytes, p99_ns, &breaches)?;
        Ok(breaches)
    }

    /// Hand-rolled CSV (no crate dependency for a 5-column, comma/newline-free
    /// row: `component` is always one of our own fixed target names, and
    /// `breach_summary` only ever emits `;`-separated `key=value` pairs).
    fn append_csv_row(&self, component: &str, rss_bytes: u64, p99_ns: u64, breaches: &[Breach]) -> std::io::Result<()> {
        if let Some(parent) = self.csv_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file_is_new = !self.csv_path.exists();
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&self.csv_path)?;
        if file_is_new {
            writeln!(file, "timestamp_ns,component,rss_bytes,p99_ns,breach")?;
        }
        writeln!(
            file,
            "{},{component},{rss_bytes},{p99_ns},{}",
            neuroos_common::now_ns(),
            breach_summary(breaches)
        )?;
        Ok(())
    }
}

fn breach_summary(breaches: &[Breach]) -> String {
    if breaches.is_empty() {
        return String::new();
    }
    breaches
        .iter()
        .map(|b| match b.kind {
            BreachKind::RssGrowth => format!("rss_growth={:.1}%", b.pct),
            BreachKind::P99Drift => format!("p99_drift={:.1}%", b.pct),
        })
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
pub fn read_csv_for_test(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn no_baseline_never_breaches() {
        assert_eq!(check_breaches("x", None, 999_999_999, 999_999_999), vec![]);
    }

    #[test]
    fn exactly_5_percent_rss_growth_does_not_breach() {
        let baseline = Baseline { rss_bytes: 100_000_000, p99_ns: 0 };
        let breaches = check_breaches("x", Some(baseline), 105_000_000, 0);
        assert!(breaches.is_empty(), "exactly 5% must not breach (only >5%)");
    }

    #[test]
    fn just_over_5_percent_rss_growth_breaches() {
        let baseline = Baseline { rss_bytes: 100_000_000, p99_ns: 0 };
        let breaches = check_breaches("x", Some(baseline), 105_000_001, 0);
        assert_eq!(breaches.len(), 1);
        assert_eq!(breaches[0].kind, BreachKind::RssGrowth);
    }

    #[test]
    fn exactly_10_percent_p99_drift_does_not_breach() {
        let baseline = Baseline { rss_bytes: 0, p99_ns: 10_000_000 };
        let breaches = check_breaches("x", Some(baseline), 0, 11_000_000);
        assert!(breaches.is_empty());
    }

    #[test]
    fn just_over_10_percent_p99_drift_breaches() {
        let baseline = Baseline { rss_bytes: 0, p99_ns: 10_000_000 };
        let breaches = check_breaches("x", Some(baseline), 0, 11_000_001);
        assert_eq!(breaches.len(), 1);
        assert_eq!(breaches[0].kind, BreachKind::P99Drift);
    }

    #[test]
    fn rss_shrinking_never_breaches() {
        let baseline = Baseline { rss_bytes: 100_000_000, p99_ns: 0 };
        let breaches = check_breaches("x", Some(baseline), 1, 0);
        assert!(breaches.is_empty());
    }

    #[test]
    fn both_can_breach_simultaneously() {
        let baseline = Baseline { rss_bytes: 100, p99_ns: 100 };
        let breaches = check_breaches("x", Some(baseline), 1000, 1000);
        assert_eq!(breaches.len(), 2);
    }

    #[test]
    fn record_writes_a_csv_header_and_row() {
        let dir = tempfile::tempdir().unwrap();
        let csv_path = dir.path().join("soak.csv");
        let engine = SoakEngine::new(&csv_path);
        engine.set_baseline("comp", Baseline { rss_bytes: 100, p99_ns: 100 });
        engine.record("comp", 200, 100).unwrap();
        let text = read_csv_for_test(&csv_path);
        assert!(text.starts_with("timestamp_ns,component,rss_bytes,p99_ns,breach"));
        assert!(text.contains("comp,200,100,rss_growth=100.0%"));
    }

    #[test]
    fn record_appends_multiple_rows() {
        let dir = tempfile::tempdir().unwrap();
        let csv_path = dir.path().join("soak.csv");
        let engine = SoakEngine::new(&csv_path);
        engine.record("comp", 1, 1).unwrap();
        engine.record("comp", 2, 2).unwrap();
        let text = read_csv_for_test(&csv_path);
        assert_eq!(text.lines().count(), 3); // header + 2 rows
    }
}

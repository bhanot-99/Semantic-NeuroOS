//! The in-memory aggregate model: one record per target, updated by each
//! scrape cycle, read by the `healthd.sock` server and the soak engine.
use std::collections::HashMap;
use std::sync::Mutex;

use neuroos_proto::v1::{LatencyHistogram, Status};

use crate::targets::Target;

#[derive(Debug, Clone, PartialEq)]
pub struct ComponentRecord {
    pub name: String,
    pub status: Status,
    pub rss_bytes: u64,
    pub budget_bytes: u64,
    pub uptime_s: u64,
    pub latency_histograms: HashMap<String, LatencyHistogram>,
    pub error_counters: HashMap<String, u64>,
    pub last_error_at_ns: u64,
    pub build_info: String,
}

impl ComponentRecord {
    /// A record for a target that has never been successfully scraped yet
    /// (PRD FR-HLT-01/02's "UNKNOWN" state, distinct from "DOWN": DOWN means
    /// "was reachable, now isn't").
    pub fn unknown(target: &Target) -> Self {
        Self {
            name: target.name.clone(),
            status: Status::Unknown,
            rss_bytes: 0,
            budget_bytes: target.budget_bytes,
            uptime_s: 0,
            latency_histograms: HashMap::new(),
            error_counters: HashMap::new(),
            last_error_at_ns: 0,
            build_info: String::new(),
        }
    }

    /// Architecture.md §7.1: "healthd alerts at 90% of budget." A component
    /// reporting OK but over budget is downgraded to DEGRADED; DOWN/UNKNOWN
    /// are left as-is (budget doesn't matter if we can't reach it anyway).
    pub fn apply_budget_alert(mut self) -> Self {
        if self.status == Status::Ok && self.budget_bytes > 0 && self.rss_bytes * 10 >= self.budget_bytes * 9 {
            self.status = Status::Degraded;
        }
        self
    }
}

#[derive(Default)]
pub struct Aggregate {
    records: Mutex<HashMap<String, ComponentRecord>>,
}

impl Aggregate {
    pub fn new(targets: &[Target]) -> Self {
        let records =
            targets.iter().map(|t| (t.name.clone(), ComponentRecord::unknown(t))).collect();
        Self { records: Mutex::new(records) }
    }

    pub fn update(&self, record: ComponentRecord) {
        let mut records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        records.insert(record.name.clone(), record);
    }

    pub fn snapshot(&self) -> Vec<ComponentRecord> {
        let records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<_> = records.values().cloned().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use std::path::PathBuf;

    fn target(name: &str, budget_mib: u64) -> Target {
        Target { name: name.into(), socket: PathBuf::from("/tmp/x.sock"), budget_bytes: budget_mib * 1024 * 1024, cgroup_path: None }
    }

    fn get(agg: &Aggregate, name: &str) -> Option<ComponentRecord> {
        agg.snapshot().into_iter().find(|r| r.name == name)
    }

    #[test]
    fn starts_unknown_until_scraped() {
        let agg = Aggregate::new(&[target("a", 10)]);
        assert_eq!(get(&agg, "a").unwrap().status, Status::Unknown);
    }

    #[test]
    fn update_replaces_the_record() {
        let agg = Aggregate::new(&[target("a", 10)]);
        let mut rec = ComponentRecord::unknown(&target("a", 10));
        rec.status = Status::Ok;
        rec.rss_bytes = 123;
        agg.update(rec);
        let got = get(&agg, "a").unwrap();
        assert_eq!(got.status, Status::Ok);
        assert_eq!(got.rss_bytes, 123);
    }

    #[test]
    fn budget_alert_downgrades_ok_to_degraded_at_90_percent() {
        let mut rec = ComponentRecord::unknown(&target("a", 100));
        rec.status = Status::Ok;
        rec.rss_bytes = 91 * 1024 * 1024; // 91% of 100 MiB
        let rec = rec.apply_budget_alert();
        assert_eq!(rec.status, Status::Degraded);
    }

    #[test]
    fn budget_alert_leaves_ok_under_90_percent() {
        let mut rec = ComponentRecord::unknown(&target("a", 100));
        rec.status = Status::Ok;
        rec.rss_bytes = 50 * 1024 * 1024;
        let rec = rec.apply_budget_alert();
        assert_eq!(rec.status, Status::Ok);
    }

    #[test]
    fn budget_alert_never_upgrades_down_to_anything() {
        let mut rec = ComponentRecord::unknown(&target("a", 100));
        rec.status = Status::Down;
        rec.rss_bytes = 999 * 1024 * 1024;
        let rec = rec.apply_budget_alert();
        assert_eq!(rec.status, Status::Down);
    }

    #[test]
    fn snapshot_is_sorted_by_name() {
        let agg = Aggregate::new(&[target("zebra", 1), target("apple", 1)]);
        let names: Vec<_> = agg.snapshot().iter().map(|r| r.name.clone()).collect();
        assert_eq!(names, vec!["apple", "zebra"]);
    }
}

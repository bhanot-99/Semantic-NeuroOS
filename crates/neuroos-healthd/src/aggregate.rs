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
        if self.status == Status::Ok
            && self.budget_bytes > 0
            && self.rss_bytes * 10 >= self.budget_bytes * 9
        {
            self.status = Status::Degraded;
        }
        self
    }
}

#[derive(Default)]
pub struct Aggregate {
    records: Mutex<HashMap<String, ComponentRecord>>,
    /// Targets that have answered a scrape at least once. M4: `scrape_one`
    /// has no history of its own, so it reports every failure as DOWN --
    /// including the very first one, which contradicts
    /// [`ComponentRecord::unknown`]'s documented distinction ("DOWN means
    /// was reachable, now isn't"). The aggregate is where that history
    /// lives, so it is where the demotion to UNKNOWN happens.
    ever_reached: Mutex<std::collections::HashSet<String>>,
}

impl Aggregate {
    pub fn new(targets: &[Target]) -> Self {
        let records = targets
            .iter()
            .map(|t| (t.name.clone(), ComponentRecord::unknown(t)))
            .collect();
        Self {
            records: Mutex::new(records),
            ever_reached: Mutex::new(std::collections::HashSet::new()),
        }
    }

    pub fn update(&self, mut record: ComponentRecord) {
        match record.status {
            // Unreachable (`scrape_one` could not connect). If it answered
            // at some point in this healthd's life, it did not fail to
            // start -- it died, which is DOWN.
            Status::Unknown if self.ever_reached(&record.name) => {
                record.status = Status::Down;
            }
            Status::Ok | Status::Degraded => {
                self.ever_reached
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(record.name.clone());
            }
            _ => {}
        }
        let mut records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        records.insert(record.name.clone(), record);
    }

    /// Whether `name` has answered a scrape at least once -- the soak
    /// engine's cue that a baseline captured now would be real (M4).
    pub fn ever_reached(&self, name: &str) -> bool {
        self.ever_reached
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(name)
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
        Target {
            name: name.into(),
            socket: PathBuf::from("/tmp/x.sock"),
            budget_bytes: budget_mib * 1024 * 1024,
            cgroup_path: None,
        }
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

    /// M4: a target that has never been reachable is UNKNOWN (it may not
    /// be installed yet), which `ComponentRecord::unknown`'s own doc
    /// comment already said but nothing honoured.
    #[test]
    fn a_target_that_was_never_reachable_stays_unknown() {
        let agg = Aggregate::new(&[target("a", 10)]);
        agg.update(ComponentRecord::unknown(&target("a", 10)));
        assert_eq!(get(&agg, "a").unwrap().status, Status::Unknown);
        assert!(!agg.ever_reached("a"));
    }

    /// Once it has answered, becoming unreachable means it died: DOWN.
    #[test]
    fn a_target_that_answered_once_is_down_when_it_disappears() {
        let agg = Aggregate::new(&[target("a", 10)]);
        let mut up = ComponentRecord::unknown(&target("a", 10));
        up.status = Status::Ok;
        agg.update(up);
        assert!(agg.ever_reached("a"));

        agg.update(ComponentRecord::unknown(&target("a", 10)));
        assert_eq!(get(&agg, "a").unwrap().status, Status::Down);
    }

    /// A target that accepts the connection and then misbehaves is
    /// reachable and broken, so `scrape_one` reports DOWN directly and the
    /// aggregate leaves it alone.
    #[test]
    fn a_reachable_but_broken_target_is_down_on_the_first_failure() {
        let agg = Aggregate::new(&[target("a", 10)]);
        let mut rec = ComponentRecord::unknown(&target("a", 10));
        rec.status = Status::Down;
        agg.update(rec);
        assert_eq!(get(&agg, "a").unwrap().status, Status::Down);
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

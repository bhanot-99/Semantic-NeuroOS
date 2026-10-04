//! `/proc` sensor: system resource sampling (FR-MON-04) and process-tree
//! snapshots for the focused window's PID (FR-MON-05), plus the app_id → PID
//! resolution heuristic ADR-0009 records as a real constraint found while
//! building P3-S01: no Wayland protocol or D-Bus interface on the reference
//! COSMIC session exposes a toplevel's PID, so it is approximated by
//! matching `app_id` against the running process list.
use std::collections::HashMap;

use procfs::{Current, CurrentSI};

use neuroos_proto::v1::{ProcessInfo, ProcessTreeSnapshot};

#[derive(Debug, thiserror::Error)]
pub enum ProcError {
    #[error("/proc read failed: {0}")]
    Procfs(#[from] procfs::ProcError),
}

/// Tracks the previous `/proc/stat` sample so successive calls can report a
/// CPU-percent delta rather than the since-boot cumulative average.
pub struct ResourceSampler {
    prev_total_ticks: Option<(u64, u64)>, // (busy_ticks, total_ticks)
}

impl Default for ResourceSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceSampler {
    pub fn new() -> Self {
        Self {
            prev_total_ticks: None,
        }
    }

    /// `None` on the first call (no prior sample to diff against).
    pub fn sample(&mut self) -> Result<Option<SampledResources>, ProcError> {
        let stat = procfs::KernelStats::current()?;
        let mem = procfs::Meminfo::current()?;
        let idle = stat.total.idle + stat.total.iowait.unwrap_or(0);
        let busy = stat.total.user
            + stat.total.nice
            + stat.total.system
            + stat.total.irq.unwrap_or(0)
            + stat.total.softirq.unwrap_or(0)
            + stat.total.steal.unwrap_or(0);
        let total = busy + idle;

        let result = self
            .prev_total_ticks
            .map(|(prev_busy, prev_total)| {
                cpu_percent_from_ticks(prev_busy, prev_total, busy, total)
            })
            .map(|cpu_percent| SampledResources {
                cpu_percent,
                mem_used_bytes: mem
                    .mem_total
                    .saturating_sub(mem.mem_available.unwrap_or(mem.mem_free)),
                mem_total_bytes: mem.mem_total,
            });
        self.prev_total_ticks = Some((busy, total));
        Ok(result)
    }
}

pub struct SampledResources {
    pub cpu_percent: f64,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
}

/// Pure so it's testable without real `/proc` state: percent busy between two
/// monotonically increasing (busy, total) tick samples.
fn cpu_percent_from_ticks(prev_busy: u64, prev_total: u64, busy: u64, total: u64) -> f64 {
    let d_total = total.saturating_sub(prev_total);
    if d_total == 0 {
        return 0.0;
    }
    let d_busy = busy.saturating_sub(prev_busy);
    (d_busy as f64 / d_total as f64) * 100.0
}

/// FR-MON-05: every process in `root_pid`'s subtree (itself plus every
/// descendant), so C3 can collapse e.g. a compiler subprocess under the
/// editor that launched it.
pub fn process_tree(root_pid: u32) -> Result<ProcessTreeSnapshot, ProcError> {
    let mut entries = Vec::new();
    for proc in procfs::process::all_processes()? {
        let Ok(proc) = proc else { continue }; // process exited mid-scan; skip it
        let Ok(stat) = proc.stat() else { continue };
        entries.push((stat.pid, stat.ppid, stat.comm));
    }
    let root_known = entries.iter().any(|(pid, ..)| *pid == root_pid as i32);
    Ok(ProcessTreeSnapshot {
        root_pid,
        root_pid_known: root_known,
        processes: if root_known {
            collect_subtree(root_pid as i32, &entries)
        } else {
            Vec::new()
        },
    })
}

fn collect_subtree(root_pid: i32, entries: &[(i32, i32, String)]) -> Vec<ProcessInfo> {
    let mut children: HashMap<i32, Vec<usize>> = HashMap::new();
    for (i, (_, ppid, _)) in entries.iter().enumerate() {
        children.entry(*ppid).or_default().push(i);
    }
    let mut out = Vec::new();
    let mut stack = vec![root_pid];
    let mut visited = std::collections::HashSet::new();
    while let Some(pid) = stack.pop() {
        if !visited.insert(pid) {
            continue; // defends against a pathological ppid cycle
        }
        if let Some((p, ppid, comm)) = entries.iter().find(|(p, ..)| *p == pid) {
            out.push(ProcessInfo {
                pid: *p as u32,
                ppid: *ppid as u32,
                comm: comm.clone(),
            });
        }
        if let Some(kids) = children.get(&pid) {
            for &i in kids {
                stack.push(entries[i].0);
            }
        }
    }
    out
}

/// Best-effort PID resolution for a focused toplevel's `app_id` (see module
/// doc comment / ADR-0009). Matches against the kernel's truncated `comm`
/// (15 chars) since that's what's cheaply available for every process;
/// picks the lowest PID among matches as a stable, deterministic tie-break.
pub fn find_pid_for_app_id(app_id: &str) -> Result<Option<u32>, ProcError> {
    let candidate = app_id_binary_guess(app_id);
    // M6: nothing can match, so don't walk all of `/proc` to find that out.
    if candidate.is_empty() {
        return Ok(None);
    }
    let mut best: Option<i32> = None;
    for proc in procfs::process::all_processes()? {
        let Ok(proc) = proc else { continue };
        let Ok(stat) = proc.stat() else { continue };
        if comm_matches(&candidate, &stat.comm) && best.is_none_or(|b| stat.pid < b) {
            best = Some(stat.pid);
        }
    }
    Ok(best.map(|p| p as u32))
}

/// `org.mozilla.firefox` → `firefox`; an app_id with no dots is used as-is.
fn app_id_binary_guess(app_id: &str) -> String {
    app_id
        .rsplit('.')
        .next()
        .unwrap_or(app_id)
        .to_ascii_lowercase()
}

/// The kernel's `comm` is `TASK_COMM_LEN - 1` = 15 bytes at most, so a
/// `comm` of exactly this length is the only one that may have been
/// truncated (M6).
const COMM_TRUNCATED_LEN: usize = 15;

/// Shortest candidate worth prefix-matching. One or two characters prefix
/// a large share of every process table, which is evidence of nothing
/// (M6).
const MIN_PREFIX_LEN: usize = 3;

fn comm_matches(candidate: &str, comm: &str) -> bool {
    // M6: an empty `app_id` guesses an empty candidate, and
    // `comm.starts_with("")` is true for *every* process -- so the old
    // match picked PID 1 and FR-MON-05's process-tree snapshot walked the
    // whole system. Nothing can be identified from nothing.
    if candidate.is_empty() || comm.is_empty() {
        return false;
    }
    let comm = comm.to_ascii_lowercase();
    if comm == candidate {
        return true;
    }
    // Candidate is the stem of a longer binary name (`firefox` ->
    // `firefox-bin`).
    if candidate.len() >= MIN_PREFIX_LEN && comm.starts_with(candidate) {
        return true;
    }
    // The reverse only makes sense when the kernel actually truncated
    // `comm`; a shorter `comm` is a complete process name, and treating it
    // as a prefix of the candidate invents matches (ADR-0009's heuristic is
    // best-effort, but it should not be arbitrary).
    comm.len() == COMM_TRUNCATED_LEN && candidate.starts_with(&comm)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn cpu_percent_zero_when_no_time_elapsed() {
        assert_eq!(cpu_percent_from_ticks(10, 100, 10, 100), 0.0);
    }

    #[test]
    fn cpu_percent_matches_busy_fraction_of_delta() {
        // 50 busy ticks out of 100 total ticks elapsed = 50%
        assert!((cpu_percent_from_ticks(0, 0, 50, 100) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn app_id_binary_guess_takes_the_last_dotted_segment() {
        assert_eq!(app_id_binary_guess("org.mozilla.firefox"), "firefox");
        assert_eq!(app_id_binary_guess("cosmic-term"), "cosmic-term");
    }

    #[test]
    fn comm_matches_handles_kernel_truncation_either_direction() {
        assert!(comm_matches("firefox", "firefox"));
        assert!(comm_matches("firefox", "firefox-bin")); // candidate is the stem
        // A `comm` at the kernel's 15-byte limit is the only case where the
        // candidate may legitimately be the longer of the two.
        assert_eq!("cosmic-text-edi".len(), COMM_TRUNCATED_LEN);
        assert!(comm_matches("cosmic-text-editor", "cosmic-text-edi"));
        assert!(!comm_matches("firefox", "chromium"));
    }

    /// M6: `comm.starts_with("")` is true for every string, so an empty
    /// `app_id` matched every process on the machine; `find_pid_for_app_id`
    /// then returned the lowest PID -- PID 1 -- and FR-MON-05's process-tree
    /// snapshot rooted there walked the entire system.
    #[test]
    fn an_empty_app_id_matches_nothing() {
        assert!(!comm_matches("", "systemd"));
        assert!(!comm_matches("", "firefox"));
        assert!(!comm_matches("", ""));
        assert!(!comm_matches("firefox", ""));
        assert_eq!(
            app_id_binary_guess(""),
            "",
            "the empty app_id reaches comm_matches"
        );
    }

    /// M6: before the fix this returned `Some(1)` -- PID 1 matched because
    /// every `comm` starts with the empty string.
    #[test]
    fn an_empty_app_id_resolves_to_no_pid() {
        assert_eq!(find_pid_for_app_id("").unwrap(), None);
    }

    /// M6: the reverse direction exists only to absorb the kernel's 15-byte
    /// `comm` truncation. A `comm` shorter than that was not truncated, so
    /// accepting it as a prefix of the candidate just invents matches.
    #[test]
    fn a_short_comm_is_not_treated_as_a_truncated_one() {
        // "fire" is a real 4-char process name, not a truncation of anything.
        assert!(!comm_matches("firefox", "fire"));
        assert!(!comm_matches("cosmic-text-editor", "cosmic"));
    }

    /// M6: a one- or two-character candidate prefix-matches far too much to
    /// be evidence of anything.
    #[test]
    fn a_very_short_candidate_never_prefix_matches() {
        assert!(!comm_matches("c", "cosmic-comp"));
        assert!(!comm_matches("co", "cosmic-comp"));
        assert!(comm_matches("cos", "cosmic-comp"), "3 chars is the cutoff");
    }

    #[test]
    fn collect_subtree_gathers_every_descendant_and_ignores_unrelated_processes() {
        let entries = vec![
            (1, 0, "init".to_string()),
            (100, 1, "editor".to_string()),
            (101, 100, "build.sh".to_string()),
            (102, 101, "cc1".to_string()),
            (200, 1, "unrelated".to_string()),
        ];
        let mut pids: Vec<u32> = collect_subtree(100, &entries)
            .into_iter()
            .map(|p| p.pid)
            .collect();
        pids.sort_unstable();
        assert_eq!(pids, vec![100, 101, 102]);
    }

    #[test]
    fn collect_subtree_survives_a_ppid_cycle() {
        // pathological input (should never occur from real /proc) must not hang
        let entries = vec![(1, 2, "a".to_string()), (2, 1, "b".to_string())];
        let pids: Vec<u32> = collect_subtree(1, &entries)
            .into_iter()
            .map(|p| p.pid)
            .collect();
        assert_eq!(pids.len(), 2);
    }

    /// Live proof: matches this test process's own PID via its comm.
    #[test]
    fn find_pid_for_app_id_resolves_a_real_running_process() {
        let my_comm = procfs::process::Process::myself()
            .unwrap()
            .stat()
            .unwrap()
            .comm;
        let found = find_pid_for_app_id(&my_comm).unwrap();
        assert!(found.is_some());
    }

    #[test]
    fn process_tree_of_this_test_process_includes_itself() {
        let my_pid = std::process::id();
        let snapshot = process_tree(my_pid).unwrap();
        assert!(snapshot.root_pid_known);
        assert!(snapshot.processes.iter().any(|p| p.pid == my_pid));
    }

    #[test]
    fn process_tree_of_unknown_pid_is_empty_but_not_an_error() {
        let snapshot = process_tree(u32::MAX).unwrap();
        assert!(!snapshot.root_pid_known);
        assert!(snapshot.processes.is_empty());
    }
}

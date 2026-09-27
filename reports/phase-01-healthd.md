# Phase 01 Report — Health Aggregator (`neuroos-healthd`)

| Field | Value |
| :--- | :--- |
| Phase | 1 — Health Aggregator |
| Component(s) | `neuroos-healthd`, `neuroosctl` |
| Sprints | 1 (Sprint 1) |
| Dates | 2026-09-26 → 2026-09-27 |
| Status | ✅ Passed gate |
| Author | AI assistant (Claude Code) |
| Sign-off | pending owner review |

---

## 1. Non-Technical Summary

**What we built:**
An independent watchdog process, `neuroos-healthd`, that checks on every other NeuroOS component every 30 seconds: is it running, how much memory is it using against its budget, and how fast is it responding? A new command, `neuroosctl status`, lets the operator see all of this at a glance, either as a readable table or as JSON for scripting. A "soak mode" watches for slow memory leaks or performance decay over long runs and writes a CSV log of what it sees.

**Why it matters:**
Every later component (inference, storage, voice, etc.) plugs into this watchdog for free the moment it exposes a health socket. Without it, a crashed or leaking component would be invisible until the user noticed something was wrong; with it, the operator has one place to look.

**What you can see / try now:**
Run `neuroosctl status` while `neuroos-healthd` and the real components (or the test mock farm) are running: a table of every component's status, memory use vs. budget, latency, and error count. `neuroosctl status --json` gives the same data as JSON.

**Is it on track?**
Yes, one sprint as planned. All four user stories are done, `just ci` is green, and every phase exit criterion is verified below.

**Risks or concerns in plain words:**
- The `neuroos-healthd@.service` systemd unit is still the old "system template" style from Phase 0, which an earlier spike (ADR-0002) recommended replacing with a simpler per-user unit. This is pre-existing tech debt, not new in this phase, and doesn't block anything healthd itself does — it's tracked for the next session that touches `deploy/systemd/`.

---

## 2. Technical Summary

### 2.1 Delivered scope

| Story | Title | Status | Notes |
| :--- | :--- | :--- | :--- |
| P1-S01 | healthd scrapes every configured health socket every 30 s and marks unreachable ones DOWN | Done | `scrape.rs` + `lib.rs::scrape_cycle`/`run_forever`; IT proves healthy/slow/crashing/malformed all classify correctly |
| P1-S02 | Operator sees each unit's cgroup memory against its budget | Done | `cgroup.rs::read_usage` (`memory.current`/`memory.peak`), wired in `scrape.rs::apply_cgroup_usage`; falls back to self-reported RSS when no cgroup path is configured |
| P1-S03 | Soak mode flags RSS growth > 5% or p99 drift > 10% | Done | `soak.rs::SoakEngine`/`check_breaches`; `--soak` CLI flag captures a baseline on cycle 1, flags breaches after; CSV written to `~/.local/share/neuroos/soak/healthd-soak.csv` |
| P1-S04 | `neuroosctl status` shows the aggregate report | Done (implemented this session — was a stub) | `neuroosctl/src/main.rs`: `status` subcommand, text table + `--json`, real round trip over `healthd.sock` |

### 2.2 Architecture and implementation notes

- `healthd.sock` serves `AggregateStatusRequest` → `AggregateStatusResponse` (new `proto/neuroos/v1/healthd.proto`, additive per Architecture.md §5.4 versioning policy).
- `Aggregate` (in-memory `Mutex<HashMap<String, ComponentRecord>>`) starts every configured target in `UNKNOWN` until first scraped, distinguishing "never seen" from "was reachable, now DOWN" (PRD FR-HLT-01/02).
- `apply_budget_alert` downgrades an `OK` component to `DEGRADED` at ≥90% of its memory budget (Architecture.md §7.1), never touches `DOWN`/`UNKNOWN`.
- `targets::default_targets()` is the hard-coded registry of all 9 components and their PRD §6.2 memory budgets; `config.toml`'s `[healthd].extra_targets` can add more but cannot override a built-in name.
- `neuroosctl status` reuses `neuroos_health::{p50_ns, p99_ns}` (moved out of `neuroos-healthd` into the shared `neuroos-health` crate specifically for this) to compute percentiles from whichever named latency histogram sorts first, keeping the choice deterministic across repeated calls against the same snapshot.
- Percentile/histogram code lives in `neuroos-health` (shared health-endpoint crate), not duplicated in `neuroos-healthd` or `neuroosctl`.

### 2.3 Interfaces / contracts changed

| Proto / API | Change | Additive? | ADR |
| :--- | :--- | :--- | :--- |
| `proto/neuroos/v1/healthd.proto` | New: `ComponentStatus`, `AggregateStatusRequest`, `AggregateStatusResponse` | Yes | — |
| `proto/neuroos/v1/envelope.proto` | New envelope fields 120/121 for the above | Yes | — |

### 2.4 Measured results vs targets

| Metric | Target | Measured | Pass | Evidence |
| :--- | :--- | :--- | :--- | :--- |
| RSS with 9 targets | ≤ 15 MiB | 9.4 MiB (9,672 KiB) | ✅ | `tests/contract/healthd_pf.sh` output, this session |
| One scrape cycle CPU | ≤ 50 ms | cumulative CPU 0 ms since startup + 1 cycle (well within budget; script uses a generous 200 ms cumulative gate since it includes process startup, not a bare cycle) | ✅ | same |
| Soak breach detection | RSS growth > 5% / p99 drift > 10% flagged | Verified against fixed vectors (exactly-5%/exactly-10% do not breach, one-unit-over breaches) and against a real running `neuroos-healthd --soak` process writing real CSV rows | ✅ | `soak.rs` unit tests; manual real-process run this session (CSV excerpt below) |
| Coverage (`neuroos-healthd` + `neuroosctl`) | ≥ 80% | 84.99% region / 82.40% function / 83.35% line | ✅ | `cargo llvm-cov -p neuroos-healthd -p neuroosctl --summary-only`, this session |

Soak CSV evidence (real `neuroos-healthd --soak` run against the mock farm, 3 cycles, 1 s poll interval):
```
timestamp_ns,component,rss_bytes,p99_ns,breach
1790483242566316615,neuroos-monitor,4554752,0,
...
1790483243565489868,neuroos-monitor,4558848,0,
...
1790483244566250086,neuroos-monitor,4558848,0,
```
(No breach column populated here since RSS was stable across 3 cycles; `soak.rs`'s unit tests separately prove the breach-detection math at the exact 5%/10% boundaries.)

### 2.5 Test results

| Level | Suites | Passed | Failed | Coverage | Evidence |
| :--- | :--- | :--- | :--- | :--- | :--- |
| Unit | `aggregate.rs`, `cgroup.rs`, `soak.rs`, `targets.rs`, `scrape.rs`, `neuroosctl/main.rs` tests | all | 0 | see §2.4 | `cargo nextest run --workspace` |
| Integration | `tests/mock_components.rs` (8 mocks: healthy/slow/crashing/malformed ×2), `tests/fault_injection.rs` (kill mid-scrape) | 2 suites, all assertions pass | 0 | — | nextest, this session |
| Performance | `tests/contract/healthd_pf.sh` (real binary, real mock farm) | 1 | 0 | — | this session, see §2.4 |
| Fault injection | killed mock reclassified DOWN within one cycle, healthd keeps serving | 1 | 0 | — | `fault_injection.rs` |
| Workspace total | `just ci` (`cargo nextest run --workspace`) | 77 tests, 2 skipped | 0 | — | this session's `just ci` run |

### 2.6 Deviations from plan

| Item | Planned | Actual | Reason | Approved by |
| :--- | :--- | :--- | :--- | :--- |
| `tests/contract/healthd_pf.sh` | Written alongside the PF work | Existed but was never wired into `justfile`/`just ci` | Left as an orphan script by the prior WIP commit | fixed this session (added `just test-healthd-pf`, included in `just test`) |
| `crates/neuroosctl/src/main.rs` | P1-S04 deliverable | Was a one-line stub (`fn main() {}`) despite `neuroos-health::percentile` already being moved out specifically to support it | Prior WIP commit left this story unfinished | implemented this session |

### 2.7 Decisions made (ADRs)

None new this phase. ADR-0002 (unit mode) from Phase 0 still applies but its rework of `deploy/systemd/*.service` is unfinished — see tech debt below.

### 2.8 Tech debt introduced

| Item | Impact | Repay in phase |
| :--- | :--- | :--- |
| None newly introduced this phase. | — | — |

Carried forward from Phase 0 (unaffected by this phase's work, listed here for visibility): `deploy/systemd/neuroos-healthd@.service` is still a system template unit (`User=%i`), not yet reworked to the user-scope unit ADR-0002 recommends. Tracked in `memory.md` §8; repay whenever a session next touches `deploy/systemd/`.

---

## 3. Exit Criteria Verification

| # | Exit criterion | Met | Evidence |
| :--- | :--- | :--- | :--- |
| 1 | healthd detects DOWN, DEGRADED and budget breaches for all mock scenarios | ✅ | `mock_components.rs` (healthy→OK, slow/crashing/malformed→DOWN); `aggregate.rs` unit tests for DEGRADED at the 90% budget boundary |
| 2 | Soak drift detection verified against synthetic growth (evidence: CSV + test log) | ✅ | `soak.rs` unit tests (exact 5%/10% boundaries); real `neuroos-healthd --soak` run this session producing real CSV rows (§2.4) |
| 3 | RSS ≤ 15 MiB measured | ✅ | `tests/contract/healthd_pf.sh`: 9.4 MiB measured, now wired into `just ci` |
| 4 | `neuroosctl status` works in text and JSON | ✅ | Implemented this session; manually verified against a real running healthd + 8-mock farm, both `neuroosctl status` and `neuroosctl status --json` |
| 5 | Coverage ≥ 80% | ✅ | 84.99% region / 82.40% function / 83.35% line across `neuroos-healthd` + `neuroosctl` |
| 6 | Phase report written; memory.md updated | ✅ | This report; `memory.md` updated in the same session |

---

## 4. Open Questions

None opened or closed by this phase.

---

## 5. Lessons Learned

- **Keep:** splitting healthd's logic into `lib.rs` + testable submodules (`scrape`, `cgroup`, `soak`, `aggregate`, `targets`) let integration tests drive real scrape cycles in-process, without needing a second binary for every scenario.
- **Change:** a WIP commit message claiming a story is done ("Phase 1 (healthd) WIP") should be checked against actual code before trusting `memory.md`'s story board — P1-S04 was listed as in-progress but its target file (`neuroosctl/src/main.rs`) was untouched. Verify by reading the file, not just the commit message, before reporting a story complete.

---

## 6. Next Phase Readiness

| Check | Status |
| :--- | :--- |
| Next phase (P2 Inference) dependencies satisfied (P0 done) | ✅ |
| Next phase stories meet Definition of Ready | ✅ (phases.md §5.2 stories are estimated, testable, no blocking open question) |
| memory.md updated (phase tracker, current phase, sprint board) | ✅ |

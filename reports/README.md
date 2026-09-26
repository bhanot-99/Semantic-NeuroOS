# reports/ — Phase Completion Reports

This folder holds **one report per finished phase**, each in its own file. A phase is not complete until its report exists here and the project owner has signed it off (see [phases.md](../phases.md) §1.6).

## Naming

| Phase | File |
| :--- | :--- |
| 0 | `phase-00-foundation.md` |
| 1 | `phase-01-healthd.md` |
| 2 | `phase-02-inference.md` |
| 3 | `phase-03-monitor.md` |
| 4 | `phase-04-storage.md` |
| 5 | `phase-05-knowledge.md` |
| 6 | `phase-06-voice.md` |
| 7 | `phase-07-kernel.md` |
| 8 | `phase-08-fetcher.md` |
| 9 | `phase-09-integration-soak.md` |
| 10 | `phase-10-release.md` |

Machine-readable benchmark output goes in `reports/bench/` (`<component>-<YYYYMMDD>.json`) and is linked from the phase report.

## How to write a report

1. Copy `_TEMPLATE.md` to the file name above.
2. Fill the **non-technical summary** first: plain language, no jargon, readable by someone who has never seen the code.
3. Fill the **technical summary**: what was built, measured results against targets, test results, deviations, tech debt.
4. Complete the exit criteria table with **evidence for every row** (link to test output, benchmark JSON, screenshot or log).
5. Record the owner sign-off.
6. Update [memory.md](../memory.md) §3 and §5.

Numbers in reports must be measured, never estimated (see [rules.md](../rules.md) §7.5).

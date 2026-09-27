# ADR-0009: Focused toplevel PID is resolved by a best-effort app_id → process heuristic, not a protocol

- Status: Accepted
- Date: 2026-09-27

## Context

phases.md P3-S01 ("As C3, I receive focus changes with app_id, title, PID
and UTC-ns timestamps") and FR-MON-05 ("Snapshot the process tree for the
focused window's PID") both assume C1 can obtain a toplevel's PID.

Verified live against the reference COSMIC session while building P3-S01:

- `zcosmic_toplevel_info_v1`/`zcosmic_toplevel_handle_v1` (all three protocol
  versions, per `cosmic-protocols` 0.2.0's vendored XML) expose no `pid`
  field or event — only `title`, `app_id`, `state`, `geometry`,
  `output_enter/leave`, `workspace_enter/leave`.
- `com.system76.CosmicComp`'s D-Bus interface (introspected directly:
  `busctl --user introspect com.system76.CosmicComp
  /com/system76/CosmicComp`) exposes only the standard
  `org.freedesktop.DBus.{Introspectable,Peer,Properties}` interfaces — no
  window/toplevel query method at all.

This isn't a COSMIC-specific gap: Wayland deliberately doesn't let a client
learn another client's PID through the compositor protocol (a sandboxing
boundary), and no wlroots-based compositor's foreign-toplevel protocol
exposes it either.

## Decision

Resolve a toplevel's PID with a best-effort heuristic
(`neuroos-monitor::sensors::proc::find_pid_for_app_id`): take the last
dot-separated segment of `app_id` (`org.mozilla.firefox` → `firefox`) as a
guessed binary name, scan `/proc` for a process whose kernel-truncated
`comm` matches in either prefix direction (handles the 15-byte `comm`
truncation), and pick the lowest matching PID as a deterministic tie-break
when several processes share a name.

`WindowOpened.pid_known` is `false` when no match is found (e.g. an app_id
that doesn't resemble its binary's name at all — Electron apps, some
Flatpak-sandboxed app_ids). Consumers (C3, Phase 4) must treat `pid` as
advisory, not authoritative, and must not use it as a security boundary.

## Consequences

- FR-MON-05's process-tree snapshot inherits this same uncertainty: it's
  rooted at whatever PID the heuristic returned, which may occasionally be
  wrong (misattributed to a same-named unrelated process) or absent
  entirely (`root_pid_known: false`, empty `processes`).
- Live-verified this resolves correctly for at least one real case during
  P3-S01's own testing: a real focused Brave browser window's `app_id`
  ("brave-browser") resolved to its actual running PID.
- No spec change needed elsewhere — FR-MON-01/FR-MON-05 already only ever
  claimed PID as data C1 emits, not a guarantee of its precision; this ADR
  just records that the precision is heuristic, and why.

## Alternatives considered

- **Match on window title instead of app_id.** Titles are far less stable
  (change per-tab, per-document) and have no natural relationship to a
  binary name; rejected.
- **Ask the user to run a privileged/setuid helper that queries the
  compositor's own internal client table.** Real fix, but a large privilege
  and complexity increase for a "nice to have" field that Phase 4 downgrades
  to "collapse subprocesses under it" (not safety- or taint-critical).
  Revisit only if the heuristic proves unacceptably inaccurate in the
  ≥ 8 h real-usage dumps this phase's exit criteria require.

# ADR-0011: `storage.sock` maintenance RPC for GC and backup

- Status: Proposed (needs owner approval — rules.md §10.2.4: contract change)
- Date: 2026-10-04
- Relates to: BUGS.md H12, Architecture.md §7.5, ADR-0002

## Context

Architecture.md §7.5 schedules C3's lifecycle jobs from systemd timers: a
daily GC vacuum and a 6-hourly backup, each "a systemd timer triggers a C3
RPC". The timer units (`neuroos-gc`, `neuroos-backup`) run
`neuroosctl storage gc` / `neuroosctl storage backup`, but neither command
nor any RPC for them existed, so both jobs always failed and retention was
never enforced. C3 is the only writer of user data (AB-2) and owns the
SQLite connection and LanceDB handles, so the jobs must run inside C3, not
in `neuroosctl`.

`storage.proto`'s envelope range (40–59) has only fields 58 and 59 left.

## Decision

Add one request/response pair to `proto/neuroos/v1/storage.proto`, at
envelope fields 58/59 — additive only, no existing field number or meaning
changes (Architecture.md §5.4):

```proto
message MaintenanceRequest {
  oneof job {
    GcJob gc = 1;
    BackupJob backup = 2;
  }
}
message GcJob {}
message BackupJob { uint32 keep = 1; }   // 0 = Architecture.md §7.5's 8
message MaintenanceResponse {
  uint64 entities_deleted = 1;
  uint64 focus_history_deleted = 2;
  string backup_path = 3;
}
```

One `oneof` pair rather than separate `Gc*`/`Backup*` pairs, so the two
remaining envelope fields cover every future C3 maintenance job (e.g.
re-index on demand) by adding a `oneof` member instead of new envelope
fields.

`neuroosctl` gains `storage gc`, `storage backup [--keep N]` (sending this
RPC) and `forget --app <id> | --since <duration>` (sending the existing
`ForgetRequest`, which had no CLI before — FR-PRV-03).

## Consequences

- The C3 envelope range is now full. A further C3 RPC needs either a new
  `oneof` member here or an ADR that allocates more envelope space.
- Backups are labelled with their UTC creation time (`YYYYMMDDTHHMMSSZ`),
  which sorts chronologically, as `lifecycle::prune_old_backups` requires.
- GC and backup run while holding C3's engine lock, so `storage.sock` queries
  wait for them; both are scheduled outside interactive hours (03:30, every
  6 h) and the clients use generous deadlines.

## Alternatives considered

- **Schedule the jobs inside C3 (no RPC).** Simpler on the wire, but it
  contradicts Architecture.md §7.5 and loses systemd's `Persistent=` catch-up
  of runs missed while the machine was off.
- **Separate `GcRequest`/`BackupRequest` pairs.** Needs four envelope fields;
  only two are left.

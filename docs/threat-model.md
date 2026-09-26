# Threat Model v0 (STRIDE per trust boundary)

> Status: v0, written in Phase 0 (P0-S05). Updated in Phase 7 (SafetyGate kernel)
> and Phase 8 (fetcher). Source: Architecture.md §4 (trust zones), §5 (IPC),
> §7.4 (taint), §8 (security architecture).

## 1. Trust zones (Architecture.md §4)

| Zone | Contents | Identity | Isolation |
| :--- | :--- | :--- | :--- |
| Z0 | The internet | n/a | n/a |
| Z1 (Egress DMZ) | C7 fetcher, spool dir | dedicated UID `neuroos-fetcher` | separate UID, own systemd unit, network allowed |
| Z2 (isolated user services) | C1, C2, C3, C4, C5, healthd | user UID | `PrivateNetwork=true`, Landlock, same UID as Z3 |
| Z3 (privileged decision core) | C6 kernel, skill runner, `neuroos-confirm` | user UID | strictest sandbox; only place a skill executes |

Zones 2 and 3 share a UID (Architecture.md §4 boundary rule 4); the boundary
between them is enforced by Landlock rulesets (§8.2 table), not by UID
separation. This is the weakest boundary in the system and the one most
worth re-reviewing as each component lands.

## 2. Z0 → Z1: internet → fetcher

| STRIDE | Threat | Mitigation | Status |
| :--- | :--- | :--- | :--- |
| Spoofing | Malicious server impersonates a legitimate host | TLS cert validation (default `https` only, FR-FET-04) | Phase 8 |
| Tampering | Response body tampered in transit | TLS | Phase 8 |
| Info. disclosure | SSRF: fetcher tricked into reaching internal/metadata IPs | Landlock ABI v4 net rules restrict to TCP 443/80 (§8.2); anti-SSRF checks (IP literal / redirect / DNS-rebind guards) | Phase 8 — not yet implemented |
| Denial of service | Fetcher used to exhaust local disk/CPU via huge responses | FR-FET-04: 5 MiB body cap, 15 s timeout | Phase 8 |
| Elevation of privilege | Fetched content executed instead of stored inertly | Fetcher only writes to spool as tainted `IngestRequest`-shaped payload; never executes | Phase 8 |

## 3. Z1 → Z2: fetcher → storage (spool dir)

| STRIDE | Threat | Mitigation | Status |
| :--- | :--- | :--- | :--- |
| Spoofing | Non-fetcher process writes into the spool dir | `2770 neuroos-fetcher:neuroos`, group-writable only by the two identities that need it (`deploy/sysusers.d/neuroos.conf`, `deploy/tmpfiles.d/neuroos.conf`) | P0-S05 |
| Tampering | Spool file modified after being written, before C3 ingests it | C3 parses+ingests+deletes in one pass (§7.5 spool ingest job); no window for a third party to edit (same-UID group, no other writers) | Phase 4 |
| Repudiation | No record of what was ingested from the untrusted zone | Ingested chunks carry `EXTERNAL_UNTRUSTED` taint (§7.4), permanently distinguishing fetcher-sourced data in storage | Phase 4 |
| Info. disclosure | C3 reads a spool file it can't yet validate as well-formed | Schema validation before ingest (§7.5); malformed files rejected, not partially trusted | Phase 4 |
| Denial of service | Fetcher floods the spool dir | Out of v1.0 scope (personal single-user install, OQ-06); revisit if R0-5/multi-tenant ever applies | Watching |
| Elevation of privilege | Untrusted content changes SafetyGate tier decisions | Taint propagation rule (§7.4): `EXTERNAL_UNTRUSTED` can only raise `SAFE` to `REVIEW`, never lower a tier | P0-S03/S04 (taint flags exist); enforced in Phase 7 |

## 4. Z2 internal: IPC between C1/C2/C3/C4/C5/healthd

| STRIDE | Threat | Mitigation | Status |
| :--- | :--- | :--- | :--- |
| Spoofing | A non-neuroos local process connects to a component's socket | `SO_PEERCRED` checked against a per-socket UID allowlist on every accept (`neuroos-ipc::peercred`) | P0-S03, done |
| Spoofing | Abstract-namespace socket collision (shared across netns) | Filesystem-path sockets only, never abstract namespace (Architecture.md §5.1) | P0-S03, done |
| Tampering | Frame injected mid-stream, or a frame lies about its own length | u32-LE length-prefixed framing rejects oversized/truncated frames (`neuroos-ipc::framing`, 100% region coverage) | P0-S03, done |
| Info. disclosure | DNS queries leak component activity/timing to the network despite `PrivateNetwork=true` | **Found in P0-S05** (`scripts/check-egress.sh`): `PrivateNetwork=true` alone does not block DNS, because glibc's `resolve` NSS module talks to `systemd-resolved` over a local socket (`/run/systemd/resolve/`), not the network. Fixed with `InaccessiblePaths=-/run/systemd/resolve` on every Z2/Z3 unit except the fetcher (Architecture.md §8.1 addendum). | P0-S05, done |
| Denial of service | A component hangs a peer by never responding | Read/write/connect deadlines (`neuroos-ipc::deadline`, `tokio::time::timeout`) | P0-S03, done |
| Denial of service | A crashed server leaves clients unable to reconnect | Reconnect-with-backoff, 10 s budget (`neuroos-ipc::client::connect_with_reconnect`) | P0-S03, done |
| Elevation of privilege | One component reads another's private files under shared-UID sandboxing | Per-component Landlock rulesets (§8.2 table); `hmac.key` readable only by C6 | Phase 7 for C6; other components' rulesets land with their own phase |

## 5. Z2/Z3 boundary: C5 → C6 (capability dispatch)

| STRIDE | Threat | Mitigation | Status |
| :--- | :--- | :--- | :--- |
| Spoofing | Something other than C5 asks C6 to run a capability | `SO_PEERCRED` on `kernel.sock` (same mechanism as §4 above) | P0-S03 mechanism done; Phase 7 wiring |
| Tampering | Audit log (`actions.jsonl`) edited after the fact | Hash-chained audit log (§7.1) | Phase 7 |
| Repudiation | No durable record of what a skill did and why it was approved | `actions.jsonl`, rotated at 10 MB, hash-chained | Phase 7 |
| Info. disclosure | Tainted (externally-sourced) data silently used to justify a DANGEROUS action | `evaluate_tier` (§8.3): `EXTERNAL_UNTRUSTED` can only raise a tier, never lower one; DANGEROUS+tainted always requires human confirmation | Phase 7 |
| Denial of service | Skill runner hangs, blocking the kernel | Per-skill Landlock + seccomp ruleset, narrower than C6's own (§8.2) | Phase 7 |
| Elevation of privilege | A REVIEW/DANGEROUS capability runs without the confirmation dialog actually being shown | `neuroos-confirm` is the only path to a human confirmation; C6 refuses to proceed without a dialog response for tiers above SAFE | Phase 7 |

## 6. Cross-cutting: systemd hardening baseline (Architecture.md §8.1)

Applies to every unit except `neuroos-fetcher.service` (network-exempt) and
with per-component overrides noted inline in each `deploy/systemd/*.service`
file (`RestrictRealtime=false` for C2, `MemoryDenyWriteExecute=false` for
C2/C4 pending Phase 2/6 verification).

| Control | Threat it addresses |
| :--- | :--- |
| `PrivateNetwork=true` + `InaccessiblePaths=-/run/systemd/resolve` | Network exfiltration and DNS-based side channels from Z2/Z3 |
| `NoNewPrivileges=true`, `RestrictSUIDSGID=true`, `CapabilityBoundingSet=` | Privilege escalation via setuid binaries or capabilities |
| `ProtectSystem=strict`, `ProtectHome=tmpfs` + explicit `BindPaths=`/`BindReadOnlyPaths=` | Reading/writing files outside each component's declared need |
| `RestrictNamespaces=true`, `LockPersonality=true` | Sandbox escape via nested namespaces or `personality()` |
| `MemoryDenyWriteExecute=true` (where compatible) | Code injection via a writable+executable mapping |
| `SystemCallFilter=@system-service` | Reduces kernel attack surface to the syscalls a service actually needs |
| `MemoryMax=` (per PRD §6.2) | One component's leak/runaway allocation cannot exhaust the whole 2,340 MiB budget |

## 7. Open items for later phases

- Real cross-UID `SO_PEERCRED` rejection has not been exercised on real
  hardware (needs a second local user or root; noted in memory.md tech
  debt). The allow/deny decision function is unit-tested in isolation.
- Landlock rulesets (§8.2) are documented but not yet implemented in any
  component; each lands with that component's own phase.
- SSRF guard (`ssrf.rs`) is a stub; real anti-SSRF logic is Phase 8.
- HMAC key generation/rotation procedure is not yet designed (Phase 7).
- Unit-mode choice (system template units vs. user units, §8.1 note) is
  confirmed by spike S-01 / ADR-0002 (P0-S06).

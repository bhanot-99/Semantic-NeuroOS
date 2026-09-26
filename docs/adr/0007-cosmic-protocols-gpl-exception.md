# ADR-0007: `cosmic-protocols` GPL-3.0-only exception

- Status: Accepted
- Date: 2026-09-26

## Context

rules.md §4's licence allowlist (mirrored in `deny.toml`) is MIT,
Apache-2.0, BSD-2/3-Clause, ISC, Zlib, Unicode-3.0, MPL-2.0; GPL/LGPL/AGPL
and non-commercial licences need an ADR. Two such exceptions were already
known and pre-acknowledged (PRD R-06/R-07): `espeak-ng` (GPL-3.0, a system
package, not a Rust crate) and the openWakeWord models (CC-BY-NC-SA-4.0, a
model licence, not a code licence).

Wiring `cargo deny check` for real into `just ci` (P0-S10) surfaced a third:
`cosmic-protocols` (used by `crates/neuroos-monitor` for `zcosmic_toplevel_
info_v1`, spike S-03, P0-S08) is `license = "GPL-3.0-only"` in its own
`Cargo.toml` — confirmed directly from the crate source, not just
metadata. This wasn't caught in P0-S08 because `cargo deny check` wasn't
in `just ci` yet at that point (P0-S01 deliberately deferred full `deny`
wiring to this story).

## Decision

Add a `[[licenses.exceptions]]` entry in `deny.toml` allowing GPL-3.0-only
specifically for `cosmic-protocols`, with a comment pointing here.
`cosmic-protocols` is the *only* way to reach `zcosmic_toplevel_info_v1`
(Architecture.md FR-MON-01 names this protocol specifically; there is no
non-GPL binding for it, since COSMIC's own protocol-bindings crate chose
GPL-3.0), so there is no alternative that avoids this.

This project is a personal, single-user install (OQ-06 default) and is
not currently distributed to third parties, so GPL-3.0's copyleft
source-disclosure obligation is not triggered by ordinary use. If
distribution posture ever changes (OQ-06 is revisited), this ADR must be
revisited too — GPL-3.0 linked into a distributed binary is a real
constraint on the whole binary's licence, not just this one crate's.

## Consequences

- `neuroos-monitor`'s binary, once built, is subject to GPL-3.0's terms
  through this one dependency. Fine for personal use; a blocker if this
  project is ever distributed without addressing it (e.g., relicensing,
  isolating the COSMIC-specific code into a separate GPL-licensed
  component, or dropping COSMIC-specific support).
- `cargo deny check` now passes cleanly (`licenses ok`) with this single,
  explicit, documented exception rather than a blanket GPL allowance.

## Alternatives considered

- **Drop `zcosmic_toplevel_info_v1`, use only `wlr-foreign-toplevel-
  management`** (the non-COSMIC-specific wlroots protocol, MIT-licensed
  bindings via `wayland-protocols-wlr`): rejected — COSMIC (this machine's
  compositor) does not implement the wlr protocol; Architecture.md
  specifically names the COSMIC protocol for this reason.
- **Vendor just the generated protocol code instead of depending on the
  crate**: technically avoids a `Cargo.lock` dependency edge, but the
  vendored code is still GPL-3.0 licensed and still ends up in the
  binary — doesn't change the actual legal situation, just hides it from
  `cargo deny`. Rejected as worse, not better.

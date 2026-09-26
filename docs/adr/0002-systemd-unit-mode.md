# ADR-0002: systemd unit mode (spike S-01, risk R-01)

- Status: Accepted
- Date: 2026-09-26

## Context

Architecture.md §8.1 stated a preference for **system template units**
(`neuroos-<component>@<user>.service`, `User=%i`) over user units, reasoning
that `PrivateNetwork=true` in *user* units needs user namespaces (assumed
restricted on Ubuntu 24.04) and that `PrivateUsers=` remaps UIDs, breaking
`SO_PEERCRED`. Risk R-01 flagged this as unverified. P0-S05 built all
`deploy/systemd/*` unit templates on the system-template-unit assumption
before this spike ran, so this ADR also decides whether that work needs
reworking.

## Spike method

On the reference machine (Pop!_OS 24.04 LTS, COSMIC/Wayland, kernel
7.1.5-76070105-generic — see memory.md §10), tested both candidates for:
(a) whether `PrivateNetwork=true` works at all, (b) whether the Wayland,
D-Bus and PipeWire sockets are reachable, (c) whether `SO_PEERCRED` reports
the real UID.

**Candidate A — user unit** (`systemd-run --user -p PrivateNetwork=yes`):
```
$ systemd-run --user -p PrivateNetwork=yes --wait --pipe -- bash -c 'ip link show | head -3; id'
1: lo: <LOOPBACK,UP,LOWER_UP> ...
uid=1000(bhanot) gid=1000(bhanot) groups=1000(bhanot),65534(nogroup)
```
`PrivateNetwork=true` succeeded outright — no user-namespace restriction
error. `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY`, `DBUS_SESSION_BUS_ADDRESS` were
inherited automatically from the login session (no manual `Environment=`
needed); all three sockets (`$XDG_RUNTIME_DIR/{wayland-1,bus,pipewire-0}`)
were reachable. A real `SO_PEERCRED` check (UDS server run inside the unit,
client connecting from the unsandboxed host session) reported
`uid=1000 gid=1000` — the real UID, unchanged.

**Candidate B — system-scope transient unit simulating `User=%i`**
(`sudo systemd-run --uid=1000 --gid=1000 -p PrivateNetwork=yes -p Environment=XDG_RUNTIME_DIR=/run/user/1000`),
run by the machine owner (needs root, which this session doesn't have):
```
UID=1000
WAYLAND_OK
DBUS_OK
PIPEWIRE_OK
1: lo: <LOOPBACK,UP,LOWER_UP> ...
```
Also succeeded — but only once `XDG_RUNTIME_DIR` was passed explicitly via
`Environment=`. `WAYLAND_DISPLAY` was not set at all; the socket path had to
be hardcoded (`wayland-1`) in the test rather than discovered, because
system units don't inherit the compositor-exported session environment the
way user units do.

## Decision

**Use user-scope units** (`~/.config/systemd/user/` or
`/usr/lib/systemd/user/`, managed via `systemctl --user`), not system
template units with `User=%i`, for every Zone 2/3 component (C1–C6,
`neuroos-confirm`, healthd). This reverses Architecture.md §8.1's stated
preference.

Reasoning: both candidates work on the reference machine — R-01's premise
(user namespaces restricted on Ubuntu 24.04) does not hold here. Given both
work, user units are simpler and more robust: `XDG_RUNTIME_DIR`,
`WAYLAND_DISPLAY` and `DBUS_SESSION_BUS_ADDRESS` are inherited automatically
and correctly (including `WAYLAND_DISPLAY`'s actual value, which varies by
session and a system unit has no reliable way to discover), removing a
whole class of "component can't find the compositor socket" failure modes.
This project is a personal, single-user install (OQ-06 default), so a user
unit's natural lifecycle — starts on login, stops on logout — matches every
Zone 2/3 component's real dependency on an active desktop session (C1
watches the compositor, C2 needs PipeWire, etc.); none of them are meant to
run without a logged-in session anyway, so user-unit lifecycle is not a
practical limitation here.

`neuroos-fetcher` (Zone 1, dedicated UID, network-exempt) is unaffected: it
was never a template-unit candidate and stays a system unit under its own
`neuroos-fetcher` user, per Architecture.md §4/§8.1.

## Consequences

- `deploy/systemd/*.service` (built in P0-S05 as system templates,
  `User=%i`) need reworking to user-unit form: drop `User=`/`Group=`, drop
  the `neuroos@%i.target`/`PartOf=`/`After=` instance-parameter plumbing,
  install target is `~/.config/systemd/user/` instead of
  `/etc/systemd/system/`, `WantedBy=` targets a user target
  (`default.target` or a `neuroos.target` pulled in at login) instead of
  `multi-user.target`. **Not done in this session** — tracked as tech debt
  (memory.md §8) and left for the next session touching `deploy/systemd/`,
  since it is a mechanical rename/restructure of already-correct hardening
  content, not new design work.
- `neuroos-fetcher.service` needs no change.
- `scripts/install.sh` (not yet written) installs to the user unit
  directory and runs `systemctl --user enable`, not `systemctl enable`.
- The `SO_PEERCRED` allowlist for each socket can stay a single real UID
  (no `PrivateUsers=`-style remapping to account for).

## Alternatives considered

- **Keep system template units** (Architecture.md's original preference):
  rejected — works, but strictly more complex (explicit env wiring, no
  reliable way to discover `WAYLAND_DISPLAY`) for no measured benefit on
  this machine.
- **Test on a second, differently-configured machine** before deciding:
  not done — out of scope for a time-boxed spike; if a future deployment
  target turns out to restrict unprivileged user namespaces after all, this
  ADR's assumption should be re-tested there and superseded.

# ADR-0003: IPC transport — filesystem UDS + length-prefixed protobuf

- Status: Accepted
- Date: 2026-09-26 (decision D-04, memory.md §6)

## Context

Every component needs to talk to several others (Architecture.md §5.2
socket map) across three languages (Rust, C++, Python), under
`PrivateNetwork=true` and `PrivateDevices`/`PrivateTmp` sandboxing
(Architecture.md §8.1), and needs a peer-authentication mechanism that
survives that sandboxing.

## Decision

Unix domain sockets bound to filesystem paths only (never the abstract
namespace — abstract sockets are bound to the network namespace and would
break under `PrivateNetwork=true`), framed as a `u32` little-endian length
prefix followed by a protobuf-encoded `Envelope` (max 4 MiB, configurable
lower per socket). Every server calls `getsockopt(SO_PEERCRED)` on accept
and checks the peer UID/GID against a per-socket allowlist. Bulk token
streaming (C4 → C2) is the one exception: it uses a `memfd` shared-memory
seqlock ring passed via `SCM_RIGHTS` over `inference.sock`, not this framing
(Architecture.md §5.5; proven separately by spike S-02, P0-S07).

Implemented in `crates/neuroos-ipc` (P0-S03): `framing` (encode/decode, max
frame enforcement), `peercred` (the allow/deny decision), `server`
(`UdsServer::bind`/`accept`), `client` (deadline connect, reconnect with
backoff), `deadline` (timeout-wrapped read/write). One contract
(`proto/neuroos/v1/*.proto`) generates bindings for all three languages
(P0-S02); every message round-trips Rust → C++ → Python → Rust
byte-identically (`tests/contract/roundtrip.sh`).

## Consequences

- A single, small (`neuroos-ipc`, `libneuroos`, a Python equivalent when
  C5b needs to speak this protocol directly rather than through
  higher-level RPCs) implementation of framing/auth/deadlines/reconnect is
  shared by every component, instead of each one reinventing it.
- Socket paths live under `$XDG_RUNTIME_DIR/neuroos/` (mode 0700) for Zone
  2/3 components, `/run/neuroos-fetcher/` for C7 (Architecture.md §5.2).
- Contract changes are additive-only after Phase 0 (Architecture.md §5.4);
  a breaking change needs `neuroos.v2` and its own ADR.

## Alternatives considered

- **gRPC / HTTP**: rejected — pulls in a network stack and TLS machinery
  that conflicts with `PrivateNetwork=true`'s purpose, and is unnecessary
  for same-host IPC.
- **Abstract-namespace UDS**: rejected — bound to the network namespace,
  breaks under `PrivateNetwork=true` (Architecture.md §5.1).
- **A single shared memfd ring for everything, not just C4→C2 tokens**:
  rejected — most traffic here is request/response, not a hot streaming
  path; the ring's single-writer/single-reader seqlock design doesn't fit
  that shape.

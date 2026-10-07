# rules.md — Engineering, Quality and AI Rules

**Document version:** 1.0 · **Date:** 2026-09-26 · **Status:** Binding
**Applies to:** every human and every AI assistant that writes, reviews or changes code in this repository.

These rules are **mandatory**. "MUST / MUST NOT / SHOULD / MAY" follow RFC 2119. When a rule blocks you, stop and ask; do not work around it. A rule changes only through an ADR in `docs/adr/` and an update to this file.

---

## 1. Rule Zero — The Non-Negotiables

| # | Rule |
| :--- | :--- |
| R0-1 | **No network code outside C7.** No HTTP client, DNS resolver, socket to an IP address, or telemetry SDK in any other component, library or test helper. |
| R0-2 | **Never weaken a sandbox to make something work.** Do not remove `PrivateNetwork=true`, Landlock rules, seccomp filters or `SO_PEERCRED` checks. Fix the design instead, and record the exception in an ADR if one is truly needed. |
| R0-3 | **Taint is never cleared and tiers are never lowered.** No code path may remove `EXTERNAL_UNTRUSTED` from derived data or lower a skill's tier. |
| R0-4 | **Every skill execution is audited.** No skill runs without an `ATTEMPT` record written first and a `RESULT` record written after, including on error. |
| R0-5 | **No runtime downloads.** Services never fetch models, updates or data. Only the operator script `scripts/fetch-models.sh` downloads, and it verifies SHA-256. |
| R0-6 | **No personal content in logs** at `info` level or above: no window titles, transcripts, prompts, evidence text or URLs with query strings. |
| R0-7 | **Contracts first.** No component talks to another except through the `proto/neuroos/v1` messages. |
| R0-8 | **One phase at a time.** Work only on the current phase in [memory.md](memory.md). Do not start the next phase before the current exit criteria pass. |

---

## 2. Process Rules (Agile)

1. Every change traces to a user story ID from [phases.md](phases.md) (e.g. `P4-S03`) and, where applicable, a requirement ID from [PRD.md](PRD.md) (e.g. `FR-STO-05`).
2. A story is **Ready** only when it meets the Definition of Ready ([phases.md](phases.md) §1.4). A story is **Done** only when it meets the Definition of Done (§1.5).
3. Branch per story: `p<phase>/<story-id>-<slug>`, e.g. `p4/s03-hybrid-query`. `main` is always green.
4. Commits follow Conventional Commits: `feat(storage): add hybrid query (P4-S03)`.
5. Each phase ends with a phase report in `reports/` (template `reports/_TEMPLATE.md`) and a [memory.md](memory.md) update. A phase without a report is not finished.
6. Decisions that change architecture, contracts, security posture, a dependency, or a budget need an ADR (`docs/adr/NNNN-title.md`: Context, Decision, Consequences, Alternatives).

---

## 3. Architecture Boundaries

| Rule | Detail |
| :--- | :--- |
| AB-1 | Components MUST NOT link each other's crates or libraries. Shared code lives only in `neuroos-common`, `neuroos-proto`, `neuroos-ipc`, `neuroos-shm`, `neuroos-taint`, `neuroos-health`, `neuroos-sandbox` (Rust) and `cpp/libneuroos` (C++). **Scope (C9):** this is about what a shipped binary links. A `[dev-dependencies]` entry on another component, used only by a test binary to drive that component for real instead of through a mock, is permitted and MUST carry a comment in the manifest naming the tests that need it. A component's `src/` may never `use` another component's crate. |
| AB-2 | C3 is the **only** writer of persistent user data. C5b writes graph updates through `storage.sock`, not to SQLite directly. |
| AB-3 | C3 is the **only** component that loads the embedding model. C4 is the **only** component that loads the LLM. |
| AB-4 | C6 is the **only** component that holds `hmac.key`, writes `actions.jsonl`, or talks to C7. |
| AB-5 | C1 emits raw events. Its only filtering is pause state and the privacy exclusion list (FR-MON-08). All other filtering belongs in C3. |
| AB-6 | Unix sockets MUST be filesystem paths under the runtime directories in [Architecture.md](Architecture.md) §5.2. Abstract sockets and TCP/UDP loopback are forbidden. |
| AB-7 | Shared memory MUST use memfd passed by `SCM_RIGHTS`. Named POSIX SHM (`/dev/shm`) is forbidden. |
| AB-8 | Every timestamp crossing a boundary is `u64` UTC epoch nanoseconds. Convert at the sensor edge; never pass local time. |
| AB-9 | Every IPC server MUST check `SO_PEERCRED` against a per-socket allowlist. |
| AB-10 | Clients MUST tolerate a missing server: reconnect with jittered exponential backoff (100 ms → 10 s cap) and report a degraded state. |
| AB-11 | Hot paths MUST NOT block on slow work. Distillation, graph learning, re-indexing, backups and GC are asynchronous. |

---

## 4. Libraries and Dependencies

### 4.1 Policy

- Use only the libraries listed in [Architecture.md](Architecture.md) §12. Adding a dependency needs a justification in the PR and, if it is not a small dev-only tool, an ADR.
- Every dependency is pinned (`Cargo.lock`, `uv.lock`, submodule commit, tarball SHA-256).
- Licence allowlist (`deny.toml`): MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, Unicode-3.0, MPL-2.0. GPL/LGPL/AGPL and non-commercial licences need an ADR (known: espeak-ng GPL-3.0, openWakeWord models CC BY-NC-SA — see PRD R-06/R-07).
- `cargo-deny` bans `reqwest`, `hyper`, `hickory-*`, `ureq`, `isahc`, `surf`, `curl` and `openssl` everywhere except `crates/neuroos-fetcher`.
- `cargo deny check` (all four checks: `advisories`, `bans`, `licenses`, `sources`) MUST pass in
  CI with zero unignored advisories. It reads the same RustSec database as `cargo audit`, so the
  separate `cargo audit` run it used to require added nothing and was never installed; `just deny`
  is the single gate. Every entry in `deny.toml`'s `advisories.ignore` carries a written reason
  and a revisit condition.

### 4.2 Use / Avoid table

| Area | USE | AVOID |
| :--- | :--- | :--- |
| Rust async | `tokio` | `async-std`, `smol`, mixing runtimes, `block_on` inside async |
| Rust errors | `thiserror` enums in libraries; `anyhow` only in `main.rs` | `Box<dyn Error>` in public APIs, string errors, `panic!` for control flow |
| Rust syscalls | `rustix` | raw `libc` calls unless `rustix` lacks the call (then wrap in one module with a `// SAFETY:` comment) |
| Rust serialisation | `prost` on the wire, `serde` for config/JSONL | `bincode` or ad-hoc formats on the wire |
| Rust logging | `tracing` | `println!`/`eprintln!` in services, `log` crate directly |
| Rust time | `jiff`, `std::time::Instant` for durations | `chrono`, `SystemTime` arithmetic for latency |
| Concurrency | channels (`tokio::sync::mpsc`), actors per resource | `Arc<Mutex<_>>` held across `.await`; global mutable state |
| C++ errors | `tl::expected<T, Error>` returns | exceptions across module/ABI boundaries; error codes as bare `int` |
| C++ memory | RAII, `std::unique_ptr`, `std::span`, `std::string_view` | raw `new`/`delete`, owning raw pointers, `malloc` outside third-party glue |
| C++ logging | `spdlog` | `std::cout` in services |
| C++ threads | `std::jthread`, lock-free SPSC for audio | locks or allocation in the real-time audio callback |
| Python | type hints everywhere, `dataclasses`, NumPy/SciPy vectorised code | PyTorch/TensorFlow, `pickle` for IPC, untyped dicts across functions, global state |
| Storage | parameterised SQL (`rusqlite` params), migrations in `crates/neuroos-storage/src/migrations/` | string-built SQL, schema changes outside migrations |
| Config | one `config.toml`, validated at start, fail fast on unknown keys | env vars for behaviour (only `NEUROOS_LOG` and `NEUROOS_CONFIG` allowed) |
| Crypto | `hmac` + `sha2`, `subtle::ConstantTimeEq`, `getrandom`, `zeroize` | hand-rolled crypto, `==` on MACs, `rand::thread_rng` for secrets |
| UI | GTK4 for `neuroos-confirm`; vendored D3 + fonts inlined in `graph_view.html` | CDNs, web fonts from the internet, Electron, any remote asset |
| Testing | `cargo nextest`, `proptest`, `insta`, GoogleTest, `pytest`+`hypothesis` | tests that need the internet; sleeping to wait (use events/timeouts) |

---

## 5. Error Handling

1. **No `unwrap()`, `expect()`, `panic!`, `unreachable!` or `todo!` in non-test Rust code.** Clippy enforces `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`, `clippy::todo`. The only exception is a startup invariant in `main.rs` that is documented and covered by a test.
2. Every library crate defines its own `Error` enum with `thiserror`. Variants describe *what failed*, not *where*.
3. Errors crossing IPC become `neuroos.v1.Error{code, message, retryable}`. `message` MUST NOT contain user content.
4. Classify every failure as **retryable** (timeouts, peer restarting) or **fatal** (bad config, model hash mismatch, sandbox setup failure). Retryable errors are retried with backoff by the caller. Fatal errors exit with a non-zero code so systemd restarts or stops the unit.
5. **Fail closed in security code.** If tier evaluation, taint lookup, HMAC verification, SSRF validation or Landlock setup fails for any reason, the action is denied and audited as `DENIED_POLICY`.
6. **Fail soft in user-experience code.** If C4 or C3 is unavailable, C2 plays a short pre-rendered apology clip in the normal voice and records the degraded state; it never hangs silently.
7. Every IPC request has a deadline. Defaults: control calls 250 ms, C3 queries 100 ms, C6 checks 50 ms (confirmation excluded), C4 generation 30 s, C7 fetch 15 s.
8. C++: return `tl::expected`. Wrap third-party code that throws at the boundary with `try/catch` and convert to `Error`. The real-time audio callback MUST NOT throw, allocate, lock or log.
9. Python: raise specific exceptions internally; the IPC layer converts them to `Error`. A job failure is logged and retried on the next cycle; it never crashes the worker loop.

---

## 6. Code Quality Standards

| Area | Standard |
| :--- | :--- |
| Formatting | `rustfmt`, `clang-format` (config in repo), `ruff format`. CI rejects unformatted code. |
| Linting | `clippy --all-targets -- -D warnings` (plus the lints in §5), `clang-tidy` (bugprone, cert, cppcoreguidelines, performance), `ruff`, `mypy --strict`, `shellcheck`. |
| Warnings | Treated as errors in every language. |
| `unsafe` | Allowed only in `neuroos-shm`, `neuroos-sandbox` and FFI shims. Every block has a `// SAFETY:` comment and a test. `#![deny(unsafe_code)]` everywhere else. |
| Functions | Prefer ≤ 60 lines and ≤ 5 parameters; split or introduce a struct when larger. |
| Comments | Explain *why*, not *what*. Public items have doc comments. Every performance-critical or security-critical function names the requirement it satisfies (e.g. `// FR-KER-03`). |
| Naming | Rust/Python `snake_case`, types `PascalCase`; C++ `snake_case` functions, `PascalCase` types, `kConstant`. Binaries and units `neuroos-<name>`. |
| Magic numbers | Budgets and thresholds live in config or named constants that cite their source (PRD/Architecture section). |

---

## 7. Testing Rules

1. **Test pyramid per component:** unit → contract → component integration → performance → security → fault injection → mini-soak. [phases.md](phases.md) defines each phase's test set.
2. Coverage gates (NFR-QA-01): Rust libraries ≥ 80% lines, C++/Python ≥ 70%, and **100% branch coverage** for taint propagation, tier evaluation, HMAC verification, SSRF validation and the ingest filter.
3. Every bug fix starts with a failing test that reproduces it.
4. No test may use the internet. C7 tests use a local test server inside a private network namespace.
5. Performance claims need a benchmark in `bench/` with JSON output in `reports/bench/`. **Never state a latency or memory number that was not measured.** Mark unmeasured numbers as "target".
6. Tests are deterministic: fixed seeds, fake clocks (`tokio::time::pause`), recorded fixtures. Flaky tests are fixed or quarantined within one sprint, never ignored silently.
7. Fixtures containing real telemetry MUST be anonymised (titles/paths hashed or replaced) before commit.

---

## 8. Security Practices

- Threat model (`docs/threat-model.md`, STRIDE per trust boundary) is updated whenever a boundary changes.
- Parse all external or cross-component input defensively: size limits on every frame and field, schema validation on spool files, UTF-8 validation on text.
- Secrets: `hmac.key` is 32 random bytes, created at install with mode 0600, loaded into `Zeroizing<[u8; 32]>`, never logged, never sent over IPC.
- Run `systemd-analyze security neuroos-<c>@$USER.service` in Phase 9; every unit MUST score "OK" or better (exposure ≤ 4.0), except C2/C7 with documented exceptions.
- Prompt-injection corpus (`tests/security/injection/`) runs in CI against C5 + C6 from Phase 7 onward.
- SSRF corpus (`tests/security/ssrf/`) runs in CI against C7 from Phase 8 onward.

---

## 9. Performance and Resource Practices

- Respect the budgets in [PRD.md](PRD.md) §6. A change that adds ≥ 5% to a component's p99 latency or RSS needs a benchmark comparison in the PR.
- No allocation in the audio callback; pre-allocate buffers.
- Load models once, at startup, via `mmap` where the runtime supports it.
- Bound every queue and cache (size + eviction policy). Unbounded growth is a soak failure.

---

## 10. Rules for the AI Assistant (vibe-coding boundaries)

### 10.1 Always

1. **Read [memory.md](memory.md) first** at the start of every session, then the current phase section of [phases.md](phases.md).
2. **Update [memory.md](memory.md)** at the end of every work session, after every completed story, whenever the "currently working on" file changes, and whenever a decision, blocker or open question appears. This is mandatory.
3. Work on **one story at a time**, in the current phase only. State the story ID before starting.
4. Write or update tests in the same change as the code. Run them and report real results, including failures.
5. Cite the requirement IDs a change satisfies.
6. Keep changes minimal and focused. Match the surrounding code style.
7. When the blueprint, PRD and Architecture disagree, follow **Architecture.md §13**, then PRD, then the V3.2 blueprint, then V2.7. Flag the conflict in memory.md.
8. Write the phase report in `reports/` when a phase's exit criteria all pass.

### 10.2 Never

1. Never add network access, telemetry, analytics, crash reporters or auto-updaters to any component other than C7.
2. Never disable, skip or weaken a test, lint, sandbox stanza or security check to make CI pass.
3. Never invent benchmark results, test outcomes, API behaviour or library functions. If unsure a library API exists, check the docs or source first.
4. Never change a `.proto` contract field number or meaning, a budget, a tier, or a retention period without an ADR approved by the user.
5. Never introduce a dependency outside the allowlist without asking.
6. Never commit models, secrets, real user telemetry, audio recordings or `hmac.key`.
7. Never run destructive commands on user data (`~/.local/share/neuroos`, backups) without explicit confirmation.
8. Never start work on a future phase "to save time".
9. Never mark a story or phase Done while any exit criterion is unmet. Report the gap instead.

### 10.3 Stop and ask the user when

- A requirement is ambiguous or two documents conflict in a way §13 of Architecture.md does not settle.
- A budget (latency, RSS) cannot be met after reasonable optimisation.
- A spike result invalidates an architectural assumption.
- A task needs root privileges, a system package install, or a change outside the repository.
- A licence concern appears.
- An open question from [PRD.md](PRD.md) §12 blocks the current story.

---

## 11. Documentation Rules

- The six root documents are the source of truth. Update them in the same PR as the change they describe.
- Each component has a `README.md` covering purpose, sockets, config keys, sandbox paths, and how to run it alone.
- Each finished phase has a report in `reports/` (see `reports/README.md`).
- Runbooks in `docs/runbooks/` for install, upgrade, backup restore, and "component won't start".

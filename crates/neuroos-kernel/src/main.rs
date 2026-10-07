// STUB: C6 SafetyGate kernel entry point. Phase 7.
// Intentionally empty until then -- not lost work (BUGS.md D5).

// C1 / rules.md §6: `unsafe` is allowed only in neuroos-shm,
// neuroos-sandbox and FFI shims. This enforces that.
#![deny(unsafe_code)]

// SafetyGate kernel entry point

fn main() {}

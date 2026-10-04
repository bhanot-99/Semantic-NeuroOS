//! M16: `Embedder::load` used to write `ORT_DYLIB_PATH` itself, with a
//! SAFETY comment claiming it ran "single-threaded at startup, before any
//! embed() call races this". That was false: `reindex_family` loads an
//! embedder from a spawned task, so the write happened while the runtime's
//! worker threads, the health server and the socket server were all
//! running -- and `std::env::set_var` is unsound if any other thread may
//! touch the environment concurrently.
//!
//! Its own test binary, because the contract is about process-global state
//! (the recorded path, and the environment) that a test cannot un-set.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code

use std::path::Path;

use neuroos_storage::embed::{EmbedError, Embedder};

/// One sequenced test, not four: the state under test is process-global,
/// so the order is part of what is being asserted.
#[test]
fn the_dylib_path_is_set_once_explicitly_and_never_by_load() {
    assert!(
        std::env::var_os("ORT_DYLIB_PATH").is_none(),
        "this test owns the variable; something set it first"
    );

    // Loading without a path having been set is an error, not a silent
    // write from whatever thread happens to get here first.
    let Err(err) = Embedder::load(Path::new("/nonexistent"), Path::new("/nonexistent/lib.so"))
    else {
        panic!("load must refuse to run before the path is set");
    };
    assert!(matches!(err, EmbedError::DylibPathNotSet), "{err}");
    assert!(
        std::env::var_os("ORT_DYLIB_PATH").is_none(),
        "load must not write the variable"
    );

    let chosen = Path::new("/opt/onnxruntime/lib/libonnxruntime.so");
    Embedder::set_dylib_path(chosen).unwrap();
    assert_eq!(
        std::env::var("ORT_DYLIB_PATH").unwrap(),
        chosen.to_str().unwrap()
    );

    // Idempotent for the same path: a second service component (or a
    // second test) asking for what is already in effect is fine.
    Embedder::set_dylib_path(chosen).unwrap();

    // A *different* path is a programming error, and must not silently
    // re-write the variable: `ort` has already dlopen'd the first one.
    let other = Path::new("/usr/lib/libonnxruntime.so");
    let err = Embedder::set_dylib_path(other).expect_err("a conflicting path must be rejected");
    assert!(
        matches!(err, EmbedError::DylibPathAlreadySet { .. }),
        "{err}"
    );
    assert_eq!(
        std::env::var("ORT_DYLIB_PATH").unwrap(),
        chosen.to_str().unwrap(),
        "the variable must still hold the path that is actually loaded"
    );
}

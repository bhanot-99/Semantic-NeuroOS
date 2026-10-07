//! Shared test fixtures for the real embedding model.
//!
//! C4: `dev_models_dir` and `dev_onnxruntime_dylib` were copy-pasted into
//! eleven test modules across `neuroos-storage` and `neuroos-knowledge-query`.
//! They belong here because what they pin — `Embedder`'s process-wide
//! `ORT_DYLIB_PATH` (M16) — is this crate's own global, and because
//! `CARGO_MANIFEST_DIR` then resolves against one crate rather than each
//! caller's directory.
//!
//! Compiled only under `cfg(test)` or the `test-support` feature, so nothing
//! here reaches a shipped binary.

use std::path::{Path, PathBuf};

use crate::embed::Embedder;

/// The real models fetched by `scripts/fetch-models.sh` into the repo's
/// `.dev-cache/models` (`/opt/neuroos/models` needs root to populate and is
/// not present on a dev machine).
pub fn dev_models_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dev-cache/models")
}

/// Pins the ONNX Runtime dylib the way `main` does and returns its path.
///
/// M16: a test binary is its own `main`, so it has to pin the dylib before
/// anything can reach `Embedder::load`. Harmless to repeat — only a
/// *conflicting* path is an error — so every test may call this.
pub fn dev_onnxruntime_dylib() -> PathBuf {
    let path =
        dev_models_dir().join("onnxruntime/onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so");
    let _ = Embedder::set_dylib_path(&path);
    path
}

//! The single FastEmbed `bge-small-en-v1.5` embedder (FR-STO-03): C3 is the
//! only component that loads an embedding model (rules.md AB-3). Built
//! against a locally-provided model + tokenizer files (never
//! `hf-hub`/`ort-download-binaries` — rules.md R0-5, see
//! `models/manifest.toml`'s entries and `Cargo.toml`'s `ort-load-dynamic`
//! feature) and a pinned, operator-fetched ONNX Runtime `.so`.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use fastembed::{
    InitOptionsUserDefined, Pooling, QuantizationMode, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};

/// `bge-small-en-v1.5`'s real output width (Architecture.md §7.2's LanceDB
/// `vector[384]` column).
pub const EMBEDDING_DIM: usize = 384;

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("failed to read model file {path}: {source}")]
    ReadModelFile {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("fastembed/onnxruntime error: {0}")]
    FastEmbed(#[from] fastembed::Error),
    /// M16: [`Embedder::set_dylib_path`] was never called, so there is no
    /// pinned ONNX Runtime to load. `load` used to write the environment
    /// variable itself from wherever it happened to be called, which is
    /// unsound once any other thread exists.
    #[error("the ONNX Runtime dylib path was never set; call Embedder::set_dylib_path from main")]
    DylibPathNotSet,
    #[error(
        "the ONNX Runtime dylib path is already set to {in_effect}, cannot change it to {requested}"
    )]
    DylibPathAlreadySet {
        in_effect: PathBuf,
        requested: PathBuf,
    },
    /// C5: the `spawn_blocking` worker that owns the embedder panicked.
    /// Reported as an error rather than re-raised with
    /// `std::panic::resume_unwind`, which rules.md §5 forbids outside test
    /// code: a panic inside fastembed/ort must degrade this one ingest or
    /// query, not take down C3's whole request loop.
    #[error("the embedding worker panicked: {0}")]
    WorkerPanicked(String),
}

/// The dylib this process has committed to (M16). `OnceLock`, not a plain
/// write, because `ort` dlopens the path on its first use and can never
/// load a second one.
static DYLIB_PATH: OnceLock<PathBuf> = OnceLock::new();

/// How many times a model has actually been loaded in this process. C3's
/// 300 MiB cap (PRD §6.2) only holds for one resident model, so this is
/// asserted on rather than left to RSS measurement (M16).
static LOADS: AtomicUsize = AtomicUsize::new(0);

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    /// Loads `bge-small-en-v1.5` from `models_dir/bge-small-en-v1.5/{model.onnx,
    /// tokenizer.json, config.json, tokenizer_config.json,
    /// special_tokens_map.json}` (models/manifest.toml's C3 entries) and
    /// points `ort` at `onnxruntime_dylib` (models/manifest.toml's
    /// `onnxruntime-linux-x64` entry, extracted `lib/libonnxruntime.so`).
    /// Points `ort` at the pinned ONNX Runtime `.so`
    /// (models/manifest.toml's `onnxruntime-linux-x64` entry, extracted
    /// `lib/libonnxruntime.so`). `ort`'s "load-dynamic" feature reads
    /// `ORT_DYLIB_PATH` to dlopen it instead of
    /// ort-download-binaries/a system search path (rules.md R0-5), and an
    /// environment variable is the only interface it offers --
    /// `ort::load_dynamic::init` is `pub(crate)`.
    ///
    /// # Contract
    /// Call this from `main` **before the async runtime starts and before
    /// any thread is spawned**, the same place and for the same reason as
    /// H15's `neuroos_sandbox::enter`. [`Self::load`] deliberately does
    /// *not* do it: it is called from a background re-index task, where
    /// the process is thoroughly multi-threaded.
    ///
    /// Calling it again with the same path is a no-op; with a different
    /// one it is an error, since `ort` has already dlopen'd the first.
    pub fn set_dylib_path(onnxruntime_dylib: &Path) -> Result<(), EmbedError> {
        let requested = onnxruntime_dylib.to_path_buf();
        if let Some(in_effect) = DYLIB_PATH.get() {
            if in_effect == &requested {
                return Ok(());
            }
            return Err(EmbedError::DylibPathAlreadySet {
                in_effect: in_effect.clone(),
                requested,
            });
        }
        // SAFETY: the caller's documented contract above is that this runs
        // before any other thread exists, which is what makes
        // `set_var` sound -- there is no other thread that could be in
        // `getenv` or holding a pointer into the environment. The
        // `OnceLock` below makes the write happen at most once even if
        // that contract is broken, so a late second caller cannot move the
        // environment out from under a thread that is reading it.
        // (Under `cargo test` several test binaries' threads share a
        // process; see tests/embedder_dylib_path.rs for why the
        // observable contract is tested in a process of its own.)
        #[allow(unsafe_code)] // the one documented exception in this crate
        unsafe {
            std::env::set_var("ORT_DYLIB_PATH", onnxruntime_dylib);
        }
        let _ = DYLIB_PATH.set(requested);
        Ok(())
    }

    /// The dylib path in effect, if [`Self::set_dylib_path`] has run.
    pub fn dylib_path() -> Option<&'static Path> {
        DYLIB_PATH.get().map(PathBuf::as_path)
    }

    /// How many models this process has loaded (M16: C3's memory cap only
    /// holds for one).
    pub fn load_count() -> usize {
        LOADS.load(Ordering::Relaxed)
    }

    pub fn load(models_dir: &Path, onnxruntime_dylib: &Path) -> Result<Self, EmbedError> {
        // M16: `load` no longer writes `ORT_DYLIB_PATH`. It used to, from
        // whatever thread called it -- including a spawned re-index task,
        // long after the runtime's workers, the health server and the
        // socket server were all running.
        match DYLIB_PATH.get() {
            None => return Err(EmbedError::DylibPathNotSet),
            Some(in_effect) if in_effect != onnxruntime_dylib => {
                return Err(EmbedError::DylibPathAlreadySet {
                    in_effect: in_effect.clone(),
                    requested: onnxruntime_dylib.to_path_buf(),
                });
            }
            Some(_) => {}
        }

        let dir = models_dir.join("bge-small-en-v1.5");
        let read = |name: &str| -> Result<Vec<u8>, EmbedError> {
            let path = dir.join(name);
            std::fs::read(&path).map_err(|source| EmbedError::ReadModelFile { path, source })
        };

        let onnx_file = read("model.onnx")?;
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
            tokenizer_config_file: read("tokenizer_config.json")?,
        };

        let user_model = UserDefinedEmbeddingModel::new(onnx_file, tokenizer_files)
            .with_pooling(Pooling::Cls) // matches fastembed's own BGESmallENV15 metadata
            .with_quantization(QuantizationMode::None);
        let model = TextEmbedding::try_new_from_user_defined(
            user_model,
            InitOptionsUserDefined::default(),
        )?;
        LOADS.fetch_add(1, Ordering::Relaxed);
        Ok(Self { model })
    }

    /// FR-STO-05's ~11-13ms budget is embedding (9.5-12ms) + search
    /// (<1ms); this is the embedding half. Single-query path — batching for
    /// ingest is the caller's concern (pass multiple texts at once).
    pub fn embed(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(self.model.embed(texts, None)?)
    }

    /// Identifies which embedding model produced a vector, stored in
    /// `index_meta.embedding_model_id` (FR-STO-11: re-index on change).
    pub fn model_id() -> &'static str {
        "bge-small-en-v1.5"
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;
    use crate::test_support::{dev_models_dir, dev_onnxruntime_dylib};

    /// Live proof (P4-S03): loads the real downloaded model against the
    /// real fetched ONNX Runtime and embeds real sentences — needs
    /// `just fetch-models` (or this dev session's `.dev-cache/models`) to
    /// have run first, so it's `#[ignore]`d like this repo's other
    /// real-download-dependent tests (`cargo test -p neuroos-storage --lib
    /// -- --ignored embed`).
    #[test]
    #[ignore = "needs models fetched into .dev-cache/models (just fetch-models); see doc comment"]
    fn embeds_real_sentences_to_384_dims_and_distinguishes_them() {
        let mut embedder = Embedder::load(&dev_models_dir(), &dev_onnxruntime_dylib())
            .expect("real model + real onnxruntime should load");
        let vectors = embedder
            .embed(&["the cat sat on the mat", "quarterly revenue report"])
            .expect("real embed call");
        assert_eq!(vectors.len(), 2);
        for v in &vectors {
            assert_eq!(v.len(), EMBEDDING_DIM);
        }
        // two unrelated sentences must not produce identical vectors
        assert_ne!(vectors[0], vectors[1]);
    }
}

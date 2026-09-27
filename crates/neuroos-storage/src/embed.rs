//! The single FastEmbed `bge-small-en-v1.5` embedder (FR-STO-03): C3 is the
//! only component that loads an embedding model (rules.md AB-3). Built
//! against a locally-provided model + tokenizer files (never
//! `hf-hub`/`ort-download-binaries` — rules.md R0-5, see
//! `models/manifest.toml`'s entries and `Cargo.toml`'s `ort-load-dynamic`
//! feature) and a pinned, operator-fetched ONNX Runtime `.so`.
use std::path::Path;

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
}

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    /// Loads `bge-small-en-v1.5` from `models_dir/bge-small-en-v1.5/{model.onnx,
    /// tokenizer.json, config.json, tokenizer_config.json,
    /// special_tokens_map.json}` (models/manifest.toml's C3 entries) and
    /// points `ort` at `onnxruntime_dylib` (models/manifest.toml's
    /// `onnxruntime-linux-x64` entry, extracted `lib/libonnxruntime.so`).
    pub fn load(models_dir: &Path, onnxruntime_dylib: &Path) -> Result<Self, EmbedError> {
        // SAFETY: setting a process-wide env var before `ort` lazily
        // initializes its environment on first use (single-threaded at
        // startup, before any embed() call races this). `ort`'s own
        // "load-dynamic" feature reads this to dlopen the pinned .so
        // instead of ort-download-binaries/a system search path.
        unsafe {
            std::env::set_var("ORT_DYLIB_PATH", onnxruntime_dylib);
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

    fn dev_models_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dev-cache/models")
    }

    fn dev_onnxruntime_dylib() -> std::path::PathBuf {
        dev_models_dir().join("onnxruntime/onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so")
    }

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

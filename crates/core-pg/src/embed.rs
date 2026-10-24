//! Local embeddings for `memory.facts.embedding` (D-32, D-103): fastembed with
//! BAAI/bge-small-en-v1.5, 384 dims, loaded once per process on first use.
//!
//! `TextEmbedding::embed` takes `&mut self`, so the model sits behind a mutex; the mutex is
//! also the once-only initialiser (the first caller downloads and loads the model, later
//! callers find it in place). All work runs on `spawn_blocking` because ONNX inference blocks.

use crate::{backend, EMBEDDING_DIMS};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use harness_core::memory::Result;
use std::path::PathBuf;
use std::sync::Mutex;

static MODEL: Mutex<Option<TextEmbedding>> = Mutex::new(None);

/// D-103: `SB_MODEL_DIR`, else `<cache_dir>/sheetbrowse/fastembed`; never the crate's default
/// `.fastembed_cache` in the working directory. (fastembed lets `HF_HOME` override both.)
pub fn model_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("SB_MODEL_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("sheetbrowse")
        .join("fastembed")
}

/// The platform cache directory (what `dirs::cache_dir` returns), without the dependency.
fn cache_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        var("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|h| h.join("Library").join("Caches"))
    } else {
        var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache")))
    }
}

/// Embeds each text into a 384-dim vector, in order.
pub async fn embed(texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    tokio::task::spawn_blocking(move || embed_blocking(&texts))
        .await
        .map_err(|e| backend(format!("embedding task failed: {e}")))?
}

/// Embeds a single text.
pub async fn embed_one(text: String) -> Result<Vec<f32>> {
    embed(vec![text])
        .await?
        .pop()
        .ok_or_else(|| backend("embedding model returned no vector"))
}

fn embed_blocking(texts: &[String]) -> Result<Vec<Vec<f32>>> {
    let mut guard = MODEL.lock().unwrap_or_else(|e| e.into_inner());
    let model = match guard.as_mut() {
        Some(m) => m,
        None => {
            let opts = TextInitOptions::new(EmbeddingModel::BGESmallENV15)
                .with_cache_dir(model_dir())
                .with_show_download_progress(false);
            let m = TextEmbedding::try_new(opts)
                .map_err(|e| backend(format!("loading embedding model: {e}")))?;
            guard.insert(m)
        }
    };
    let out = model
        .embed(texts, None)
        // A fixed message: the library's text could quote the input being embedded (rule 2).
        .map_err(|_| backend("embedding: the model failed on this input"))?;
    if out.len() != texts.len() || out.iter().any(|v| v.len() != EMBEDDING_DIMS) {
        return Err(backend(format!(
            "embedding model returned the wrong shape (want {} x {EMBEDDING_DIMS})",
            texts.len()
        )));
    }
    Ok(out)
}

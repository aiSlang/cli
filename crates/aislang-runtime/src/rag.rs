//! v0.3 Week 19 Wed — RAG runtime backing `vector_store` blocks
//! (Decision 44.6).
//!
//! ## What this module does
//!
//! - **`build_index`** — chunks a source corpus into overlapping
//!   character windows, embeds each chunk via a user-supplied
//!   [`Embedder`], and stores `(chunk_text, vector)` pairs in
//!   memory.  Called at `aislang apply` time per declared
//!   `vector_store` block.
//! - **`Index::query`** — embeds the query string + ranks every
//!   stored chunk by cosine similarity, returns the top-K
//!   `(chunk_text, score)` pairs.
//! - **`Index::save` / `Index::load`** — persist the in-memory
//!   index to a single file under `<source_dir>/.aislang/vector/
//!   <store-label>.bin` (bincode-serialised; see file format
//!   note below).  `aislang destroy -v` wipes that subtree.
//!
//! ## v0.3 deviation from Decision 44.4 (usearch)
//!
//! Decision 44.4 locked usearch as the embedded vector DB.  v0.3
//! Wed deliberately **deviates** and uses a brute-force cosine
//! scan over a plain `Vec<Chunk>`:
//!
//!   - **Why deviate**: usearch has a C++ backend (build-toolchain
//!     dep — adds ~30s to first `cargo build`, can break in
//!     toolchain-thin CI runners); v0.3 RAG corpus sizes
//!     (hundreds of chunks per typical .ais project knowledge
//!     base) don't justify the HNSW indexing cost — at n ≤ ~1k
//!     a linear scan is ~µs per query on commodity hardware.
//!   - **Forward-compat**: the [`Index`] type's public API
//!     (`build_index`, `query`, `save`, `load`) is shaped to be
//!     swap-compatible with usearch's surface, so v0.5+ can drop
//!     in usearch without breaking callers.
//!   - **Trigger to swap**: a real `examples/agents/` project
//!     accumulates >5k chunks AND retrieval p95 exceeds ~10ms,
//!     OR HNSW recall-vs-latency tradeoff becomes meaningful.
//!
//! ## Chunking model (v0.3 simplification)
//!
//! `vector_store.chunk_size` + `chunk_overlap` are declared in
//! the .ais source as `Size` units (the parser enforces 256..=
//! 8192).  **v0.3 interprets those numbers as CHARACTERS, not
//! tokens** — token-aware chunking would require a tokenizer
//! dependency (`tiktoken-rs` ~5K LoC + bundled bpe tables).  The
//! character interpretation is documented at the chunker entry
//! point + at the .ais grammar surface (LANGUAGE-SPEC §4.13);
//! v0.5+ may add per-embedding tokenizer integration when the
//! retrieval-quality gap matters.
//!
//! ## Embedding abstraction
//!
//! The module is parameterised on the [`Embedder`] trait so unit
//! tests can inject deterministic vectors without standing up a
//! LiteLLM proxy.  Production callers (the chat-time RAG hook,
//! `aislang apply`-time index build) supply [`LiteLlmEmbedder`]
//! which POSTs to LiteLLM's OpenAI-compatible `/v1/embeddings`
//! endpoint and returns the `data[0].embedding` Vec<f32>.

use serde::{Deserialize, Serialize};
use std::collections::BinaryHeap;
use std::path::Path;

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, thiserror::Error)]
pub enum RagError {
    #[error("embedder failed: {0}")]
    Embed(String),

    #[error("index I/O at `{path}`: {cause}")]
    Io { path: String, cause: String },

    #[error("index file `{path}` deserialize failed: {cause}")]
    Decode { path: String, cause: String },

    #[error("dimension mismatch: query is {query_dims}, index is {index_dims} \
             (built with a different embedding model?)")]
    DimensionMismatch { query_dims: usize, index_dims: usize },
}

// ============================================================================
// Embedder — pluggable embedding source
// ============================================================================

/// Abstraction over "how do I turn this text into a Vec<f32>?".
/// Production = HTTP call to LiteLLM's `/v1/embeddings`; tests use
/// deterministic mocks ([`HashEmbedder`]) so the index round-trips
/// are reproducible without infra.
pub trait Embedder {
    /// Embed a single string.  Returns the dimension-D vector.
    fn embed(&self, text: &str) -> Result<Vec<f32>, RagError>;

    /// Output dimension.  Used by `build_index` to validate every
    /// chunk's embedding matches the index's expected shape.
    fn dimensions(&self) -> usize;
}

/// Test-only deterministic embedder: chunks the text into a fixed
/// number of buckets via FNV-1a hashing and produces a vector
/// derived from the bucket counts.  Same text → same vector;
/// similar texts → similar vectors (shared bucket overlap), so
/// cosine ranking is meaningful in tests.
///
/// **NOT for production** — no semantic content, no language
/// understanding.  Production tests should mock at a higher
/// level (or use the live `LiteLlmEmbedder` against a real proxy).
#[derive(Debug, Clone)]
pub struct HashEmbedder {
    pub dims: usize,
}

impl HashEmbedder {
    pub fn new(dims: usize) -> Self { Self { dims } }
}

impl Embedder for HashEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, RagError> {
        // FNV-1a 64-bit per word, then bucket counts → normalise.
        let mut counts = vec![0u32; self.dims];
        for word in text.split_whitespace() {
            let mut h: u64 = 0xcbf29ce484222325;
            for b in word.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            counts[(h as usize) % self.dims] += 1;
        }
        // Normalise to unit length so cosine == dot-product.
        let v: Vec<f32> = counts.iter().map(|&c| c as f32).collect();
        let norm = (v.iter().map(|x| x*x).sum::<f32>()).sqrt();
        if norm == 0.0 {
            return Ok(vec![0.0; self.dims]);
        }
        Ok(v.iter().map(|x| x / norm).collect())
    }

    fn dimensions(&self) -> usize { self.dims }
}

// ============================================================================
// Index — stored chunks + cosine search
// ============================================================================

/// One indexable text chunk + its embedding vector.  Persisted as
/// part of the on-disk index file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chunk {
    pub text:   String,
    pub vector: Vec<f32>,
}

/// In-memory RAG index.  Field set is the on-disk format too:
/// bincode-serialised at `save` time, restored at `load` time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Index {
    /// Embedding dimension — pinned at build time + checked on
    /// every query.  Mismatch (e.g. query embedded via a
    /// different model than the index was built with) surfaces
    /// as [`RagError::DimensionMismatch`].
    pub dims:   usize,
    /// `vector_store.label` the index was built for — recorded
    /// here so a misplaced .bin file can be identified during
    /// debugging.  Not used for routing (the caller picks the
    /// file by label-named path).
    pub label:  String,
    /// All indexed chunks.  v0.3 stores them in build order;
    /// `query` returns the top-K by cosine similarity score.
    pub chunks: Vec<Chunk>,
}

impl Index {
    /// In-memory result of [`Index::query`].  `score` is the
    /// cosine similarity in [-1, 1] (closer to 1 = better match).
    #[allow(clippy::missing_const_for_fn)]
    pub fn new_empty(label: impl Into<String>, dims: usize) -> Self {
        Self { dims, label: label.into(), chunks: Vec::new() }
    }

    /// Append one chunk + its embedding to the index.  Dimension-
    /// checked: mismatched-shape vectors are rejected before
    /// landing on disk.
    pub fn push(&mut self, chunk: Chunk) -> Result<(), RagError> {
        if chunk.vector.len() != self.dims {
            return Err(RagError::DimensionMismatch {
                query_dims: chunk.vector.len(),
                index_dims: self.dims,
            });
        }
        self.chunks.push(chunk);
        Ok(())
    }

    /// Top-K cosine-similarity ranked chunks for `query_vec`.
    /// Returns `(chunk_text, score)` pairs ordered by score
    /// descending.  Score is `dot(a,b)` (since stored vectors are
    /// unit-normalised — see [`HashEmbedder::embed`]; production
    /// [`LiteLlmEmbedder`] does NOT normalise, so this is
    /// dot-product-of-arbitrary-magnitude-vectors, which still
    /// preserves rank under monotone embed magnitudes — good
    /// enough for v0.3.  v0.5+ may normalise explicitly).
    pub fn query(&self, query_vec: &[f32], top_k: usize)
        -> Result<Vec<(String, f32)>, RagError>
    {
        if query_vec.len() != self.dims {
            return Err(RagError::DimensionMismatch {
                query_dims: query_vec.len(),
                index_dims: self.dims,
            });
        }
        // Compute scores; collect into a max-heap by score.  We
        // wrap in a custom Ord-impl wrapper because f32 isn't Ord.
        // Pop the top K; reverse to get descending order.
        let mut heap: BinaryHeap<(F32Ord, usize)> =
            BinaryHeap::with_capacity(self.chunks.len());
        for (i, c) in self.chunks.iter().enumerate() {
            let score = cosine(&c.vector, query_vec);
            heap.push((F32Ord(score), i));
        }
        let mut out = Vec::with_capacity(top_k.min(self.chunks.len()));
        for _ in 0..top_k {
            if let Some((F32Ord(score), i)) = heap.pop() {
                out.push((self.chunks[i].text.clone(), score));
            } else {
                break;
            }
        }
        Ok(out)
    }

    /// Persist via bincode (compact + fast + matches the v0.5+
    /// usearch-binary file shape's posture).  Returns the bytes-
    /// written count for diagnostic logging.
    pub fn save(&self, path: &Path) -> Result<usize, RagError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RagError::Io {
                path:  parent.display().to_string(),
                cause: e.to_string(),
            })?;
        }
        let bytes = serde_json::to_vec(self).map_err(|e| RagError::Io {
            path:  path.display().to_string(),
            cause: format!("serialise: {e}"),
        })?;
        std::fs::write(path, &bytes).map_err(|e| RagError::Io {
            path:  path.display().to_string(),
            cause: e.to_string(),
        })?;
        Ok(bytes.len())
    }

    /// Restore from the file [`save`] wrote.  Doesn't touch the
    /// embedder — caller is responsible for ensuring the index
    /// was built with a model that produces vectors of `self.dims`
    /// (the runtime catches mismatches at query time anyway).
    pub fn load(path: &Path) -> Result<Self, RagError> {
        let bytes = std::fs::read(path).map_err(|e| RagError::Io {
            path:  path.display().to_string(),
            cause: e.to_string(),
        })?;
        let idx: Self = serde_json::from_slice(&bytes)
            .map_err(|e| RagError::Decode {
                path:  path.display().to_string(),
                cause: e.to_string(),
            })?;
        Ok(idx)
    }
}

// f32-as-Ord wrapper for the heap.  NaN sorts as -Infinity (i.e. last);
// the embedder is contract-bound to never produce NaN, but we degrade
// gracefully rather than panicking.
#[derive(PartialEq)]
struct F32Ord(f32);
impl Eq for F32Ord {}
impl PartialOrd for F32Ord {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for F32Ord {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.partial_cmp(&other.0).unwrap_or(std::cmp::Ordering::Less)
    }
}

/// Cosine similarity between two vectors.  Returns 0 if either
/// vector has zero magnitude (degenerate).  Assumes equal length —
/// callers (Index::query, etc.) check dimension upstream.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot:    f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x*x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x*x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 { 0.0 } else { dot / (norm_a * norm_b) }
}

// ============================================================================
// Chunking
// ============================================================================

/// Split a source string into overlapping character windows.
/// **v0.3 character-counts**, not tokens — see module docstring for
/// the rationale.  Chunks honour UTF-8 char boundaries; if `size`
/// or `overlap` cuts inside a multi-byte char, the chunker rounds
/// down to the previous boundary.
///
/// Returns chunks in source order.  The last chunk may be shorter
/// than `size` (trailing remainder).  Empty input → empty Vec.
pub fn chunk_text(text: &str, size: usize, overlap: usize) -> Vec<String> {
    assert!(overlap < size, "chunk_overlap must be < chunk_size (checker enforced)");
    assert!(size > 0,       "chunk_size must be > 0 (parser enforced)");

    let mut out = Vec::new();
    if text.is_empty() { return out; }
    let stride = size - overlap;

    // Walk char-by-char, emitting a chunk whenever we've accumulated
    // `size` chars OR hit end-of-input.
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.is_empty() { return out; }
    let mut cursor = 0usize;
    while cursor < chars.len() {
        let end = (cursor + size).min(chars.len());
        let start_byte = chars[cursor].0;
        let end_byte = if end < chars.len() { chars[end].0 } else { text.len() };
        out.push(text[start_byte..end_byte].to_string());
        if end == chars.len() { break; }
        cursor += stride;
    }
    out
}

/// Top-level: chunk + embed + build an [`Index`] for one
/// `vector_store` block's `source` content.  The source body is
/// supplied by the caller (path / URL / crawl fetching is
/// orthogonal — the caller resolves `SourceUri` to a String).
pub fn build_index<E: Embedder>(
    label:      impl Into<String>,
    source:     &str,
    chunk_size: usize,
    overlap:    usize,
    embedder:   &E,
) -> Result<Index, RagError> {
    let label = label.into();
    let mut idx = Index::new_empty(label, embedder.dimensions());
    for chunk_text in chunk_text(source, chunk_size, overlap) {
        let vec = embedder.embed(&chunk_text)?;
        idx.push(Chunk { text: chunk_text, vector: vec })?;
    }
    Ok(idx)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- chunk_text -------------------------------------------- //

    #[test] fn chunk_text_splits_at_chunk_size_with_overlap() {
        let text  = "abcdefghijklmnop";
        let out   = chunk_text(text, 8, 2);
        // size=8, overlap=2, stride=6 → starts at 0, 6, 12
        assert_eq!(out, vec![
            "abcdefgh".to_string(),     // [0..8)
            "ghijklmn".to_string(),     // [6..14)
            "mnop".to_string(),         // [12..16) — trailing remainder
        ]);
    }

    #[test] fn chunk_text_empty_input_returns_empty_vec() {
        assert!(chunk_text("", 8, 2).is_empty());
    }

    #[test] fn chunk_text_no_overlap_disjoint_partition() {
        let text = "abcdefghij";
        let out  = chunk_text(text, 4, 0);
        assert_eq!(out, vec!["abcd", "efgh", "ij"]);
    }

    #[test] fn chunk_text_respects_utf8_char_boundaries() {
        // Each emoji is 4 bytes; `size = 2` (chars) → 2 emojis per chunk.
        // No invalid-UTF-8 panic; byte boundaries calculated from
        // char_indices, not raw arithmetic.
        let text = "🍎🍌🍇🍐🍓";
        let out  = chunk_text(text, 2, 0);
        assert_eq!(out, vec!["🍎🍌", "🍇🍐", "🍓"]);
    }

    // ---- HashEmbedder ------------------------------------------ //

    #[test] fn hash_embedder_deterministic_for_same_text() {
        let e = HashEmbedder::new(64);
        let a = e.embed("hello world").expect("embed");
        let b = e.embed("hello world").expect("embed");
        assert_eq!(a, b, "same text MUST yield same vector");
    }

    #[test] fn hash_embedder_unit_normalised() {
        let e = HashEmbedder::new(64);
        let v = e.embed("aiSlang RAG runtime test fixture body").expect("embed");
        let norm: f32 = v.iter().map(|x| x*x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5,
            "HashEmbedder must produce unit-normalised vectors; got |v|={norm}");
    }

    // ---- build_index + query round-trip ------------------------ //

    #[test] fn build_index_round_trip_returns_chunk_count_matches() {
        // PLAN DoD: "build_index → query round-trip with synthetic
        // 100-doc fixture".  We use a single long-ish source string
        // chunked into 100+ pieces (cheaper than 100 separate docs;
        // same code path through chunk_text → embed → push).
        let source: String = (0..100)
            .map(|i| format!("doc{i} body content "))
            .collect();
        let embedder = HashEmbedder::new(64);
        // chunk_size=32 chars, no overlap → 100 docs of ~16 chars
        // each yield ~50 chunks (two docs per chunk).  Exact count
        // less important than "non-empty" + the round-trip works.
        let idx = build_index("test", &source, 32, 0, &embedder).expect("build");
        assert!(!idx.chunks.is_empty(), "index must have chunks");
        assert_eq!(idx.dims, 64);
        assert_eq!(idx.label, "test");
    }

    #[test] fn query_returns_top_k_in_score_descending_order() {
        let source = "alpha bravo\ncharlie delta\necho foxtrot\ngolf hotel";
        let embedder = HashEmbedder::new(64);
        // chunk_size=12 chars → 4 chunks (one per line).
        let idx = build_index("t", source, 12, 0, &embedder).expect("build");
        let q = embedder.embed("alpha bravo").expect("embed query");
        let top = idx.query(&q, 4).expect("query");
        // Same-text chunk must rank first (cosine ≈ 1.0).
        assert_eq!(top.len(), 4);
        assert!(top[0].0.contains("alpha"),
            "top result must be the matching chunk; got: {top:?}");
        // Scores descending.
        for w in top.windows(2) {
            assert!(w[0].1 >= w[1].1,
                "scores must be descending; got {:?} -> {:?}", w[0], w[1]);
        }
    }

    #[test] fn query_with_wrong_dims_errors_cleanly() {
        let idx = Index { dims: 64, label: "t".into(), chunks: vec![] };
        let err = idx.query(&vec![0.0; 32], 5).unwrap_err();
        match err {
            RagError::DimensionMismatch { query_dims: 32, index_dims: 64 } => (),
            other => panic!("expected DimensionMismatch; got {other:?}"),
        }
    }

    #[test] fn push_with_wrong_dims_errors_cleanly() {
        let mut idx = Index::new_empty("t", 64);
        let err = idx.push(Chunk {
            text: "x".into(), vector: vec![0.0; 32]
        }).unwrap_err();
        match err {
            RagError::DimensionMismatch { .. } => (),
            other => panic!("expected DimensionMismatch; got {other:?}"),
        }
    }

    // ---- save + load (persistence DoD) ------------------------- //

    #[test] fn save_then_load_preserves_index_state_byte_for_byte() {
        // PLAN DoD: "persistence: load existing index, no rebuild
        // needed".  Build → save → load → query against the loaded
        // index produces the same answers as querying the built one.
        let source = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
        let embedder = HashEmbedder::new(32);
        let built = build_index("persist", source, 16, 4, &embedder).expect("build");

        let tmp = std::env::temp_dir().join(format!(
            "aislang-rag-persist-test-{}.bin", std::process::id()));
        let written = built.save(&tmp).expect("save");
        assert!(written > 0, "save must write non-zero bytes");

        let loaded = Index::load(&tmp).expect("load");
        assert_eq!(built, loaded,
            "save → load round-trip must preserve EVERY field byte-stable");

        // Query equivalence: same query, same top-K, same scores.
        let q = embedder.embed("delta echo").expect("embed");
        let top_built  = built.query(&q, 3).expect("query built");
        let top_loaded = loaded.query(&q, 3).expect("query loaded");
        assert_eq!(top_built, top_loaded);

        let _ = std::fs::remove_file(&tmp);
    }

    #[test] fn load_missing_file_errors_with_path() {
        let err = Index::load(std::path::Path::new("/nonexistent/rag-index.bin"))
            .unwrap_err();
        match err {
            RagError::Io { path, .. } =>
                assert!(path.contains("/nonexistent/rag-index.bin")),
            other => panic!("expected Io; got {other:?}"),
        }
    }
}

//! aiSlang model catalog.
//!
//! Defines the universe of `ModelId` values the language can reference.
//! In v0.1 the catalog is bundled in the binary via [`include_str!`]; a
//! remote update mechanism is Phase 1 (see LANGUAGE-SPEC-v0.md §5.3).

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// JSON content of the bundled catalog. Updated in `catalog.json`.
pub const BUNDLED_CATALOG_JSON: &str = include_str!("../catalog.json");

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("failed to parse bundled catalog: {0}")]
    Parse(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    /// Catalog version (ISO-8601 date, e.g. `"2026-05-22"`).
    pub version: String,
    pub models:  Vec<CatalogModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModel {
    /// Canonical aiSlang ID, e.g. `"anthropic.claude_haiku_4_5"`.
    pub id: String,
    pub provider: String,
    /// The string passed to LiteLLM at runtime.
    pub litellm_name: String,
    pub capabilities: Vec<String>,
    /// Maximum context window, expressed in the aiSlang `Size` literal form
    /// (e.g. `"200k"`).
    pub context_max: String,
    pub cost_input_per_1k_tokens:  String,
    pub cost_output_per_1k_tokens: String,
    pub latency_p95: String,
    pub residency:   Vec<String>,
    #[serde(default)]
    pub deprecated:  bool,
}

/// Load and parse the catalog bundled with this binary.
pub fn bundled() -> Result<Catalog, CatalogError> {
    let cat: Catalog = serde_json::from_str(BUNDLED_CATALOG_JSON)?;
    Ok(cat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_parses() {
        let cat = bundled().expect("bundled catalog must parse");
        assert!(!cat.models.is_empty(), "catalog must list at least one model");
        assert!(!cat.version.is_empty(), "catalog must have a version");
    }

    #[test]
    fn every_model_id_is_unique() {
        let cat = bundled().expect("bundled catalog must parse");
        let mut ids: Vec<&str> = cat.models.iter().map(|m| m.id.as_str()).collect();
        ids.sort();
        let len_before = ids.len();
        ids.dedup();
        assert_eq!(len_before, ids.len(), "duplicate model id in catalog");
    }
}

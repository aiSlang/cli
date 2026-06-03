//! aiSlang model catalog.
//!
//! Defines the universe of `ModelId` values the language can reference.
//! In v0.1 the catalog is bundled in the binary via [`include_str!`]; a
//! remote update mechanism is Phase 1 (see LANGUAGE-SPEC-v0.md §5.3).
//!
//! Every value-typed field in [`CatalogModel`] is deserialized through
//! the same `from_literal` parsers the language uses (see
//! `aislang_core::types`). A malformed value in `catalog.json` fails at
//! binary build time (the `bundled_catalog_parses` test trips), not at
//! runtime — see LANGUAGE-SPEC-v0.md §5.5 for the rationale.

use aislang_core::ast::ModelId;
use aislang_core::types::{
    Capability, Duration, Money, Percentage, Residency, Size,
};
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
    /// Catalog version (ISO-8601 date, e.g. `"2026-05-25"`).
    pub version: String,
    pub models:  Vec<CatalogModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModel {
    /// Canonical aiSlang ID, e.g. `"anthropic.claude_haiku_4_5"`.
    pub id: String,

    /// Provider identifier, e.g. `"anthropic"`.
    pub provider: String,

    /// The string passed to LiteLLM at runtime, e.g. `"anthropic/claude-haiku-4-5"`.
    pub litellm_name: String,

    /// Capabilities the model satisfies. Deserialized from JSON `["chat", "tool_use"]`
    /// via the `rename_all = "snake_case"` derive on [`Capability`].
    pub capabilities: Vec<Capability>,

    /// Maximum context window. Parsed via `Size::from_literal` (e.g. `"200k"` → 200·1024).
    #[serde(deserialize_with = "shims::deserialize_size", serialize_with = "shims::serialize_size")]
    pub context_max: Size,

    /// Input price per 1k tokens. Parsed via `Money::from_literal`.
    #[serde(rename = "cost_input_per_1k_tokens")]
    #[serde(deserialize_with = "shims::deserialize_money", serialize_with = "shims::serialize_money")]
    pub cost_input: Money,

    /// Output price per 1k tokens.
    #[serde(rename = "cost_output_per_1k_tokens")]
    #[serde(deserialize_with = "shims::deserialize_money", serialize_with = "shims::serialize_money")]
    pub cost_output: Money,

    /// p95 first-token latency. Parsed via `Duration::from_literal` (integer-only;
    /// use `1500ms` rather than `1.5s` — see LANGUAGE-SPEC-v0.md §3.2 note).
    #[serde(deserialize_with = "shims::deserialize_duration", serialize_with = "shims::serialize_duration")]
    pub latency_p95: Duration,

    /// Data residencies where the provider hosts the model.
    pub residency: Vec<Residency>,

    #[serde(default)]
    pub deprecated: bool,
}

/// Load and parse the catalog bundled with this binary.
pub fn bundled() -> Result<Catalog, CatalogError> {
    let cat: Catalog = serde_json::from_str(BUNDLED_CATALOG_JSON)?;
    Ok(cat)
}

impl Catalog {
    /// Look up a catalog entry by `ModelId`. The catalog stores `id` as
    /// the dotted `"provider.name"` string; this function reconstructs
    /// that form from a parsed `ModelId` and does a linear scan.
    ///
    /// For v0.1's 10-entry catalog the linear scan is trivially fast
    /// (~100 ns); a future index can be added behind this same API
    /// without breaking callers if the catalog ever grows past O(100).
    pub fn find(&self, id: &ModelId) -> Option<&CatalogModel> {
        let dotted = format!("{}.{}", id.provider, id.name);
        self.models.iter().find(|m| m.id == dotted)
    }
}

// ---------------------------------------------------------------------- //
// Serde shims — route value strings through the language's literal parsers
// so the catalog and `.ais` source share one source of truth for value
// formats. See LANGUAGE-SPEC-v0.md §5.5.
// ---------------------------------------------------------------------- //

mod shims {
    use super::*;
    use serde::{de, ser, Deserialize, Deserializer, Serializer};

    pub fn deserialize_money<'de, D>(de: D) -> Result<Money, D::Error>
    where D: Deserializer<'de> {
        let s = String::deserialize(de)?;
        Money::from_literal(&s).map_err(de::Error::custom)
    }

    pub fn deserialize_size<'de, D>(de: D) -> Result<Size, D::Error>
    where D: Deserializer<'de> {
        let s = String::deserialize(de)?;
        Size::from_literal(&s).map_err(de::Error::custom)
    }

    pub fn deserialize_duration<'de, D>(de: D) -> Result<Duration, D::Error>
    where D: Deserializer<'de> {
        let s = String::deserialize(de)?;
        Duration::from_literal(&s).map_err(de::Error::custom)
    }

    // Serialize routes are stubbed — the v0.1 binary never writes the catalog,
    // it only reads. Provided so Catalog stays round-trippable for future tools
    // (catalog editor, registry sync). Format intentionally matches input.
    pub fn serialize_money<S: Serializer>(m: &Money, s: S) -> Result<S::Ok, S::Error> {
        let _ = m; let _ = s;
        Err(ser::Error::custom("Money serialization not implemented in v0.1"))
    }
    pub fn serialize_size<S: Serializer>(_: &Size, s: S) -> Result<S::Ok, S::Error> {
        let _ = s;
        Err(ser::Error::custom("Size serialization not implemented in v0.1"))
    }
    pub fn serialize_duration<S: Serializer>(_: &Duration, s: S) -> Result<S::Ok, S::Error> {
        let _ = s;
        Err(ser::Error::custom("Duration serialization not implemented in v0.1"))
    }

    // Silence unused-import warning in v0.1 — Percentage isn't currently in
    // the catalog schema but is exported by aislang_core::types alongside the
    // others. Remove this when Percentage gains a catalog field.
    #[allow(dead_code)]
    fn _percentage_referenced(_: Percentage) {}
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use aislang_core::types::Currency;

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

    #[test]
    fn known_entry_has_expected_typed_values() {
        // Haiku is a useful anchor: known capabilities, sub-cent pricing,
        // sub-second latency (now expressed as `1500ms` post-Tuesday-parser).
        let cat = bundled().expect("bundled catalog must parse");
        let haiku = cat
            .models
            .iter()
            .find(|m| m.id == "anthropic.claude_haiku_4_5")
            .expect("Haiku entry must exist in bundled catalog");

        assert_eq!(haiku.provider, "anthropic");
        assert!(haiku.capabilities.contains(&Capability::Chat));
        assert!(haiku.capabilities.contains(&Capability::ToolUse));
        assert!(haiku.capabilities.contains(&Capability::StructuredOutput));

        // Size: "200k" → 200 * 1024
        assert_eq!(haiku.context_max, Size { units: 200 * 1024 });

        // Money: "$0.0008" → 800 micro-USD
        assert_eq!(
            haiku.cost_input,
            Money::Capped { currency: Currency::Usd, micro_units: 800 }
        );

        // Duration: "1500ms" → 1.5 billion nanos. The integer-only Duration
        // parser would reject "1.5s"; this asserts the catalog has been
        // migrated to integer units.
        assert_eq!(haiku.latency_p95, Duration { nanos: 1_500_000_000 });

        // Residency — Anthropic serves Claude in both US and EU
        assert_eq!(haiku.residency, vec![Residency::Us, Residency::Eu]);
    }

    #[test]
    fn every_money_value_parses() {
        // Defense-in-depth: the bundled catalog deserialization above already
        // routes every Money string through `Money::from_literal`, but make
        // the contract explicit. If this test fires, look at the corresponding
        // catalog entry.
        let cat = bundled().expect("bundled catalog must parse");
        for m in &cat.models {
            // `Money::Unlimited` is technically valid for catalog costs
            // ("unbounded/unknown cost") — skip the non-negativity check
            // for it. `Capped` values must be ≥ 0.
            if let Some(u) = m.cost_input.micro_units() {
                assert!(u >= 0, "cost_input must be non-negative for {}", m.id);
            }
            if let Some(u) = m.cost_output.micro_units() {
                assert!(u >= 0, "cost_output must be non-negative for {}", m.id);
            }
        }
    }

    #[test]
    fn malformed_money_string_fails_deserialization() {
        // Sanity-check: a bad Money string in JSON must produce a serde error,
        // not silently default. This is the property §5.5 promises.
        let bad = r#"{
            "version": "test",
            "models": [{
                "id": "x.y", "provider": "x", "litellm_name": "x",
                "capabilities": ["chat"],
                "context_max": "100k",
                "cost_input_per_1k_tokens": "0.001",
                "cost_output_per_1k_tokens": "$0.001",
                "latency_p95": "1s",
                "residency": ["us"]
            }]
        }"#;
        let err = serde_json::from_str::<Catalog>(bad).err()
            .expect("missing currency symbol must fail deserialization");
        let msg = err.to_string();
        assert!(msg.contains("Money") || msg.contains("currency"),
            "error must mention the offending type, got: {msg}");
    }

    #[test]
    fn malformed_duration_string_fails_deserialization() {
        // Decimal duration ("1.5s") was the exact problem the bundled catalog
        // had before the Wednesday migration to "1500ms". This pins the
        // behavior so the integer-only Duration invariant is enforced at the
        // catalog level, not just the language level.
        let bad = r#"{
            "version": "test",
            "models": [{
                "id": "x.y", "provider": "x", "litellm_name": "x",
                "capabilities": ["chat"],
                "context_max": "100k",
                "cost_input_per_1k_tokens": "$0.001",
                "cost_output_per_1k_tokens": "$0.001",
                "latency_p95": "1.5s",
                "residency": ["us"]
            }]
        }"#;
        let err = serde_json::from_str::<Catalog>(bad).err()
            .expect("decimal duration must fail deserialization");
        assert!(err.to_string().contains("Duration"),
            "error must mention Duration, got: {err}");
    }

    // ---- Qwen catalog entries (v0.3 Week 18 Fri) ---------------- //

    #[test] fn ollama_qwen3_8b_entry_carries_expected_shape() {
        // PLAN DoD: "catalog ships both Qwen3 + Qwen2.5-Coder
        // entries with correct litellm_name mapping".  This pins
        // the Qwen3 entry's litellm_name (the exact slug LiteLLM
        // forwards to Ollama's `/api/chat`).  Ollama publishes
        // qwen3 in 0.6b / 1.7b / 4b / 8b / 14b / 32b sizes; PLAN
        // wrote "7b" but no Qwen3-7b tag exists — we ship 8b as
        // the closest cousin (still on consumer-GPU VRAM
        // budget at q4_K_M).
        let cat = bundled().expect("bundled");
        let qwen = cat.models.iter()
            .find(|m| m.id == "ollama.qwen3_8b")
            .expect("ollama.qwen3_8b must exist in bundled catalog");
        assert_eq!(qwen.provider, "ollama");
        assert_eq!(qwen.litellm_name, "ollama/qwen3:8b",
            "litellm_name must match the upstream Ollama tag exactly");
        assert!(qwen.capabilities.contains(&Capability::Chat));
        assert!(qwen.capabilities.contains(&Capability::ToolUse));
        assert!(qwen.capabilities.contains(&Capability::Reasoning),
            "qwen3 ships with reasoning-mode toggle — capability must reflect");
        // Local model → free.
        assert_eq!(qwen.cost_input,
            Money::Capped { currency: Currency::Usd, micro_units: 0 });
        assert_eq!(qwen.cost_output,
            Money::Capped { currency: Currency::Usd, micro_units: 0 });
    }

    #[test] fn ollama_qwen2_5_coder_7b_entry_carries_expected_shape() {
        // PLAN DoD: same as above for Qwen2.5-Coder.  The coder
        // variant exists in 1.5b / 3b / 7b / 14b / 32b; we ship
        // 7b as the consumer-GPU-friendly choice that matches the
        // open-source coding leader (better than CodeLlama /
        // DeepSeek-Coder per HumanEval, per Decision 44.8).
        let cat = bundled().expect("bundled");
        let coder = cat.models.iter()
            .find(|m| m.id == "ollama.qwen2_5_coder_7b")
            .expect("ollama.qwen2_5_coder_7b must exist in bundled catalog");
        assert_eq!(coder.provider, "ollama");
        assert_eq!(coder.litellm_name, "ollama/qwen2.5-coder:7b",
            "litellm_name must match the upstream Ollama tag exactly \
             (note the `2.5` decimal + `-coder` infix)");
        assert!(coder.capabilities.contains(&Capability::Chat));
        assert!(coder.capabilities.contains(&Capability::ToolUse));
        assert_eq!(coder.cost_input,
            Money::Capped { currency: Currency::Usd, micro_units: 0 });
        assert_eq!(coder.cost_output,
            Money::Capped { currency: Currency::Usd, micro_units: 0 });
    }
}

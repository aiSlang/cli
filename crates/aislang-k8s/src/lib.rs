//! Kubernetes emitter for aiSlang projects — Phase 2 Track B.
//!
//! Takes a [`ResolvedProject`] (post-validate, post-resolve, post-
//! lockfile-reconciliation) and emits the matching Kubernetes
//! manifests **and** a Helm chart wrapper.  Two views of the same
//! resources — users `kubectl apply -f manifests/` or
//! `helm upgrade --install <release> ./chart`; `aislang apply` uses
//! the Helm path so `aislang destroy` can `helm uninstall` cleanly.
//!
//! ## Phase 2 Week 14 Mon scope (this commit)
//!
//! Scaffolding only.  [`emit`] returns a [`K8sArtifacts`] skeleton
//! with empty `manifests` + `chart_files` + a placeholder
//! `values_yaml`.  Mirrors `aislang-compose::emit`'s shape so the
//! CLI's `run_apply` can branch on [`DeployTarget`] cleanly when
//! the rest of Week 14 lands the actual resource shapes.
//!
//! Week 14 day-by-day:
//!
//!   - **Mon (this commit):** crate scaffold + [`K8sArtifacts`] type
//!     + skeleton [`emit`] + parser/grammar acceptance of
//!     `target = kubernetes` + release-workflow image-publish job.
//!   - **Tue:** LiteLLM + Jaeger + budget_counter as
//!     Deployment+Service+ConfigMap; aislang labels; envFrom for
//!     provider creds.
//!   - **Wed:** mcp-serve runtime pod + prompt ConfigMaps;
//!     `.ais` source + `aislang.lock` baked into an
//!     `aislang-project` ConfigMap; `expose.port` becomes the
//!     mcp-serve Service port (finally functional under K8s).
//!   - **Thu:** Helm chart wrapper around Tue+Wed manifests;
//!     `expose { web_ui = true }` → Open WebUI Deployment +
//!     Service + PVC + opt-in Ingress; CLI [`run_apply`] /
//!     [`run_destroy`] branch on [`DeployTarget`] and shell out
//!     to `helm upgrade --install` / `helm uninstall`.
//!   - **Fri:** CI `k8s-smoke` matrix axis on kind; new
//!     `examples/support-agent-k8s.ais` fixture; README + LANGUAGE-
//!     SPEC update.
//!
//! See PLAN-90-days-v0_2.md "Phase 2 Week 14" table for the locked
//! DoDs + the 8 architectural decisions (Decision 43.1–43.8) that
//! shape the resource mapping.

use aislang_catalog::Catalog;
use aislang_resolver::ResolvedProject;
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;

// ============================================================================
// Public types
// ============================================================================

/// Bundle of files emitted per plan run under `target = kubernetes`.
///
/// **Two views of the same resources, packaged for two consumption
/// flows:**
///
///   - `manifests`: a `BTreeMap<filename, yaml-body>` of standalone
///     K8s YAML files (`litellm.yaml`, `jaeger.yaml`, `agent-runtime.
///     yaml`, ...).  Users can `kubectl apply -f manifests/` if they
///     prefer the imperative path.  `aislang plan` writes these to
///     `<output_dir>/manifests/`.
///   - `chart_files`: a `BTreeMap<relative_path, file-body>` representing
///     a Helm chart directory tree (`Chart.yaml`, `values.yaml`,
///     `templates/*.yaml`).  `aislang apply` writes these to
///     `<output_dir>/chart/` and shells out to `helm upgrade --install
///     <release> ./chart`.
///   - `values_yaml`: a convenience handle to the chart's `values.yaml`
///     content — same string that lands in `chart_files`, surfaced
///     separately so callers can compute `--set` overrides without
///     re-deserialising the chart's contents.
///
/// **Determinism contract** (locked Decision 43): output is built
/// from typed structs via `serde_yaml::to_string`; `BTreeMap` keys
/// give deterministic ordering across plan runs.  Re-running
/// `aislang plan` on unchanged input + lockfile produces byte-
/// identical K8sArtifacts.  Verified by a Phase 2 Week 15 Mon
/// regression test.
pub struct K8sArtifacts {
    /// Raw K8s manifests — one YAML body per service/resource.
    /// Empty in Mon's scaffolding; populated Tue (LiteLLM/Jaeger/
    /// budget_counter) + Wed (mcp-serve + prompt ConfigMaps) +
    /// Thu (Open WebUI when `expose.web_ui = true`).
    pub manifests:   BTreeMap<String, String>,

    /// Helm chart files keyed by RELATIVE path inside `chart/`
    /// (e.g. `Chart.yaml`, `values.yaml`, `templates/litellm.yaml`).
    /// Empty in Mon's scaffolding; populated Thu.
    pub chart_files: BTreeMap<PathBuf, String>,

    /// The chart's `values.yaml` content surfaced for convenience.
    /// Same string that lands in `chart_files["values.yaml"]`.
    /// Mon's scaffolding emits a single-line placeholder; Thu adds
    /// the real schema (image.tag, ingress.enabled,
    /// openwebui.persistence, litellm.replicas, ...).
    pub values_yaml: String,
}

// ============================================================================
// Constants — filenames + chart-template-name conventions
// ============================================================================

/// Subdirectory under `<output_dir>` that `aislang plan` writes the
/// raw manifests to.  Mirrors `aislang-compose`'s `STACK_YAML_FILENAME`
/// convention — predictable on-disk layout so `aislang destroy` can
/// find them later.
pub const MANIFESTS_DIRNAME: &str = "manifests";

/// Subdirectory under `<output_dir>` that holds the emitted Helm chart.
/// `aislang apply` shells out `helm upgrade --install <release>
/// <output_dir>/<CHART_DIRNAME>` (Thu).
pub const CHART_DIRNAME: &str = "chart";

/// Chart name as it appears in `Chart.yaml`.  Phase 2 Week 14 locks
/// "one chart per `deploy` block" (Decision 43.1); the chart name
/// is the deploy block's label (e.g. `local` → `Chart.yaml.name =
/// "local"`).  Constant kept here for grep-ability + a future
/// chart-namespace flag if we add multi-deploy-per-project support
/// in v0.3+.
pub const CHART_NAME_KEY: &str = "name";

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Error)]
pub enum EmitError {
    /// serde_yaml serialisation failure — unreachable in practice
    /// (our typed structs all derive `Serialize`), kept here for
    /// the symmetric `From` impl with `aislang-compose::EmitError`.
    #[error("YAML serialization failed: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// Re-export from the existing LiteLLM config generator —
    /// `aislang-compose::litellm::generate_config` is the single
    /// source of truth for the LiteLLM YAML across both deploy
    /// targets.  Wired Tue.
    #[error("LiteLLM config generation failed: {0}")]
    LitellmConfig(#[from] aislang_compose::litellm::ConfigError),
}

// ============================================================================
// Public entry point
// ============================================================================

/// Emit Kubernetes artifacts (raw manifests + Helm chart) for the
/// resolved project's `deploy { target = kubernetes }` block.
///
/// **Phase 2 Week 14 Mon scope (this commit):** returns a skeleton
/// `K8sArtifacts` with empty `manifests` + `chart_files` and a
/// placeholder `values_yaml`.  Lets the CLI's `run_apply` branch on
/// [`aislang_core::types::DeployTarget`] cleanly + the workspace
/// stay green while Tue–Fri populate the actual resource shapes.
///
/// `_catalog` is accepted (not used yet) so Tue's LiteLLM mapping
/// can pull `litellm_name` from it without changing the signature.
pub fn emit(
    _resolved: &ResolvedProject,
    _catalog:  &Catalog,
) -> Result<K8sArtifacts, EmitError> {
    // Placeholder values.yaml so consumers can serialize the
    // artifacts to disk + run `helm lint` (even on Mon — a chart
    // with no templates is technically valid, just useless).
    // Thu rewrites this with the real schema.
    let values_yaml = "\
# values.yaml — placeholder for Phase 2 Week 14 Mon scaffold.
# Real schema (image.tag, ingress.enabled, openwebui.persistence,
# litellm.replicas, …) lands Thu — see crate-level doc.
".to_string();

    Ok(K8sArtifacts {
        manifests:   BTreeMap::new(),
        chart_files: BTreeMap::new(),
        values_yaml,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use aislang_core::ast::Project;
    use std::collections::BTreeMap as Btm;

    /// Synthetic empty ResolvedProject for skeleton tests — doesn't
    /// need any catalog lookups + doesn't need an actual demo file
    /// on disk.  Replaced Tue when real emitter logic needs real
    /// resolved data.
    fn empty_resolved() -> ResolvedProject {
        ResolvedProject {
            project:         Project { name: "skeleton".into(), ..Project::default() },
            catalog_version: "test".into(),
            model_bindings:  Btm::new(),
            prompt_hashes:   Btm::new(),
        }
    }

    #[test] fn emit_returns_skeleton_for_empty_project() {
        let catalog = aislang_catalog::bundled().expect("bundled catalog");
        let resolved = empty_resolved();
        let artifacts = emit(&resolved, &catalog).expect("emit skeleton");
        assert!(artifacts.manifests.is_empty(),
            "Mon scaffold emits no manifests yet; got {:?}",
            artifacts.manifests.keys().collect::<Vec<_>>());
        assert!(artifacts.chart_files.is_empty(),
            "Mon scaffold emits no chart files yet");
        assert!(artifacts.values_yaml.contains("placeholder"),
            "Mon scaffold values.yaml is a placeholder; got:\n{}",
            artifacts.values_yaml);
    }

    #[test] fn manifests_dirname_and_chart_dirname_are_stable() {
        // Pin the on-disk layout these constants drive so downstream
        // tooling (`aislang plan`, `aislang destroy`, the CI smoke
        // job) can trust the names.  Cosmetic change = visible diff
        // = explicit review.
        assert_eq!(MANIFESTS_DIRNAME, "manifests");
        assert_eq!(CHART_DIRNAME,     "chart");
    }
}

//! Kubernetes emitter for aiSlang projects — Phase 2 Track B.
//!
//! Takes a [`ResolvedProject`] (post-validate, post-resolve, post-
//! lockfile-reconciliation) and emits the matching Kubernetes
//! manifests **and** a Helm chart wrapper.  Two views of the same
//! resources — users `kubectl apply -f manifests/` or
//! `helm upgrade --install <release> ./chart`; `aislang apply` uses
//! the Helm path so `aislang destroy` can `helm uninstall` cleanly.
//!
//! ## Phase 2 Week 14 progress
//!
//!   - **Mon**: crate scaffold + [`K8sArtifacts`] type + skeleton
//!     [`emit`] + parser/grammar acceptance of `target = kubernetes`
//!     + release-workflow image-publish job.
//!   - **Tue (this commit)**: LiteLLM + Jaeger + budget_counter as
//!     Deployment + Service + (LiteLLM-only) ConfigMap.  aislang labels.
//!     envFrom for provider creds (Decision 43.5).  manifests/ map
//!     populated; chart/ map still empty (Thu lands the wrapper).
//!   - **Wed**: mcp-serve runtime pod + prompt ConfigMaps; `.ais`
//!     source + `aislang.lock` baked into an `aislang-project`
//!     ConfigMap; `expose.port` becomes the mcp-serve Service port.
//!   - **Thu**: Helm chart wrapper around Tue+Wed manifests;
//!     `expose { web_ui = true }` → Open WebUI Deployment + Service +
//!     PVC + opt-in Ingress; CLI `run_apply` / `run_destroy` branch
//!     on [`DeployTarget`] and shell out to `helm upgrade --install` /
//!     `helm uninstall`.
//!   - **Fri**: CI `k8s-smoke` matrix axis on kind; new
//!     `examples/support-agent-k8s.ais` fixture; README + LANGUAGE-
//!     SPEC update.
//!
//! See PLAN-90-days-v0_2.md "Phase 2 Week 14" table for the locked
//! DoDs + the 8 architectural decisions (Decision 43.1–43.8) that
//! shape the resource mapping.

pub mod k8s_types;
pub mod labels;
pub mod services;

use aislang_catalog::Catalog;
use aislang_core::ast::Deploy;
use aislang_core::types::DeployTarget;
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
    /// Tue populates `litellm.yaml`, `jaeger.yaml`, `budget-
    /// counter.yaml`; Wed adds `agent-runtime.yaml` + `aislang-
    /// project-config.yaml`; Thu adds `openwebui.yaml` (when
    /// `expose.web_ui = true`) + opt-in `ingress.yaml`.
    pub manifests:   BTreeMap<String, String>,

    /// Helm chart files keyed by RELATIVE path inside `chart/`
    /// (e.g. `Chart.yaml`, `values.yaml`, `templates/litellm.yaml`).
    /// Empty in Mon+Tue's output; populated Thu.
    pub chart_files: BTreeMap<PathBuf, String>,

    /// The chart's `values.yaml` content surfaced for convenience.
    /// Same string that lands in `chart_files["values.yaml"]`.
    /// Mon+Tue emit a single-line placeholder; Thu adds the real
    /// schema (image.tag, ingress.enabled, openwebui.persistence,
    /// litellm.replicas, ...).
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

/// Filenames inside `manifests/` — pinned so downstream tooling
/// (`aislang destroy`, the Fri k8s-smoke CI job, debugging humans)
/// can find them by name.
pub const LITELLM_MANIFEST_FILENAME:               &str = "litellm.yaml";
pub const JAEGER_MANIFEST_FILENAME:                &str = "jaeger.yaml";
pub const BUDGET_COUNTER_MANIFEST_FILENAME:        &str = "budget-counter.yaml";
/// Wed (Decision 43.3) — agent-runtime Deployment + Service.
pub const AGENT_RUNTIME_MANIFEST_FILENAME:         &str = "agent-runtime.yaml";
/// Wed (Decision 43.4) — `aislang-project` ConfigMap (.ais + lockfile + prompts).
pub const AISLANG_PROJECT_CM_MANIFEST_FILENAME:    &str = "aislang-project-config.yaml";

// ============================================================================
// Inputs the emitter needs beyond ResolvedProject + Catalog
// ============================================================================

/// Bag of source-level inputs that don't live in `ResolvedProject` but
/// that the K8s emitter needs to bake into ConfigMaps (Decision 43.4,
/// Phase 2 Week 14 Wed).
///
/// **Why a separate struct rather than reaching back to disk inside
/// `emit()`**: the emitter stays a pure data-transform — no IO, no
/// surprises in test fixtures, easy mocking from synthetic
/// `ProjectSources` values in unit tests.  The CLI prepares these
/// once (it already has the source string in scope from the `validate`
/// stage, the lockfile body from the `lockfile reconcile` stage,
/// and trivially-loadable prompt bodies based on
/// `Project.prompts[i].file`).
#[derive(Debug, Default, Clone)]
pub struct ProjectSources {
    /// Verbatim .ais source text.  Lands in the `aislang-project`
    /// ConfigMap under key `project.ais`, mounted at
    /// `/etc/aislang/project.ais` inside the agent-runtime pod.
    pub ais_source: String,

    /// Lockfile body (`aislang.lock` content).  `None` when the
    /// project hasn't been planned before OR when the CLI was run
    /// with `--no-lock`.  Stored as the empty string in the
    /// ConfigMap in that case so downstream consumers don't have
    /// to special-case the absence.
    pub lockfile: Option<String>,

    /// Prompt body bytes keyed by `prompt.label`.  One CM entry +
    /// one mounted file per entry (at `/etc/aislang/prompts/<label>`).
    /// Empty map is fine — projects with no prompts skip the
    /// prompts/ sub-directory entirely.
    pub prompt_bodies: BTreeMap<String, String>,
}

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
/// **Phase 2 Week 14 Tue+Wed scope (this commit):** populates
/// `K8sArtifacts.manifests` with LiteLLM + Jaeger + budget_counter
/// + agent-runtime Deployments/Services + the LiteLLM router CM +
/// the `aislang-project` CM (Decisions 43.2 + 43.3 + 43.4).
/// `chart_files` stays empty until Thu (Decision 43.7); `values_yaml`
/// keeps the Mon+Tue placeholder.
///
/// **`deploy` block selection** (Decision 43.1): the emitter picks
/// the FIRST `deploy` block whose `target == Kubernetes`.  Multiple
/// k8s `deploy` blocks per project = v0.3 (would need one chart per
/// deploy block + a per-block output sub-directory).  No k8s
/// `deploy` block at all = the emitter still works but uses
/// `"default"` as the deploy label so labels stay populated.
///
/// **`sources` parameter** (Wed): bag of `.ais source` + lockfile +
/// per-prompt bodies the agent-runtime pod's ConfigMap needs.  See
/// [`ProjectSources`] for the rationale of keeping these out of
/// `ResolvedProject` proper.
pub fn emit(
    resolved: &ResolvedProject,
    catalog:  &Catalog,
    sources:  &ProjectSources,
) -> Result<K8sArtifacts, EmitError> {
    let deploy_label = pick_k8s_deploy_label(resolved);

    // Single source of truth for the LiteLLM router YAML — same
    // generator used by `aislang-compose::emit`.  Lands in the
    // litellm-config ConfigMap as the value of key `config.yaml`.
    let router_yaml =
        aislang_compose::litellm::generate_config(resolved, catalog)?;

    let mut manifests: BTreeMap<String, String> = BTreeMap::new();

    // ── LiteLLM: Deployment + Service + ConfigMap (one YAML file,
    //    three resources joined by `---`).
    let (litellm_dep, litellm_svc, litellm_cm) =
        services::build_litellm(resolved, &deploy_label, &router_yaml);
    manifests.insert(
        LITELLM_MANIFEST_FILENAME.to_string(),
        join_manifests(&[
            ("Deployment", serde_yaml::to_string(&litellm_dep)?),
            ("Service",    serde_yaml::to_string(&litellm_svc)?),
            ("ConfigMap",  serde_yaml::to_string(&litellm_cm)?),
        ]),
    );

    // ── Jaeger: Deployment + Service.  No CM — bundled defaults.
    let (jaeger_dep, jaeger_svc) = services::build_jaeger(&deploy_label);
    manifests.insert(
        JAEGER_MANIFEST_FILENAME.to_string(),
        join_manifests(&[
            ("Deployment", serde_yaml::to_string(&jaeger_dep)?),
            ("Service",    serde_yaml::to_string(&jaeger_svc)?),
        ]),
    );

    // ── budget_counter: Deployment only.  No Service (no incoming
    //    traffic; sleep-infinity placeholder).
    let bc_dep = services::build_budget_counter(resolved, &deploy_label);
    manifests.insert(
        BUDGET_COUNTER_MANIFEST_FILENAME.to_string(),
        join_manifests(&[
            ("Deployment", serde_yaml::to_string(&bc_dep)?),
        ]),
    );

    // ── agent-runtime: Deployment + Service (Decision 43.3).  Pod
    //    runs `aislang mcp-serve --http 0.0.0.0:<expose.port>
    //    /etc/aislang/project.ais`; Service ClusterIP exposes the
    //    same port.  Per-prompt + project-fingerprint annotations
    //    on the pod template drive ReplicaSet rollouts on any input
    //    change (Decision 43.4).
    let (agent_dep, agent_svc) = services::build_agent_runtime(
        resolved,
        &deploy_label,
        &sources.ais_source,
        sources.lockfile.as_deref(),
    );
    manifests.insert(
        AGENT_RUNTIME_MANIFEST_FILENAME.to_string(),
        join_manifests(&[
            ("Deployment", serde_yaml::to_string(&agent_dep)?),
            ("Service",    serde_yaml::to_string(&agent_svc)?),
        ]),
    );

    // ── aislang-project ConfigMap (Decision 43.4) — `.ais` source +
    //    lockfile + every prompt body, mounted at /etc/aislang/ on
    //    the agent-runtime pod.  Separate manifest file so a
    //    `kubectl apply -f manifests/aislang-project-config.yaml`
    //    can refresh the project content without touching the
    //    Deployment YAML.
    let project_cm = services::build_aislang_project_configmap(
        &deploy_label,
        &sources.ais_source,
        sources.lockfile.as_deref(),
        &sources.prompt_bodies,
    );
    manifests.insert(
        AISLANG_PROJECT_CM_MANIFEST_FILENAME.to_string(),
        join_manifests(&[
            ("ConfigMap", serde_yaml::to_string(&project_cm)?),
        ]),
    );

    // Placeholder values.yaml — Thu rewrites with real schema.
    let values_yaml = "\
# values.yaml — placeholder for Phase 2 Week 14 Mon+Tue scaffold.
# Real schema (image.tag, ingress.enabled, openwebui.persistence,
# litellm.replicas, …) lands Thu — see crate-level doc.
".to_string();

    Ok(K8sArtifacts {
        manifests,
        chart_files: BTreeMap::new(),
        values_yaml,
    })
}

// ============================================================================
// Helpers
// ============================================================================

/// Find the first `deploy` block with `target = kubernetes` and
/// return its label.  Falls back to `"default"` if no k8s deploy
/// block exists — keeps labels populated so the emitter can
/// produce a valid (if not deploy-block-attributed) artifact set
/// even on projects that haven't switched to k8s yet.  Multi-k8s-
/// deploys = v0.3+ (Decision 43.1 locks one chart per deploy
/// block; would also need a per-block output sub-directory).
fn pick_k8s_deploy_label(resolved: &ResolvedProject) -> String {
    resolved.project.deploys.iter()
        .find(|d: &&Deploy| d.target == DeployTarget::Kubernetes)
        .map(|d| d.label.clone())
        .unwrap_or_else(|| "default".to_string())
}

/// Join multiple K8s manifest YAML strings into a single file
/// body using the standard `---` document-separator.  A leading
/// `---` is added so the file's first document is unambiguously
/// a new document (some tools require it; `kubectl apply -f` is
/// permissive but `helm template` lints with `---` leading).
///
/// `kind` is unused at runtime but documents the call site —
/// future per-resource pre/post hooks (sorting? Helm-style
/// labels-merge?) can switch on it without refactoring callers.
fn join_manifests(parts: &[(&str, String)]) -> String {
    let mut out = String::new();
    for (_kind, yaml) in parts {
        out.push_str("---\n");
        // serde_yaml emits a trailing newline; we don't double up.
        let trimmed = yaml.trim_end();
        out.push_str(trimmed);
        out.push('\n');
    }
    out
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use aislang_core::ast::{Deploy, Project};
    use aislang_core::types::DeployTarget;
    use std::collections::BTreeMap as Btm;

    /// Synthetic empty ResolvedProject for skeleton tests — doesn't
    /// need any catalog lookups beyond what generate_config does.
    /// Used by both the Mon-style "no-resources" tests and the
    /// Tue "deploy-label propagation" tests.
    fn empty_resolved() -> ResolvedProject {
        ResolvedProject {
            project:         Project { name: "skeleton".into(), ..Project::default() },
            catalog_version: "test".into(),
            model_bindings:  Btm::new(),
            prompt_hashes:   Btm::new(),
        }
    }

    fn resolved_with_k8s_deploy(label: &str) -> ResolvedProject {
        ResolvedProject {
            project: Project {
                name: "x".into(),
                deploys: vec![Deploy {
                    label:  label.into(),
                    target: DeployTarget::Kubernetes,
                    expose: None,
                }],
                ..Project::default()
            },
            catalog_version: "test".into(),
            model_bindings:  Btm::new(),
            prompt_hashes:   Btm::new(),
        }
    }

    /// Empty `ProjectSources` — Tue's tests don't exercise the
    /// agent-runtime CM content, so a default-empty bag keeps the
    /// existing emit() calls compiling against the Wed-extended
    /// signature.  Wed-specific tests below build richer
    /// `ProjectSources` values to exercise the new code paths.
    fn empty_sources() -> ProjectSources {
        ProjectSources::default()
    }

    // ────────────────────────────────────────────────────────────
    // Mon scaffold invariants — still hold post-Tue
    // ────────────────────────────────────────────────────────────

    #[test] fn emit_returns_skeleton_shape_for_empty_project() {
        let catalog = aislang_catalog::bundled().expect("bundled catalog");
        let resolved = empty_resolved();
        let artifacts = emit(&resolved, &catalog, &empty_sources()).expect("emit");
        // Wed: manifests now 5 (Tue's 3 + agent-runtime + project CM).
        // chart_files + values_yaml stay at the Mon defaults.
        assert_eq!(artifacts.manifests.len(), 5,
            "Wed scaffold emits 5 manifest files (litellm/jaeger/budget-counter \
             + agent-runtime + aislang-project-config); got: {:?}",
             artifacts.manifests.keys().collect::<Vec<_>>());
        assert!(artifacts.chart_files.is_empty(),
            "Mon+Tue+Wed scaffold emits no chart files yet (Thu populates)");
        assert!(artifacts.values_yaml.contains("placeholder"),
            "Mon+Tue+Wed scaffold values.yaml is a placeholder; got:\n{}",
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

    // ────────────────────────────────────────────────────────────
    // Tue — Decision 43.2: LiteLLM/Jaeger/budget_counter manifests
    // ────────────────────────────────────────────────────────────

    #[test] fn emits_all_five_manifest_files() {
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        for name in [
            // Tue (Decision 43.2)
            LITELLM_MANIFEST_FILENAME,
            JAEGER_MANIFEST_FILENAME,
            BUDGET_COUNTER_MANIFEST_FILENAME,
            // Wed (Decisions 43.3 + 43.4)
            AGENT_RUNTIME_MANIFEST_FILENAME,
            AISLANG_PROJECT_CM_MANIFEST_FILENAME,
        ] {
            assert!(artifacts.manifests.contains_key(name),
                "manifest `{name}` must be present; got: {:?}",
                artifacts.manifests.keys().collect::<Vec<_>>());
            assert!(!artifacts.manifests[name].is_empty(),
                "manifest `{name}` must have content");
        }
    }

    #[test] fn every_manifest_round_trips_via_serde_yaml() {
        // Structural validity: every emitted manifest file must parse
        // back via serde_yaml.  Multi-document files (`---`-separated)
        // need the split-on-doc-separator approach.  If serde_yaml
        // accepts them, `kubectl --dry-run=client apply -f manifests/`
        // will too.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        for name in artifacts.manifests.keys() {
            // all_resources does the parse + panics on failure
            let resources: Vec<_> = all_resources(&artifacts).into_iter()
                .filter(|(f, _, _)| f == name)
                .collect();
            assert!(!resources.is_empty(),
                "manifest `{name}` must contain ≥1 YAML document");
        }
    }

    /// Collect every K8s resource (one per `---`-doc) across every
    /// manifest file.  Returns `Vec<(filename, kind, yaml_value)>`.
    /// Used by the per-resource assertion tests below.
    fn all_resources(artifacts: &K8sArtifacts) -> Vec<(String, String, serde_yaml::Value)> {
        let mut out = Vec::new();
        for (name, body) in &artifacts.manifests {
            for doc_str in body.split("\n---\n").filter(|s| !s.trim().is_empty()) {
                // Strip the leading "---\n" the first doc gets from
                // our join_manifests helper, and any trailing blank
                // lines from the splitter.
                let trimmed = doc_str.trim_start_matches("---").trim_start_matches('\n');
                if trimmed.trim().is_empty() { continue; }
                let v: serde_yaml::Value = serde_yaml::from_str(trimmed)
                    .unwrap_or_else(|e| panic!(
                        "manifest `{name}` doc fails to parse: {e}\n\n{trimmed}"));
                let kind = v["kind"].as_str().unwrap_or("?").to_string();
                out.push((name.clone(), kind, v));
            }
        }
        out
    }

    #[test] fn litellm_emits_deployment_service_configmap() {
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        let kinds: std::collections::BTreeSet<String> = all_resources(&artifacts).into_iter()
            .filter(|(f, _, _)| f == LITELLM_MANIFEST_FILENAME)
            .map(|(_, k, _)| k)
            .collect();
        assert_eq!(kinds,
            std::collections::BTreeSet::from([
                "Deployment".to_string(),
                "Service".to_string(),
                "ConfigMap".to_string(),
            ]),
            "litellm.yaml must contain Deployment+Service+ConfigMap");
    }

    #[test] fn jaeger_emits_deployment_and_service_only() {
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        let kinds: std::collections::BTreeSet<String> = all_resources(&artifacts).iter()
            .filter(|(f, _, _)| f == JAEGER_MANIFEST_FILENAME)
            .map(|(_, k, _)| k.clone())
            .collect();
        assert_eq!(kinds,
            std::collections::BTreeSet::from(["Deployment".to_string(), "Service".to_string()]),
            "jaeger.yaml: Deployment+Service only (bundled defaults; no CM)");
    }

    #[test] fn budget_counter_emits_deployment_only() {
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        let kinds: std::collections::BTreeSet<String> = all_resources(&artifacts).iter()
            .filter(|(f, _, _)| f == BUDGET_COUNTER_MANIFEST_FILENAME)
            .map(|(_, k, _)| k.clone())
            .collect();
        assert_eq!(kinds,
            std::collections::BTreeSet::from(["Deployment".to_string()]),
            "budget-counter.yaml: Deployment only (no incoming traffic)");
    }

    #[test] fn every_resource_carries_aislang_labels() {
        // Decision 43 sentinel: every emitted resource (across every
        // manifest file) must have `aislang.generated=true` +
        // `aislang.component=<name>` + `aislang.deploy=<label>`.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = resolved_with_k8s_deploy("prod");
        let artifacts = emit(&resolved, &catalog, &empty_sources()).expect("emit");
        for (file, kind, v) in all_resources(&artifacts) {
            let labels = &v["metadata"]["labels"];
            assert_eq!(labels["aislang.generated"].as_str(), Some("true"),
                "{file}/{kind}: aislang.generated label missing");
            assert!(labels["aislang.component"].as_str().is_some(),
                "{file}/{kind}: aislang.component label missing");
            assert_eq!(labels["aislang.deploy"].as_str(), Some("prod"),
                "{file}/{kind}: aislang.deploy label must match deploy block label");
        }
    }

    #[test] fn deploy_label_defaults_when_no_k8s_deploy_block() {
        // Project with no `deploy { target = kubernetes }` block:
        // emitter still produces manifests but stamps them with the
        // `"default"` label so the label set is well-formed.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        for (_, _, v) in all_resources(&artifacts) {
            assert_eq!(v["metadata"]["labels"]["aislang.deploy"].as_str(),
                Some("default"),
                "no-deploy-block → `default` label fallback");
        }
    }

    #[test] fn litellm_pod_envfroms_aislang_providers_secret() {
        // Decision 43.5: provider creds via envFrom → user-created
        // Secret named `aislang-providers`.  The container spec
        // MUST list it as an envFrom entry; absence here = silent
        // empty-string creds at runtime = clean-looking pod that
        // fails on first /v1/chat/completions call.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        let litellm_dep = all_resources(&artifacts).into_iter()
            .find(|(f, k, _)| f == LITELLM_MANIFEST_FILENAME && k == "Deployment")
            .expect("litellm Deployment present");
        let env_from = &litellm_dep.2["spec"]["template"]["spec"]
            ["containers"][0]["envFrom"];
        let seq = env_from.as_sequence().expect("envFrom is a list");
        let names: Vec<&str> = seq.iter()
            .filter_map(|e| e["secretRef"]["name"].as_str())
            .collect();
        assert!(names.contains(&services::PROVIDERS_SECRET_NAME),
            "litellm Deployment must envFrom secretRef `{}`; got names: {:?}",
            services::PROVIDERS_SECRET_NAME, names);
    }

    #[test] fn jaeger_pod_does_not_envfrom_providers_secret() {
        // Jaeger doesn't dial providers — no envFrom needed.  Keep the
        // secret-reference surface minimal so an audit of "which pods
        // can read the provider Secret" returns the smallest possible
        // set (LiteLLM + Wed's agent-runtime; not Jaeger).
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        let jaeger_dep = all_resources(&artifacts).into_iter()
            .find(|(f, k, _)| f == JAEGER_MANIFEST_FILENAME && k == "Deployment")
            .expect("jaeger Deployment present");
        let env_from = &jaeger_dep.2["spec"]["template"]["spec"]
            ["containers"][0]["envFrom"];
        assert!(env_from.is_null(),
            "jaeger must not envFrom anything (no provider dialing); got: {env_from:?}");
    }

    #[test] fn litellm_configmap_contains_router_yaml_under_config_key() {
        // The router YAML produced by aislang-compose's generate_config
        // lands in the litellm-config ConfigMap as the value of key
        // `config.yaml`.  The container mounts it at /app/config.yaml
        // via volumeMounts.subPath.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        let cm = all_resources(&artifacts).into_iter()
            .find(|(f, k, _)| f == LITELLM_MANIFEST_FILENAME && k == "ConfigMap")
            .expect("litellm-config CM present");
        let body = cm.2["data"]["config.yaml"].as_str()
            .expect("CM data[config.yaml] is a string");
        // The router YAML always carries `model_list:` (even when
        // empty) per aislang-compose's generator contract.
        assert!(body.contains("model_list:"),
            "ConfigMap data[config.yaml] must contain the router YAML; got:\n{body}");
    }

    #[test] fn budget_counter_carries_cap_envs_when_budget_set() {
        use aislang_core::ast::{Project, Budget};
        use aislang_core::types::{Money, Currency, OptimizeFor};
        let project = Project {
            name: "x".into(),
            budget: Some(Budget {
                hard_cap_per_day: Money::Capped { currency: Currency::Usd, micro_units: 50_000_000 },
                hard_cap_per_run: Money::Capped { currency: Currency::Usd, micro_units: 1_000_000 },
                optimize_for:     OptimizeFor::Cost,
            }),
            ..Project::default()
        };
        let resolved = ResolvedProject {
            project,
            catalog_version: "test".into(),
            model_bindings:  Btm::new(),
            prompt_hashes:   Btm::new(),
        };
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&resolved, &catalog, &empty_sources()).expect("emit");
        let dep = all_resources(&artifacts).into_iter()
            .find(|(f, k, _)| f == BUDGET_COUNTER_MANIFEST_FILENAME && k == "Deployment")
            .expect("budget-counter Deployment present");
        let env = &dep.2["spec"]["template"]["spec"]["containers"][0]["env"];
        let pairs: Vec<(String, String)> = env.as_sequence().expect("env list")
            .iter()
            .map(|e| (
                e["name"].as_str().unwrap_or("").to_string(),
                e["value"].as_str().unwrap_or("").to_string(),
            ))
            .collect();
        assert!(pairs.iter().any(|(n, v)|
                n == "AISLANG_HARD_CAP_PER_DAY_MICRO" && v == "50000000"),
            "per-day cap missing or wrong; got: {pairs:?}");
        assert!(pairs.iter().any(|(n, v)|
                n == "AISLANG_HARD_CAP_PER_RUN_MICRO" && v == "1000000"),
            "per-run cap missing or wrong; got: {pairs:?}");
    }

    #[test] fn services_select_pods_by_component_label_only() {
        // Decision: Service selector is the minimal `aislang.
        // component=<name>` form (not the full label set) so
        // ad-hoc label additions don't break pod-to-service
        // matching.  Pin the rule via the selector field shape.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        for (file, kind, v) in all_resources(&artifacts) {
            if kind != "Service" { continue; }
            let selector = v["spec"]["selector"].as_mapping()
                .unwrap_or_else(|| panic!(
                    "{file}/{kind}: Service must have a selector mapping"));
            let keys: std::collections::BTreeSet<&str> = selector.keys()
                .filter_map(|k| k.as_str())
                .collect();
            assert_eq!(keys, std::collections::BTreeSet::from(["aislang.component"]),
                "{file}/{kind}: selector must be component-only \
                 (see labels::component_selector rationale); got: {keys:?}");
        }
    }

    #[test] fn deploys_match_their_services_via_component_label() {
        // Round-trip: every Deployment's pod-template labels MUST
        // include the same `aislang.component=<X>` value that its
        // sibling Service selects on.  Otherwise the Service has
        // no endpoints and traffic black-holes.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        // Build a (filename, component-label) map from Services.
        let services_by_component: BTreeMap<String, String> =
            all_resources(&artifacts).iter()
                .filter(|(_, k, _)| k == "Service")
                .map(|(f, _, v)| (
                    v["spec"]["selector"]["aislang.component"]
                        .as_str().unwrap_or("").to_string(),
                    f.clone(),
                ))
                .collect();
        // Verify each Deployment in those files has a matching
        // pod-template label.
        for (file, kind, v) in all_resources(&artifacts) {
            if kind != "Deployment" { continue; }
            // Skip if there's no sibling Service in this file
            // (budget-counter has no Service — that's correct).
            let Some((component, _)) = services_by_component.iter()
                .find(|(_, f)| **f == file)
            else { continue };
            let pod_label = v["spec"]["template"]["metadata"]["labels"]
                ["aislang.component"].as_str();
            assert_eq!(pod_label, Some(component.as_str()),
                "{file}/Deployment pod-template label aislang.component \
                 must match sibling Service selector; got pod-label \
                 {pod_label:?} vs selector `{component}`");
        }
    }

    #[test] fn manifest_files_lead_with_yaml_document_separator() {
        // `helm template` lint + many CI pipelines require each
        // document to lead with `---` for unambiguous parsing.
        // Pin the rule so a future "save 4 bytes by dropping the
        // leading ---" attempt fails loudly here.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        for (name, body) in &artifacts.manifests {
            assert!(body.starts_with("---\n"),
                "manifest `{name}` must lead with `---\\n`; got:\n{body}");
        }
    }

    /// Visual-inspection helper — run with
    /// `cargo test -p aislang-k8s dump_demo_manifests -- --nocapture --ignored`
    /// to eyeball the emitted YAML against `kubectl --dry-run=client`.
    /// Ignored so it doesn't bloat the default CI run; kept in-source
    /// so the Fri k8s-smoke job can re-use it for golden-file diffing.
    #[test]
    #[ignore]
    fn dump_demo_manifests() {
        let resolved = resolved_with_k8s_deploy("prod");
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&resolved, &catalog, &empty_sources()).expect("emit");
        for (name, body) in &artifacts.manifests {
            println!("================ {name} ================");
            println!("{body}");
        }
    }

    #[test] fn emit_is_deterministic_across_runs() {
        // Decision 43 determinism contract: same input → byte-identical
        // output.  Run emit() twice on the same ResolvedProject + same
        // catalog and compare every manifest body byte-for-byte.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = resolved_with_k8s_deploy("prod");
        let a = emit(&resolved, &catalog, &empty_sources()).expect("emit 1");
        let b = emit(&resolved, &catalog, &empty_sources()).expect("emit 2");
        assert_eq!(a.manifests.keys().collect::<Vec<_>>(),
                   b.manifests.keys().collect::<Vec<_>>(),
            "manifest filenames must match across runs");
        for (k, va) in &a.manifests {
            let vb = &b.manifests[k];
            assert_eq!(va, vb,
                "manifest `{k}` content must be byte-identical across \
                 runs; left/right diverge");
        }
    }

    // ────────────────────────────────────────────────────────────
    // Wed — Decision 43.3 + 43.4: agent-runtime + project ConfigMap
    // ────────────────────────────────────────────────────────────

    /// Synthetic ResolvedProject with a k8s deploy block + a prompt
    /// hash entry.  Used by Wed tests to exercise the annotation +
    /// CM-keys + items[] mount paths against a non-trivial input.
    fn resolved_with_prompt(deploy_label: &str, prompt_label: &str, hash: &str)
        -> ResolvedProject
    {
        use aislang_core::ast::{Expose, Prompt};
        use aislang_core::types::SemVer;
        let mut prompt_hashes = Btm::new();
        prompt_hashes.insert(prompt_label.to_string(), hash.to_string());
        ResolvedProject {
            project: Project {
                name: "x".into(),
                prompts: vec![Prompt {
                    label:   prompt_label.into(),
                    file:    "./prompts/system.md".into(),
                    version: SemVer::from_literal("1.0.0").expect("semver"),
                }],
                deploys: vec![Deploy {
                    label:  deploy_label.into(),
                    target: DeployTarget::Kubernetes,
                    expose: Some(Expose { port: Some(9000), web_ui: false }),
                }],
                ..Project::default()
            },
            catalog_version: "test".into(),
            model_bindings:  Btm::new(),
            prompt_hashes,
        }
    }

    fn sources_with(
        ais_source: &str,
        lockfile:   Option<&str>,
        prompts:    &[(&str, &str)],
    ) -> ProjectSources {
        let mut bodies = Btm::new();
        for (label, body) in prompts {
            bodies.insert(label.to_string(), body.to_string());
        }
        ProjectSources {
            ais_source:    ais_source.to_string(),
            lockfile:      lockfile.map(String::from),
            prompt_bodies: bodies,
        }
    }

    #[test] fn agent_runtime_deployment_and_service_emit() {
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = resolved_with_prompt("prod", "system", "deadbeef");
        let sources = sources_with("project \"x\" {}", Some("lock-body"), &[("system", "you are helpful")]);
        let artifacts = emit(&resolved, &catalog, &sources).expect("emit");
        let kinds: std::collections::BTreeSet<String> = all_resources(&artifacts).into_iter()
            .filter(|(f, _, _)| f == AGENT_RUNTIME_MANIFEST_FILENAME)
            .map(|(_, k, _)| k)
            .collect();
        assert_eq!(kinds,
            std::collections::BTreeSet::from([
                "Deployment".to_string(),
                "Service".to_string(),
            ]),
            "agent-runtime.yaml must contain Deployment + Service");
    }

    #[test] fn agent_runtime_pod_uses_expose_port() {
        // Decision 43.3: `expose.port` becomes the agent-runtime Service
        // port + the container port + the --http bind address arg.  Pin
        // all three points so a future refactor that desyncs them fails.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = resolved_with_prompt("prod", "system", "abc");
        let sources = sources_with("p", None, &[]);
        let artifacts = emit(&resolved, &catalog, &sources).expect("emit");
        let resources = all_resources(&artifacts);

        // Service: spec.ports[0].port == 9000
        let svc = resources.iter()
            .find(|(f, k, _)| f == AGENT_RUNTIME_MANIFEST_FILENAME && k == "Service")
            .expect("agent-runtime Service");
        let svc_port = svc.2["spec"]["ports"][0]["port"].as_i64()
            .expect("Service ports[0].port is int");
        assert_eq!(svc_port, 9000, "Service port must match expose.port");

        // Deployment: container port + --http arg both 9000.
        let dep = resources.iter()
            .find(|(f, k, _)| f == AGENT_RUNTIME_MANIFEST_FILENAME && k == "Deployment")
            .expect("agent-runtime Deployment");
        let container = &dep.2["spec"]["template"]["spec"]["containers"][0];
        let cport = container["ports"][0]["containerPort"].as_i64()
            .expect("containerPort is int");
        assert_eq!(cport, 9000, "containerPort must match expose.port");
        let args: Vec<&str> = container["args"].as_sequence().expect("args list")
            .iter().filter_map(|v| v.as_str()).collect();
        assert!(args.contains(&"0.0.0.0:9000"),
            "args must include the --http bind `0.0.0.0:9000`; got: {args:?}");
        assert!(args.contains(&"mcp-serve"),
            "args must include the mcp-serve subcommand; got: {args:?}");
        // The .ais source path positional arg.
        assert!(args.iter().any(|a| a.ends_with("/project.ais")),
            "args must include /etc/aislang/project.ais path; got: {args:?}");
    }

    #[test] fn agent_runtime_defaults_to_port_8080_when_expose_absent() {
        // No `expose` block → fallback to 8080 (matches aislang-compose's
        // informational header line for parity).
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = resolved_with_k8s_deploy("prod"); // expose: None
        let artifacts = emit(&resolved, &catalog, &empty_sources()).expect("emit");
        let dep = all_resources(&artifacts).into_iter()
            .find(|(f, k, _)| f == AGENT_RUNTIME_MANIFEST_FILENAME && k == "Deployment")
            .expect("agent-runtime Deployment");
        let cport = dep.2["spec"]["template"]["spec"]["containers"][0]
            ["ports"][0]["containerPort"].as_i64().expect("containerPort int");
        assert_eq!(cport, services::AGENT_RUNTIME_DEFAULT_PORT as i64,
            "no expose → fallback to AGENT_RUNTIME_DEFAULT_PORT");
    }

    #[test] fn agent_runtime_envfroms_aislang_providers_secret() {
        // Decision 43.5: agent-runtime ALSO needs provider creds (it
        // dials LiteLLM AND, for some fallback configurations, hits
        // providers directly).  Same Secret reference as the litellm
        // pod (single source of truth on the user's side).
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        let dep = all_resources(&artifacts).into_iter()
            .find(|(f, k, _)| f == AGENT_RUNTIME_MANIFEST_FILENAME && k == "Deployment")
            .expect("agent-runtime Deployment");
        let env_from = &dep.2["spec"]["template"]["spec"]
            ["containers"][0]["envFrom"];
        let seq = env_from.as_sequence().expect("envFrom is list");
        let names: Vec<&str> = seq.iter()
            .filter_map(|e| e["secretRef"]["name"].as_str()).collect();
        assert!(names.contains(&services::PROVIDERS_SECRET_NAME),
            "agent-runtime must envFrom the providers Secret; got: {names:?}");
    }

    #[test] fn aislang_project_configmap_contains_source_and_lockfile() {
        // Decision 43.4: the CM bakes the .ais source + lockfile body
        // so the pod has everything it needs to run `aislang mcp-serve`
        // self-contained.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = resolved_with_k8s_deploy("prod");
        let sources = sources_with(
            "project \"my-app\" { }",
            Some("{\"schema_version\": 1}"),
            &[],
        );
        let artifacts = emit(&resolved, &catalog, &sources).expect("emit");
        let cm = all_resources(&artifacts).into_iter()
            .find(|(f, k, _)| f == AISLANG_PROJECT_CM_MANIFEST_FILENAME && k == "ConfigMap")
            .expect("aislang-project CM present");
        assert_eq!(cm.2["metadata"]["name"].as_str(), Some("aislang-project"));
        let ais_body = cm.2["data"]["project.ais"].as_str()
            .expect("data[project.ais] is string");
        assert!(ais_body.contains("my-app"),
            "CM data[project.ais] must hold the .ais source; got:\n{ais_body}");
        let lock_body = cm.2["data"]["aislang.lock"].as_str()
            .expect("data[aislang.lock] is string");
        assert!(lock_body.contains("schema_version"),
            "CM data[aislang.lock] must hold the lockfile body; got:\n{lock_body}");
    }

    #[test] fn aislang_project_cm_has_one_entry_per_prompt() {
        // Each `prompt` block becomes a CM entry keyed by prompt label;
        // the body is the bytes provided via ProjectSources.prompt_bodies.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = resolved_with_prompt("prod", "system", "abc123");
        let sources = sources_with(
            "p",
            None,
            &[
                ("system",     "you are a helpful assistant"),
                ("retrieval",  "search the knowledge base"),
            ],
        );
        let artifacts = emit(&resolved, &catalog, &sources).expect("emit");
        let cm = all_resources(&artifacts).into_iter()
            .find(|(f, k, _)| f == AISLANG_PROJECT_CM_MANIFEST_FILENAME && k == "ConfigMap")
            .expect("CM present");
        for (label, expected_body) in [
            ("system",    "you are a helpful assistant"),
            ("retrieval", "search the knowledge base"),
        ] {
            let body = cm.2["data"][label].as_str()
                .unwrap_or_else(|| panic!("CM missing entry for prompt `{label}`"));
            assert_eq!(body, expected_body,
                "CM entry `{label}` must match the provided prompt body");
        }
    }

    #[test] fn agent_pod_template_carries_prompt_hash_annotation_per_prompt() {
        // Decision 43.4: every prompt's SHA-256 hash lands as a
        // pod-template annotation.  A body change shifts the hash in
        // `ResolvedProject.prompt_hashes` (resolver) → annotation
        // shifts → K8s creates a new ReplicaSet → rolling restart.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = resolved_with_prompt("prod", "system", "deadbeefcafebabe");
        let sources = sources_with("p", None, &[("system", "x")]);
        let artifacts = emit(&resolved, &catalog, &sources).expect("emit");
        let dep = all_resources(&artifacts).into_iter()
            .find(|(f, k, _)| f == AGENT_RUNTIME_MANIFEST_FILENAME && k == "Deployment")
            .expect("agent Deployment");
        let ann = &dep.2["spec"]["template"]["metadata"]["annotations"];
        // Per-prompt: `aislang.prompt-hash/system: deadbeefcafebabe`
        let expected_key = format!("{}system", services::PROMPT_HASH_ANNOTATION_PREFIX);
        assert_eq!(ann[expected_key.as_str()].as_str(), Some("deadbeefcafebabe"),
            "pod-template annotation `{expected_key}` must carry the prompt hash; \
             got annotations: {ann:?}");
        // Project-fingerprint annotation: SHA-256 hex, 64 chars
        let proj_hash = ann[services::PROJECT_HASH_ANNOTATION].as_str()
            .expect("project-hash annotation present");
        assert_eq!(proj_hash.len(), 64,
            "project-hash must be 64-char hex; got `{proj_hash}` ({})",
            proj_hash.len());
        assert!(proj_hash.chars().all(|c| c.is_ascii_hexdigit()),
            "project-hash must be all hex; got `{proj_hash}`");
    }

    #[test] fn project_hash_annotation_shifts_when_source_changes() {
        // Decision 43.4 invariant: changing the .ais source byte shifts
        // the `aislang.project-hash` annotation so a `helm upgrade` /
        // `kubectl apply` triggers a rolling restart.  Compare two
        // emits with different sources.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = resolved_with_k8s_deploy("prod");
        let s1 = sources_with("project \"a\" {}", None, &[]);
        let s2 = sources_with("project \"b\" {}", None, &[]);
        let a = emit(&resolved, &catalog, &s1).expect("emit a");
        let b = emit(&resolved, &catalog, &s2).expect("emit b");
        let get_proj_hash = |arts: &K8sArtifacts| -> String {
            all_resources(arts).into_iter()
                .find(|(f, k, _)| f == AGENT_RUNTIME_MANIFEST_FILENAME && k == "Deployment")
                .map(|(_, _, v)| v["spec"]["template"]["metadata"]
                    ["annotations"][services::PROJECT_HASH_ANNOTATION]
                    .as_str().unwrap_or("").to_string())
                .unwrap_or_default()
        };
        assert_ne!(get_proj_hash(&a), get_proj_hash(&b),
            "different .ais sources must produce different project-hash annotations");
    }

    #[test] fn project_hash_annotation_shifts_when_lockfile_changes() {
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = resolved_with_k8s_deploy("prod");
        let s1 = sources_with("p", Some("lock-v1"), &[]);
        let s2 = sources_with("p", Some("lock-v2"), &[]);
        let a = emit(&resolved, &catalog, &s1).expect("emit a");
        let b = emit(&resolved, &catalog, &s2).expect("emit b");
        let extract = |arts: &K8sArtifacts| {
            all_resources(arts).into_iter()
                .find(|(f, k, _)| f == AGENT_RUNTIME_MANIFEST_FILENAME && k == "Deployment")
                .map(|(_, _, v)| v["spec"]["template"]["metadata"]
                    ["annotations"][services::PROJECT_HASH_ANNOTATION]
                    .as_str().unwrap_or("").to_string())
                .unwrap_or_default()
        };
        assert_ne!(extract(&a), extract(&b),
            "different lockfile bodies must produce different project-hash annotations");
    }

    #[test] fn agent_pod_mounts_project_configmap_at_etc_aislang() {
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = resolved_with_prompt("prod", "system", "h");
        let sources = sources_with("p", None, &[("system", "body")]);
        let artifacts = emit(&resolved, &catalog, &sources).expect("emit");
        let dep = all_resources(&artifacts).into_iter()
            .find(|(f, k, _)| f == AGENT_RUNTIME_MANIFEST_FILENAME && k == "Deployment")
            .expect("agent Deployment");

        // Container's volumeMounts[0] points at /etc/aislang.
        let mount = &dep.2["spec"]["template"]["spec"]["containers"][0]
            ["volumeMounts"][0];
        assert_eq!(mount["mountPath"].as_str(), Some(services::PROJECT_MOUNT_DIR));
        assert_eq!(mount["readOnly"].as_bool(), Some(true));

        // Pod's volumes[0].configMap references the aislang-project CM.
        let vol = &dep.2["spec"]["template"]["spec"]["volumes"][0];
        assert_eq!(vol["configMap"]["name"].as_str(), Some("aislang-project"));

        // items[] maps:
        //   project.ais  → project.ais
        //   aislang.lock → aislang.lock
        //   <prompt-label> → prompts/<prompt-label>
        let items = vol["configMap"]["items"].as_sequence()
            .expect("items[] must be present (we set per-key paths)");
        let paths: Vec<(String, String)> = items.iter()
            .map(|i| (
                i["key"].as_str().unwrap_or("").to_string(),
                i["path"].as_str().unwrap_or("").to_string(),
            ))
            .collect();
        assert!(paths.iter().any(|(k, p)| k == "project.ais"  && p == "project.ais"),
            "items[] must map project.ais → project.ais; got: {paths:?}");
        assert!(paths.iter().any(|(k, p)| k == "aislang.lock" && p == "aislang.lock"),
            "items[] must map aislang.lock → aislang.lock; got: {paths:?}");
        assert!(paths.iter().any(|(k, p)| k == "system" && p == "prompts/system"),
            "items[] must map system → prompts/system; got: {paths:?}");
    }

    #[test] fn agent_runtime_carries_aislang_labels_with_deploy_propagated() {
        let catalog = aislang_catalog::bundled().expect("bundled");
        let resolved = resolved_with_k8s_deploy("staging");
        let artifacts = emit(&resolved, &catalog, &empty_sources()).expect("emit");
        for (file, kind, v) in all_resources(&artifacts) {
            if file != AGENT_RUNTIME_MANIFEST_FILENAME &&
               file != AISLANG_PROJECT_CM_MANIFEST_FILENAME { continue; }
            let labels = &v["metadata"]["labels"];
            assert_eq!(labels["aislang.component"].as_str(), Some("agent_runtime"),
                "{file}/{kind}: aislang.component must be agent_runtime");
            assert_eq!(labels["aislang.deploy"].as_str(), Some("staging"),
                "{file}/{kind}: aislang.deploy must propagate from deploy block label");
            assert_eq!(labels["aislang.generated"].as_str(), Some("true"));
        }
    }

    #[test] fn agent_service_selector_matches_pod_template_labels() {
        // Round-trip: Service spec.selector targets pods labelled
        // aislang.component=agent_runtime; agent-runtime Deployment's
        // pod template MUST carry that label or the Service ends up
        // with no endpoints + traffic black-holes.
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&empty_resolved(), &catalog, &empty_sources()).expect("emit");
        let resources = all_resources(&artifacts);
        let svc = resources.iter()
            .find(|(f, k, _)| f == AGENT_RUNTIME_MANIFEST_FILENAME && k == "Service")
            .expect("agent Service");
        let dep = resources.iter()
            .find(|(f, k, _)| f == AGENT_RUNTIME_MANIFEST_FILENAME && k == "Deployment")
            .expect("agent Deployment");
        let sel = svc.2["spec"]["selector"]["aislang.component"].as_str();
        let pod_lbl = dep.2["spec"]["template"]["metadata"]["labels"]
            ["aislang.component"].as_str();
        assert_eq!(sel, pod_lbl,
            "Service selector aislang.component must match pod-template label; \
             selector={sel:?} pod={pod_lbl:?}");
    }
}

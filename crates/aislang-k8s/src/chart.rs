//! Helm chart wrapper around the raw K8s manifests — Phase 2 Week 14
//! Thu (Decision 43.7, locked Phase 2 Sunday).
//!
//! ## What this module produces
//!
//! A `BTreeMap<PathBuf, String>` representing a Helm chart directory
//! tree rooted at `<output_dir>/chart/`:
//!
//! ```text
//! chart/
//!   Chart.yaml           apiVersion: v2, name: <deploy-label>, version: 0.1.0
//!   values.yaml          image.tag, openwebui.persistence.size, ...
//!   templates/
//!     litellm.yaml              ← raw Tue manifest, image.tag templated
//!     jaeger.yaml               ← raw Tue manifest verbatim
//!     budget-counter.yaml       ← raw Tue manifest verbatim
//!     agent-runtime.yaml        ← raw Wed manifest, image.tag templated
//!     aislang-project-config.yaml  ← raw Wed manifest verbatim
//!     openwebui.yaml           (only when expose.web_ui = true; raw Thu manifest)
//!     ingress.yaml             (only when expose.ingress is Some)
//! ```
//!
//! ## Decision 43.7 — minimal templating
//!
//! No `{{ if }}` ladders.  The `.ais` source already declares "with
//! Open WebUI or without", "with Ingress or without"; the emitter
//! decides which template files to write vs. omit.  Files that ARE
//! emitted use exactly four templating points (see [`TEMPLATE_*`]
//! constants):
//!
//!   - `image.repository` + `image.tag`           — agent-runtime image
//!   - `openwebui.persistence.size`               — PVC size override
//!   - `litellm.replicas`                         — LiteLLM scale-up
//!   - `ingress.host`                             — Ingress hostname override
//!
//! Each is one `{{ .Values.x.y }}` substitution against the raw
//! manifest string.  Keeps the chart legible to ops humans who've
//! never seen aiSlang before.
//!
//! ## How the chart is consumed
//!
//! `aislang apply` (Thu CLI wiring) shells out to:
//!
//!   helm upgrade --install <release> <output_dir>/chart/
//!
//! where `<release>` is the deploy block's label (Decision 43.1).
//! `aislang destroy` shells out to `helm uninstall <release>`.

use crate::services::{
    AGENT_RUNTIME_IMAGE,
    OPENWEBUI_DEFAULT_PVC_SIZE,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

// ============================================================================
// Chart file paths (relative to `<output_dir>/chart/`)
// ============================================================================

pub const CHART_FILENAME:      &str = "Chart.yaml";
pub const VALUES_FILENAME:     &str = "values.yaml";
pub const TEMPLATES_DIRNAME:   &str = "templates";

// ============================================================================
// Template substitution placeholders
// ============================================================================

/// Raw `ghcr.io/aislang/aislang:latest` literal in the agent-runtime
/// manifest, replaced with `{{ .Values.image.repository }}:{{ .Values
/// .image.tag }}` when the manifest lands under `chart/templates/`.
/// Lets `helm install --set image.tag=v0.2.0-rc2` swap the image
/// without re-running `aislang plan`.
pub const IMAGE_TEMPLATE_REPLACEMENT: &str =
    "{{ .Values.image.repository }}:{{ .Values.image.tag }}";

/// Raw `1Gi` literal in the openwebui PVC manifest's
/// `spec.resources.requests.storage`, replaced with `{{ .Values
/// .openwebui.persistence.size }}` under `chart/templates/`.
/// Quoted-only to avoid YAML's auto-stringification of size suffixes.
pub const PVC_SIZE_TEMPLATE_REPLACEMENT: &str =
    "{{ .Values.openwebui.persistence.size }}";

/// Raw `replicas: 1` literal in the LiteLLM Deployment, replaced
/// with `replicas: {{ .Values.litellm.replicas }}` under chart/
/// templates/.  Lets users scale LiteLLM horizontally without
/// re-emitting.  Only LiteLLM gets this treatment — Jaeger / budget-
/// counter / agent-runtime stay at replicas: 1 (stateful + single-
/// instance posture).
pub const LITELLM_REPLICAS_TEMPLATE_REPLACEMENT: &str =
    "replicas: {{ .Values.litellm.replicas }}";

// ============================================================================
// Chart.yaml + values.yaml builders
// ============================================================================

/// Build the chart `Chart.yaml` body.
///
/// `apiVersion: v2` — current Helm 3 chart format.  `type:
/// application` distinguishes a deployable chart from a library
/// chart (we don't ship reusable templates here).
///
/// `name` is the deploy block's label (Decision 43.1: one chart per
/// deploy block).  `version` is the chart-package version (the
/// chart's own SemVer — distinct from the app's version below), and
/// is pinned at "0.1.0" for v0.2 since the chart shape is still
/// evolving week-over-week.  Bumped to "1.0.0" at v0.2.0 final.
///
/// `appVersion` is the upstream image's release — uses the same
/// `aislang.lock`'s `catalog_version` for traceability, since the
/// agent-runtime image is the aislang binary which doesn't have an
/// independent release stream from the catalog yet.
pub fn build_chart_yaml(deploy_label: &str, catalog_version: &str) -> String {
    format!(
        "apiVersion: v2\n\
         name: {deploy_label}\n\
         description: aiSlang stack — generated by `aislang plan`\n\
         type: application\n\
         version: 0.1.0\n\
         appVersion: \"{catalog_version}\"\n"
    )
}

/// Build the chart `values.yaml` body.
///
/// Four templating points (Decision 43.7):
///
///   1. `image.repository` + `image.tag`     — agent-runtime image
///   2. `openwebui.persistence.size`         — PVC size
///   3. `litellm.replicas`                   — LiteLLM scale-up
///   4. `ingress.host`                       — Ingress hostname
///
/// Fields the chart needs at render time but that users RARELY
/// override stay as constants in the templates rather than
/// surfacing here — keeps `helm install --set` surface minimal.
///
/// `ingress.host` defaults to empty string; the Ingress template
/// is only emitted when the .ais source set `expose.ingress = "…"`,
/// so an empty default is fine (no rendering happens for an
/// unrequested Ingress).
pub fn build_values_yaml(ingress_host: Option<&str>) -> String {
    // Helm parses YAML; explicit quoting around string values
    // avoids any auto-typing surprises (`latest` and version
    // strings should never be parsed as booleans/numbers/dates).
    let host_field = match ingress_host {
        Some(h) => format!("  host: \"{h}\"\n"),
        None    => "  host: \"\"  # set when expose.ingress is declared\n".to_string(),
    };
    // Extract `ghcr.io/aislang/aislang` from `<that>:latest`.
    let (image_repo, image_tag) = AGENT_RUNTIME_IMAGE
        .rsplit_once(':')
        .unwrap_or((AGENT_RUNTIME_IMAGE, "latest"));
    format!(
        "# values.yaml — Phase 2 Week 14 Thu (Decision 43.7)\n\
         # Override at install time with: helm install --set <key>=<value>\n\
         \n\
         image:\n  \
           # agent-runtime container image — same `aislang` binary published\n  \
           # by the v0.2 release workflow's `image` job.\n  \
           repository: {image_repo}\n  \
           tag: \"{image_tag}\"\n\
         \n\
         litellm:\n  \
           # Horizontally scale LiteLLM if you're hosting many concurrent\n  \
           # chat sessions.  v0.2 assumes 1 replica (LiteLLM is stateless\n  \
           # given an external Redis but we don't ship one).\n  \
           replicas: 1\n\
         \n\
         openwebui:\n  \
           persistence:\n    \
             # PVC backing /app/backend/data (SQLite + chat history +\n    \
             # uploads).  Bump to 5Gi+ for real workloads.\n    \
             size: \"{OPENWEBUI_DEFAULT_PVC_SIZE}\"\n\
         \n\
         ingress:\n\
         {host_field}"
    )
}

// ============================================================================
// Template-isation: substitute the Tue+Wed+Thu manifests for chart use
// ============================================================================

/// Convert a raw manifest body (as produced by Tue+Wed+Thu) into a
/// Helm-templated body suitable for `chart/templates/<file>.yaml`.
///
/// Substitutes the four templating points (Decision 43.7) — pure
/// string-replace, no parsing.  Manifests that don't contain the
/// pattern get returned unchanged (cheap no-op).
///
/// **Why string-replace and not parse-and-walk**: K8s manifests are
/// already serde-yaml-serialised at emission time; round-tripping
/// through a re-parse + re-serialise would lose Helm's templating
/// (Helm expects raw `{{ … }}` patterns in YAML scalars and serde-
/// yaml would quote-escape them).  String-replace preserves the
/// pattern verbatim.
pub fn helm_templatize(manifest_body: &str) -> String {
    manifest_body
        .replace(AGENT_RUNTIME_IMAGE,                    IMAGE_TEMPLATE_REPLACEMENT)
        .replace(&format!("storage: {OPENWEBUI_DEFAULT_PVC_SIZE}"),
                 &format!("storage: {PVC_SIZE_TEMPLATE_REPLACEMENT}"))
        // LiteLLM replicas template: matches the exact `replicas: 1`
        // line on the LiteLLM Deployment.  Jaeger / budget-counter /
        // agent-runtime / openwebui ALSO have `replicas: 1` lines
        // but those stay as constants (single-instance posture).
        // To avoid clobbering them, we only substitute when the
        // manifest body contains "name: litellm" — heuristic that
        // matches the litellm.yaml file specifically.  Manifests for
        // other services get passed through unchanged.
        .pipe(|s: String| {
            if s.contains("name: litellm\n") {
                s.replacen("replicas: 1", LITELLM_REPLICAS_TEMPLATE_REPLACEMENT, 1)
            } else {
                s
            }
        })
}

/// Thin `.pipe()` helper because Rust doesn't ship one in `std`.
/// Used above to chain a conditional transform without breaking
/// the `replace().replace()...` style.
trait Pipe: Sized {
    fn pipe<F, R>(self, f: F) -> R where F: FnOnce(Self) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}

// ============================================================================
// Top-level chart-files builder
// ============================================================================

/// Build the full `chart/` directory tree as a path-keyed map.
///
/// Inputs:
///   - `deploy_label`     — chart name (Decision 43.1)
///   - `catalog_version`  — appVersion field (traceability)
///   - `manifests`        — raw manifests from emit() — keys are
///                          filenames (litellm.yaml, jaeger.yaml,
///                          budget-counter.yaml, agent-runtime.yaml,
///                          aislang-project-config.yaml, +
///                          openwebui.yaml when web_ui = true, +
///                          ingress.yaml when expose.ingress is set)
///   - `ingress_host`     — Some host when expose.ingress declared,
///                          drives values.yaml::ingress.host default
///
/// Output: `BTreeMap<PathBuf, String>` whose keys are paths
/// RELATIVE to `chart/`: `Chart.yaml`, `values.yaml`, `templates/<name>.yaml`.
/// Determinism: BTreeMap iteration is sorted; re-running `aislang
/// plan` on unchanged inputs produces byte-identical chart_files.
pub fn build_chart(
    deploy_label:    &str,
    catalog_version: &str,
    manifests:       &BTreeMap<String, String>,
    ingress_host:    Option<&str>,
) -> BTreeMap<PathBuf, String> {
    let mut out: BTreeMap<PathBuf, String> = BTreeMap::new();

    out.insert(PathBuf::from(CHART_FILENAME),
               build_chart_yaml(deploy_label, catalog_version));
    out.insert(PathBuf::from(VALUES_FILENAME),
               build_values_yaml(ingress_host));

    // templates/ — one per emitted manifest, with the four templating
    // substitutions applied via helm_templatize.
    for (name, body) in manifests {
        let template_path = PathBuf::from(TEMPLATES_DIRNAME).join(name);
        out.insert(template_path, helm_templatize(body));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn chart_yaml_has_expected_fields() {
        let chart = build_chart_yaml("prod", "2026-05-25");
        assert!(chart.contains("apiVersion: v2"),
            "Chart.yaml must declare apiVersion v2 (Helm 3); got:\n{chart}");
        assert!(chart.contains("name: prod"),
            "Chart name = deploy label per Decision 43.1; got:\n{chart}");
        assert!(chart.contains("type: application"),
            "Chart type must be application (not library); got:\n{chart}");
        assert!(chart.contains("appVersion: \"2026-05-25\""),
            "appVersion tracks catalog_version; got:\n{chart}");
    }

    #[test] fn values_yaml_has_four_templating_points() {
        let v = build_values_yaml(Some("ui.example.com"));
        // Decision 43.7 — exactly four user-overridable points.
        assert!(v.contains("repository:"),    "image.repository point");
        assert!(v.contains("tag:"),            "image.tag point");
        assert!(v.contains("replicas:"),       "litellm.replicas point");
        assert!(v.contains("size:"),           "openwebui.persistence.size point");
        assert!(v.contains("host: \"ui.example.com\""), "ingress.host point");
    }

    #[test] fn values_yaml_handles_no_ingress() {
        let v = build_values_yaml(None);
        assert!(v.contains("host: \"\""),
            "no ingress → empty host default (template not rendered anyway)");
    }

    #[test] fn helm_templatize_substitutes_agent_runtime_image() {
        let body = format!("image: {AGENT_RUNTIME_IMAGE}\n");
        let templated = helm_templatize(&body);
        assert!(templated.contains(IMAGE_TEMPLATE_REPLACEMENT),
            "agent-runtime image must be replaced with chart template; got:\n{templated}");
        assert!(!templated.contains(AGENT_RUNTIME_IMAGE),
            "raw image literal must be gone; got:\n{templated}");
    }

    #[test] fn helm_templatize_substitutes_pvc_size() {
        let body = format!("storage: {OPENWEBUI_DEFAULT_PVC_SIZE}\n");
        let templated = helm_templatize(&body);
        assert!(templated.contains(PVC_SIZE_TEMPLATE_REPLACEMENT),
            "PVC size must be templated; got:\n{templated}");
    }

    #[test] fn helm_templatize_replaces_litellm_replicas_only() {
        // The LiteLLM manifest has `name: litellm` somewhere + a
        // `replicas: 1` line — that one gets templated.
        let litellm_body = "name: litellm\nreplicas: 1\n";
        let templated = helm_templatize(litellm_body);
        assert!(templated.contains(LITELLM_REPLICAS_TEMPLATE_REPLACEMENT),
            "LiteLLM replicas must be templated; got:\n{templated}");

        // A manifest WITHOUT `name: litellm` (e.g. jaeger or budget-
        // counter) keeps `replicas: 1` as a literal.  Decision: only
        // LiteLLM is horizontally scalable in v0.2.
        let other_body = "name: jaeger\nreplicas: 1\n";
        let templated = helm_templatize(other_body);
        assert!(templated.contains("replicas: 1"),
            "non-LiteLLM replicas must NOT be templated; got:\n{templated}");
        assert!(!templated.contains(LITELLM_REPLICAS_TEMPLATE_REPLACEMENT),
            "non-LiteLLM must not pick up the LiteLLM template; got:\n{templated}");
    }

    #[test] fn build_chart_produces_expected_paths() {
        let mut manifests = BTreeMap::new();
        manifests.insert("litellm.yaml".to_string(),
            "name: litellm\nreplicas: 1\n".to_string());
        manifests.insert("jaeger.yaml".to_string(),
            "name: jaeger\nreplicas: 1\n".to_string());

        let chart = build_chart("prod", "2026-05-25", &manifests, None);

        for expected in [
            PathBuf::from("Chart.yaml"),
            PathBuf::from("values.yaml"),
            PathBuf::from("templates").join("litellm.yaml"),
            PathBuf::from("templates").join("jaeger.yaml"),
        ] {
            assert!(chart.contains_key(&expected),
                "chart must include path `{}`; got: {:?}",
                expected.display(), chart.keys().collect::<Vec<_>>());
        }
        // Templated path content has the Helm substitution applied.
        let litellm = &chart[&PathBuf::from("templates").join("litellm.yaml")];
        assert!(litellm.contains(LITELLM_REPLICAS_TEMPLATE_REPLACEMENT),
            "templates/litellm.yaml must be Helm-templated; got:\n{litellm}");
    }

    #[test] fn build_chart_deterministic() {
        // Decision 43 determinism contract: re-emit produces byte-
        // identical chart_files.
        let mut manifests = BTreeMap::new();
        manifests.insert("litellm.yaml".to_string(),
            format!("image: {AGENT_RUNTIME_IMAGE}\n"));
        let a = build_chart("prod", "v1", &manifests, Some("h.example.com"));
        let b = build_chart("prod", "v1", &manifests, Some("h.example.com"));
        assert_eq!(a.len(), b.len());
        for (k, va) in &a {
            assert_eq!(va, &b[k],
                "chart file `{}` must be byte-identical across runs",
                k.display());
        }
    }
}

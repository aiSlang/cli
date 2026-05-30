//! Label conventions shared by every emitted K8s resource.
//!
//! K8s analogue of `aislang-compose`'s `aislang_labels` helper.
//! The two emitters use the same label keys so downstream tooling
//! (`aislang status`, `kubectl get -l aislang.generated=true`,
//! `docker compose ls --filter "label=aislang.generated"`) treats
//! both deploy targets uniformly.
//!
//! ## Keys
//!
//!   - **`aislang.generated=true`** — sentinel marking the resource as
//!     emitter-owned.  Distinguishes the LiteLLM Deployment we created
//!     from a user's hand-rolled `litellm-app` (which they might leave
//!     in place after a `helm uninstall` for inspection).
//!   - **`aislang.component=<name>`** — the resource's logical role
//!     within the stack: `litellm`, `jaeger`, `budget_counter`,
//!     `agent_runtime` (Wed), `openwebui` (Thu).  Used by the Service
//!     selector to match its backing pods.
//!   - **`aislang.deploy=<label>`** — the `deploy "<label>" { … }`
//!     block this resource belongs to.  Decision 43.1: one Helm chart
//!     per deploy block, so within a chart every resource carries the
//!     same deploy label.  Lets multi-deploy-per-project (v0.3+) keep
//!     resources separable post-apply.
//!
//! ## What we deliberately don't set
//!
//! Standard K8s recommended labels (`app.kubernetes.io/name`,
//! `app.kubernetes.io/instance`, `app.kubernetes.io/managed-by`)
//! aren't emitted by the bare manifests — they belong to Helm's
//! own labelling pass (Thu wires the Helm chart templates that
//! merge them in via `{{ include "<chart>.labels" . }}`).  Adding
//! them to the raw manifests would double-up under the chart path
//! and confuse `helm uninstall`'s release-tracking.

use std::collections::BTreeMap;

/// Build the standard label set for a K8s resource: sentinel +
/// component + deploy.  Use everywhere we emit `metadata.labels`.
///
/// **Determinism**: BTreeMap is sorted by key — `aislang.component <
/// aislang.deploy < aislang.generated` lexicographically — so the
/// emitted YAML always orders the three lines the same way.
pub fn aislang_labels(component: &str, deploy_label: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("aislang.component".to_string(), component.to_string());
    m.insert("aislang.deploy".to_string(),    deploy_label.to_string());
    m.insert("aislang.generated".to_string(), "true".to_string());
    m
}

/// Subset of the standard labels used by Service `spec.selector` +
/// Deployment `spec.selector.matchLabels`: just the component name.
///
/// **Why a subset, not the full label set**: K8s selectors match
/// pods whose labels are a SUPERSET of the selector.  Including
/// `aislang.generated=true` works but adds no precision — every
/// component label is unique within a stack already, so
/// `aislang.component=<name>` alone is the tightest selector.
/// Smaller selectors also produce shorter YAML diffs when a label
/// gets added/removed (the selector stays put even when labels
/// shift).
pub fn component_selector(component: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("aislang.component".to_string(), component.to_string());
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn aislang_labels_set_all_three_keys() {
        let l = aislang_labels("litellm", "prod");
        assert_eq!(l.get("aislang.component").map(String::as_str), Some("litellm"));
        assert_eq!(l.get("aislang.deploy").map(String::as_str),    Some("prod"));
        assert_eq!(l.get("aislang.generated").map(String::as_str), Some("true"));
        assert_eq!(l.len(), 3, "no other keys expected; got: {l:?}");
    }

    #[test] fn component_selector_carries_only_component_key() {
        let s = component_selector("jaeger");
        assert_eq!(s.get("aislang.component").map(String::as_str), Some("jaeger"));
        assert_eq!(s.len(), 1, "selector must be the minimal component-only \
                                form (see module-level rationale); got: {s:?}");
    }

    #[test] fn labels_ordering_is_deterministic() {
        // BTreeMap iter is sorted; pin that the three keys land in
        // the lexicographic order the YAML output relies on.
        let l = aislang_labels("x", "y");
        let keys: Vec<&str> = l.keys().map(String::as_str).collect();
        assert_eq!(keys,
            vec!["aislang.component", "aislang.deploy", "aislang.generated"]);
    }
}

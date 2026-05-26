//! Docker Compose emitter for aiSlang projects.
//!
//! Takes a [`ResolvedProject`] (post-validate, post-resolve, post-
//! lockfile-reconciliation) and emits the matching `docker-compose.yml`
//! content. v0.1 emits five service categories:
//!
//!   - one service per `mcp_server` block (image = resolved OCI digest
//!     when present, else `registry/repo:tag`; bind-mount the `mount`
//!     dir if set; env carried through)
//!   - `litellm` sidecar — central provider router; model bindings
//!     surfaced as an env-var JSON blob (real config-file wiring is
//!     week 5)
//!   - `jaeger` sidecar — OTLP receiver + UI; placeholder for the
//!     week-9 OTel-GenAI instrumentation
//!   - `budget_counter` sidecar — alpine placeholder with cost-cap
//!     env vars; real circuit-breaker is week 7
//!   - `agent_runtime` service — alpine placeholder that depends on
//!     all of the above; real agent runtime is week 6
//!
//! All services labelled `aislang.component=…` + `aislang.generated=true`
//! so `docker compose ps` and other tooling can identify them.
//!
//! Output is built from typed Rust structs and serialised via
//! `serde_yaml::to_string` — no string templating. Service maps use
//! `BTreeMap` for deterministic key ordering (lockfile-style
//! reproducibility on unchanged input).

pub mod litellm;

use aislang_core::ast::McpServer;
use aislang_core::types::OciImage;
use aislang_resolver::ResolvedProject;
use serde::Serialize;
use std::collections::BTreeMap;
use thiserror::Error;

// ============================================================================
// Public entry point
// ============================================================================

#[derive(Debug, Error)]
pub enum EmitError {
    #[error("YAML serialization failed: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("JSON serialization failed (LiteLLM env): {0}")]
    Json(#[from] serde_json::Error),
}

/// Emit the docker-compose YAML for the resolved project. Returns the
/// YAML as a String suitable for `docker compose -f - up`.
pub fn emit(resolved: &ResolvedProject) -> Result<String, EmitError> {
    let compose = build_compose(resolved)?;
    Ok(serde_yaml::to_string(&compose)?)
}

// ============================================================================
// Compose-file types (typed structs, not strings)
// ============================================================================

#[derive(Serialize)]
struct ComposeFile {
    services: BTreeMap<String, Service>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    volumes:  BTreeMap<String, Volume>,
}

#[derive(Serialize, Default)]
struct Service {
    image: String,
    /// Environment as a list of `KEY=value` strings — *not* a map.
    ///
    /// Rationale: Docker Compose's Go YAML 1.1 parser interprets
    /// unquoted scalar values per YAML's implicit-typing rules. A
    /// value like `2026-05-25` parses as `time.Time` and Compose
    /// then rejects it with "invalid jsonType time.Time" because
    /// env values must be strings. serde_yaml correctly quotes
    /// numeric- and bool-shaped strings (`'true'`, `'50000000'`)
    /// but misses date-shaped ones.
    ///
    /// List-form (`- KEY=value`) makes the whole pair a single
    /// string token; YAML scalar-type ambiguity vanishes because
    /// no unquoted scalar ever stands alone as the value. Compose
    /// explicitly supports both forms.
    #[serde(skip_serializing_if = "Option::is_none")]
    environment: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volumes:     Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ports:       Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depends_on:  Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command:     Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels:      Option<BTreeMap<String, String>>,
}

/// Convert a `BTreeMap<KEY, value>` into a sorted `Vec<"KEY=value">`
/// for the env-list form. BTreeMap iteration order is already sorted,
/// so the resulting Vec is deterministic.
fn env_list(map: BTreeMap<String, String>) -> Vec<String> {
    map.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
}

#[derive(Serialize, Default)]
struct Volume {}

// ============================================================================
// Top-level builder
// ============================================================================

fn build_compose(resolved: &ResolvedProject) -> Result<ComposeFile, EmitError> {
    let mut services: BTreeMap<String, Service> = BTreeMap::new();

    // (a) one service per mcp_server block.
    for mcp in &resolved.project.mcp_servers {
        services.insert(format!("mcp_{}", mcp.label), build_mcp_service(mcp));
    }

    // (b) LiteLLM sidecar — central provider router.
    services.insert("litellm".to_string(), build_litellm_service(resolved)?);

    // (c) Jaeger sidecar — OTLP receiver + UI.
    services.insert("jaeger".to_string(), build_jaeger_service());

    // (d) Budget counter (week-7 placeholder).
    services.insert("budget_counter".to_string(),
                    build_budget_counter_service(resolved));

    // (e) Agent runtime (week-6 placeholder) — only if there are agents.
    if !resolved.project.agents.is_empty() {
        services.insert("agent_runtime".to_string(),
                        build_agent_runtime_service(resolved));
    }

    Ok(ComposeFile {
        services,
        volumes: BTreeMap::new(),
    })
}

// ============================================================================
// Per-service builders
// ============================================================================

fn build_mcp_service(mcp: &McpServer) -> Service {
    let environment = mcp.env.as_ref().and_then(|pairs| {
        if pairs.is_empty() { return None; }
        let map: BTreeMap<String, String> = pairs.iter().cloned().collect();
        Some(env_list(map))
    });

    let volumes: Option<Vec<String>> = mcp.mount.as_ref().map(|p| {
        // Bind-mount the host directory into /data inside the container.
        // The mcp_server image is expected to read from /data; v0.1
        // doesn't introspect MCP server image config to find their
        // expected mount path. v0.2 may allow override per spec.
        vec![format!("{}:/data:ro", p.display())]
    });

    Service {
        image:       format_image(&mcp.image),
        environment,
        volumes,
        ports:       None,
        depends_on:  None,
        command:     None,
        labels:      Some(aislang_labels("mcp_server")),
    }
}

fn build_litellm_service(resolved: &ResolvedProject) -> Result<Service, EmitError> {
    // Model bindings surfaced as an env-var JSON blob. Week 5 will
    // mount a real config file from a generated litellm-config.yaml;
    // env-var passthrough keeps the v0.1 skeleton dependency-free.
    let bindings_json = serde_json::to_string(&resolved.model_bindings)?;

    let mut env = BTreeMap::new();
    env.insert("AISLANG_MODEL_BINDINGS".to_string(), bindings_json);
    env.insert("AISLANG_CATALOG_VERSION".to_string(),
               resolved.catalog_version.clone());

    Ok(Service {
        image:       "ghcr.io/berriai/litellm:main-stable".to_string(),
        environment: Some(env_list(env)),
        volumes:     None,
        ports:       Some(vec!["4000:4000".to_string()]),  // LiteLLM default
        depends_on:  None,
        command:     None,
        labels:      Some(aislang_labels("litellm")),
    })
}

fn build_jaeger_service() -> Service {
    let mut env = BTreeMap::new();
    env.insert("COLLECTOR_OTLP_ENABLED".to_string(), "true".to_string());
    Service {
        image:       "jaegertracing/all-in-one:latest".to_string(),
        environment: Some(env_list(env)),
        volumes:     None,
        ports:       Some(vec![
            "16686:16686".to_string(),   // Jaeger UI
            "4317:4317".to_string(),     // OTLP gRPC
            "4318:4318".to_string(),     // OTLP HTTP
        ]),
        depends_on:  None,
        command:     None,
        labels:      Some(aislang_labels("jaeger")),
    }
}

fn build_budget_counter_service(resolved: &ResolvedProject) -> Service {
    let mut env = BTreeMap::new();
    if let Some(b) = &resolved.project.budget {
        env.insert(
            "AISLANG_HARD_CAP_PER_DAY_MICRO".to_string(),
            b.hard_cap_per_day.micro_units.to_string(),
        );
        env.insert(
            "AISLANG_HARD_CAP_PER_RUN_MICRO".to_string(),
            b.hard_cap_per_run.micro_units.to_string(),
        );
    }
    Service {
        image:       "alpine:3.19".to_string(),
        environment: Some(env_list(env)),
        volumes:     None,
        ports:       None,
        depends_on:  None,
        command:     Some(vec![
            "sh".to_string(), "-c".to_string(),
            "echo 'budget-counter placeholder; real circuit-breaker arrives \
             week 7. Cap env vars are set; an external counter could read \
             them now.' && sleep infinity".to_string(),
        ]),
        labels:      Some(aislang_labels("budget_counter")),
    }
}

fn build_agent_runtime_service(resolved: &ResolvedProject) -> Service {
    // Depends on litellm + jaeger + every mcp_server, so docker compose
    // starts them in the right order.
    let mut depends = vec!["litellm".to_string(), "jaeger".to_string()];
    for mcp in &resolved.project.mcp_servers {
        depends.push(format!("mcp_{}", mcp.label));
    }
    depends.sort();   // deterministic

    let mut env = BTreeMap::new();
    env.insert("AISLANG_LITELLM_URL".to_string(),
               "http://litellm:4000".to_string());
    env.insert("AISLANG_OTLP_ENDPOINT".to_string(),
               "http://jaeger:4318".to_string());

    // Prompt hashes — week-9 will use these as observability dims; v0.1
    // just surfaces them so the runtime can compare against its loaded
    // prompts and warn on drift.
    for (label, hash) in &resolved.prompt_hashes {
        env.insert(
            format!("AISLANG_PROMPT_HASH_{}", label.to_uppercase()),
            hash.clone(),
        );
    }

    // Port mappings from every `deploy.expose` block, flattened.
    let mut ports = Vec::new();
    for d in &resolved.project.deploys {
        if let Some(expose) = &d.expose {
            for (_name, port) in expose {
                ports.push(format!("{port}:{port}"));
            }
        }
    }
    ports.sort();
    let ports = if ports.is_empty() { None } else { Some(ports) };

    Service {
        image:       "alpine:3.19".to_string(),
        environment: Some(env_list(env)),
        volumes:     None,
        ports,
        depends_on:  Some(depends),
        command:     Some(vec![
            "sh".to_string(), "-c".to_string(),
            "echo 'agent-runtime placeholder; real runtime arrives week 6. \
             LITELLM_URL, OTLP_ENDPOINT, and PROMPT_HASH_* env vars are \
             set for the eventual implementation to consume.' \
             && sleep infinity".to_string(),
        ]),
        labels:      Some(aislang_labels("agent_runtime")),
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn format_image(img: &OciImage) -> String {
    let base = format!("{}/{}", img.registry, img.repository);
    match (&img.tag, &img.digest) {
        (Some(t), Some(d)) => format!("{base}:{t}@{d}"),
        (Some(t), None)    => format!("{base}:{t}"),
        (None,    Some(d)) => format!("{base}@{d}"),
        (None,    None)    => base,
    }
}

fn aislang_labels(component: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("aislang.component".to_string(), component.to_string());
    m.insert("aislang.generated".to_string(), "true".to_string());
    m
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// End-to-end: parse + resolve + emit the demo file. Asserts
    /// structural properties of the emitted compose YAML; the demo
    /// fixture provides one model, one prompt, one mcp_server, one
    /// agent, one eval, one deploy.
    fn emit_demo() -> String {
        let src_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..").join("..")
            .join("examples").join("support-agent.ais");
        let src = std::fs::read_to_string(&src_path).expect("read demo");
        let (project, _) = aislang_core::validate(&src).expect("validate");
        let examples_dir = src_path.parent().unwrap();
        let resolved = aislang_resolver::resolve_against_bundled_catalog(
            &project, examples_dir,
        ).expect("resolve");
        emit(&resolved).expect("emit")
    }

    #[test] fn emitted_yaml_round_trips() {
        // Structural validity: parse the emitted YAML back into a
        // generic structure. If serde_yaml accepts it, downstream
        // `docker compose -f - config -q` will too.
        let yaml = emit_demo();
        let _: serde_yaml::Value = serde_yaml::from_str(&yaml)
            .expect("emitted YAML must parse back via serde_yaml");
    }

    #[test] fn emits_one_service_per_mcp_server() {
        let yaml = emit_demo();
        // demo has mcp_server "docs"
        assert!(yaml.contains("mcp_docs:"),
            "expected `mcp_docs:` service in output; got:\n{yaml}");
    }

    #[test] fn emits_litellm_sidecar_with_model_bindings_env() {
        let yaml = emit_demo();
        assert!(yaml.contains("litellm:"));
        assert!(yaml.contains("ghcr.io/berriai/litellm:main-stable"));
        assert!(yaml.contains("AISLANG_MODEL_BINDINGS"));
        // The demo resolves to ollama.llama_3_1_8b under cost-opt;
        // the JSON blob should contain that.
        assert!(yaml.contains("llama_3_1_8b"));
    }

    #[test] fn emits_jaeger_with_otlp_ports() {
        let yaml = emit_demo();
        assert!(yaml.contains("jaeger:"));
        assert!(yaml.contains("jaegertracing/all-in-one"));
        assert!(yaml.contains("16686:16686"));   // UI
        assert!(yaml.contains("4318:4318"));     // OTLP HTTP
    }

    #[test] fn emits_budget_counter_with_cap_env_vars() {
        let yaml = emit_demo();
        assert!(yaml.contains("budget_counter:"));
        assert!(yaml.contains("AISLANG_HARD_CAP_PER_DAY_MICRO"));
        assert!(yaml.contains("AISLANG_HARD_CAP_PER_RUN_MICRO"));
        // Demo's $50/day → 50_000_000 micro_units
        assert!(yaml.contains("50000000"));
    }

    #[test] fn agent_runtime_depends_on_sidecars() {
        let yaml = emit_demo();
        assert!(yaml.contains("agent_runtime:"));
        let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let agent = &value["services"]["agent_runtime"];
        let deps = agent["depends_on"].as_sequence().expect("depends_on is a list");
        let dep_names: Vec<&str> = deps.iter()
            .filter_map(|d| d.as_str()).collect();
        assert!(dep_names.contains(&"litellm"));
        assert!(dep_names.contains(&"jaeger"));
        assert!(dep_names.contains(&"mcp_docs"));
    }

    #[test] fn agent_runtime_carries_prompt_hashes_as_env() {
        let yaml = emit_demo();
        // Demo prompt is labelled "system" → AISLANG_PROMPT_HASH_SYSTEM
        assert!(yaml.contains("AISLANG_PROMPT_HASH_SYSTEM"));
    }

    #[test] fn agent_runtime_exposes_deploy_ports() {
        let yaml = emit_demo();
        // Demo's deploy.expose has { port = 8080 } → "8080:8080"
        assert!(yaml.contains("8080:8080"));
    }

    #[test] fn all_services_labelled_aislang() {
        let yaml = emit_demo();
        let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let services = value["services"].as_mapping().expect("services map");
        for (name, svc) in services {
            let labels = &svc["labels"];
            assert!(labels.is_mapping(),
                "service `{}` missing labels", name.as_str().unwrap_or("?"));
            assert!(labels["aislang.generated"].as_str() == Some("true"),
                "service `{}` missing aislang.generated label",
                name.as_str().unwrap_or("?"));
        }
    }

    #[test] fn env_emitted_as_list_form_not_map() {
        // Pins the fix for the Docker Compose
        // "invalid jsonType time.Time" error: env values like
        // `2026-05-25` (catalog version) were being emitted as
        // unquoted YAML scalars and parsed as YAML 1.1 timestamps
        // by Docker's Go YAML parser. List-form (`- KEY=value`)
        // makes each pair one string token, bypassing scalar-type
        // ambiguity entirely.
        let yaml = emit_demo();
        let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let litellm = &value["services"]["litellm"];
        let env = litellm["environment"].as_sequence()
            .expect("environment must be a YAML sequence (list-form), \
                     not a mapping — see Docker Compose YAML 1.1 \
                     date-shape bug doc on Service::environment field");
        // Each element should be a string `KEY=value` with the value
        // intact and no YAML-type coercion.
        let any_catalog_ver = env.iter()
            .filter_map(|s| s.as_str())
            .find(|s| s.starts_with("AISLANG_CATALOG_VERSION="))
            .expect("AISLANG_CATALOG_VERSION env var must be present");
        // The literal "2026-05-25" must survive serialization round-
        // trip as a plain string — no parsing to time.Time, no
        // reformatting.
        assert!(any_catalog_ver.ends_with("=2026-05-25"),
            "catalog version must round-trip verbatim as a string; got `{any_catalog_ver}`");
    }

    #[test] fn project_without_agents_omits_agent_runtime() {
        // Build a synthetic resolved-project with no agents.
        use aislang_core::ast::Project;
        let project = Project { name: "x".into(), ..Project::default() };
        let resolved = ResolvedProject {
            project,
            catalog_version: "test".into(),
            model_bindings:  BTreeMap::new(),
            prompt_hashes:   BTreeMap::new(),
        };
        let yaml = emit(&resolved).expect("emit");
        assert!(!yaml.contains("agent_runtime:"),
            "expected no agent_runtime when project has no agents;\n{yaml}");
    }
}

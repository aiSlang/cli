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

use aislang_catalog::Catalog;
use aislang_core::ast::McpServer;
use aislang_core::types::OciImage;
use aislang_resolver::ResolvedProject;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use thiserror::Error;

/// Bundle of files emitted per plan run. CLI writes each to disk.
/// `docker_compose` also goes to stdout for the `aislang plan | docker
/// compose -f - up` pattern.
pub struct ComposeArtifacts {
    pub docker_compose: String,
    pub litellm_config: String,
    /// `.env.example` template listing every env var the user must
    /// populate in their own `.env` before `docker compose up`. v0.1
    /// (Wednesday) covers provider API keys / base URLs derived from
    /// the resolved providers.
    pub env_example:    String,
}

/// Filenames used inside `output_dir`. Mirrored in the emitted compose
/// YAML's volume mount so the two stay in sync.
pub const LITELLM_CONFIG_FILENAME: &str = "litellm_config.yaml";
pub const ENV_EXAMPLE_FILENAME:    &str = ".env.example";

// ============================================================================
// Public entry point
// ============================================================================

#[derive(Debug, Error)]
pub enum EmitError {
    #[error("YAML serialization failed: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("JSON serialization failed (LiteLLM env): {0}")]
    Json(#[from] serde_json::Error),

    #[error("LiteLLM config generation failed: {0}")]
    LitellmConfig(#[from] litellm::ConfigError),
}

/// Emit the docker-compose YAML + the LiteLLM config YAML for the
/// resolved project. The compose YAML embeds an absolute volume-mount
/// path pointing at `<output_dir>/litellm_config.yaml` — the caller
/// (typically `aislang plan`) is responsible for writing the config
/// file there.
///
/// `output_dir` should already be an absolute path (callers obtain
/// this by canonicalising the source directory or the user-supplied
/// `--output-dir`). Absolute paths in the compose YAML mean
/// `docker compose -f - up` works regardless of the user's CWD.
pub fn emit(
    resolved:   &ResolvedProject,
    catalog:    &Catalog,
    output_dir: &Path,
) -> Result<ComposeArtifacts, EmitError> {
    let litellm_config = litellm::generate_config(resolved, catalog)?;
    let env_example    = litellm::generate_env_example(resolved);
    let compose        = build_compose(resolved, output_dir)?;
    let docker_compose = serde_yaml::to_string(&compose)?;
    Ok(ComposeArtifacts {
        docker_compose,
        litellm_config,
        env_example,
    })
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

fn build_compose(resolved: &ResolvedProject, output_dir: &Path) -> Result<ComposeFile, EmitError> {
    let mut services: BTreeMap<String, Service> = BTreeMap::new();

    // (a) one service per mcp_server block.
    for mcp in &resolved.project.mcp_servers {
        services.insert(format!("mcp_{}", mcp.label), build_mcp_service(mcp));
    }

    // (b) LiteLLM sidecar — central provider router. Reads its routing
    // table from `output_dir/litellm_config.yaml`, mounted in.
    services.insert("litellm".to_string(),
                    build_litellm_service(resolved, output_dir)?);

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

fn build_litellm_service(resolved: &ResolvedProject, output_dir: &Path)
    -> Result<Service, EmitError>
{
    // Catalog version stays as an env var for traceability — useful
    // when debugging "which catalog version was the running stack
    // resolved against?" — and also keeps env-as-list-form coverage
    // (one of our regression tests pins the YAML-1.1-date workaround
    // and needs SOMETHING with a date-shaped value here).
    let mut env = BTreeMap::new();
    env.insert("AISLANG_CATALOG_VERSION".to_string(),
               resolved.catalog_version.clone());

    // Mount the generated config file at /app/config.yaml. The host
    // path is absolute to keep `docker compose -f -` portable across
    // the user's CWD.
    let host_config = output_dir.join(LITELLM_CONFIG_FILENAME);
    let volume_spec = format!("{}:/app/config.yaml:ro", host_config.display());

    Ok(Service {
        image:       "ghcr.io/berriai/litellm:main-stable".to_string(),
        environment: Some(env_list(env)),
        volumes:     Some(vec![volume_spec]),
        ports:       Some(vec!["4000:4000".to_string()]),  // LiteLLM default
        depends_on:  None,
        command:     Some(vec![
            "--config".to_string(),
            "/app/config.yaml".to_string(),
            "--port".to_string(),
            "4000".to_string(),
        ]),
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
    fn emit_demo() -> ComposeArtifacts {
        let src_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..").join("..")
            .join("examples").join("support-agent.ais");
        let src = std::fs::read_to_string(&src_path).expect("read demo");
        let (project, _) = aislang_core::validate(&src).expect("validate");
        let examples_dir = src_path.parent().unwrap();
        let resolved = aislang_resolver::resolve_against_bundled_catalog(
            &project, examples_dir,
        ).expect("resolve");
        let catalog = aislang_catalog::bundled().expect("bundled");
        emit(&resolved, &catalog, examples_dir).expect("emit")
    }

    fn emit_demo_yaml() -> String {
        emit_demo().docker_compose
    }

    #[test] fn emitted_yaml_round_trips() {
        // Structural validity: parse the emitted YAML back into a
        // generic structure. If serde_yaml accepts it, downstream
        // `docker compose -f - config -q` will too.
        let yaml = emit_demo_yaml();
        let _: serde_yaml::Value = serde_yaml::from_str(&yaml)
            .expect("emitted YAML must parse back via serde_yaml");
    }

    #[test] fn emits_one_service_per_mcp_server() {
        let yaml = emit_demo_yaml();
        // demo has mcp_server "docs"
        assert!(yaml.contains("mcp_docs:"),
            "expected `mcp_docs:` service in output; got:\n{yaml}");
    }

    #[test] fn emits_litellm_with_mounted_config() {
        // Week-5-Tuesday change: LiteLLM no longer carries the
        // model bindings as an env-var JSON blob. They live in a
        // YAML file mounted at /app/config.yaml; the service's
        // command points at it via --config.
        let yaml = emit_demo_yaml();
        assert!(yaml.contains("litellm:"));
        assert!(yaml.contains("ghcr.io/berriai/litellm:main-stable"));

        // Mount of the host config file at the container path.
        assert!(yaml.contains("litellm_config.yaml:/app/config.yaml:ro"),
            "expected litellm to mount the generated config:\n{yaml}");

        // Command points at the mounted file.
        assert!(yaml.contains("--config"));
        assert!(yaml.contains("/app/config.yaml"));

        // Placeholder env var that used to carry the JSON blob is gone.
        assert!(!yaml.contains("AISLANG_MODEL_BINDINGS"),
            "AISLANG_MODEL_BINDINGS env var should be removed in week-5 Tue:\n{yaml}");
    }

    #[test] fn emit_returns_litellm_config_alongside_compose() {
        let artifacts = emit_demo();
        // Both files generated.
        assert!(!artifacts.docker_compose.is_empty());
        assert!(!artifacts.litellm_config.is_empty());
        // litellm_config has the expected shape — one entry for `fast`.
        assert!(artifacts.litellm_config.contains("model_list:"));
        assert!(artifacts.litellm_config.contains("model_name: fast"));
        assert!(artifacts.litellm_config.contains("model: ollama/llama3.1:8b"));
    }

    #[test] fn litellm_volume_uses_absolute_host_path() {
        // The host-side path in the volume mount must be absolute so
        // `docker compose -f - up` works regardless of the user's CWD.
        let yaml = emit_demo_yaml();
        let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let volumes = value["services"]["litellm"]["volumes"]
            .as_sequence().expect("litellm.volumes is a list");
        let mount = volumes[0].as_str().expect("first volume is a string");
        let host_side = mount.split(':').next().expect("split on ':'");
        assert!(host_side.starts_with('/'),
            "host path must be absolute; got `{host_side}` in `{mount}`");
        assert!(host_side.ends_with("/litellm_config.yaml"),
            "host path must point at litellm_config.yaml; got `{host_side}`");
    }

    #[test] fn emits_jaeger_with_otlp_ports() {
        let yaml = emit_demo_yaml();
        assert!(yaml.contains("jaeger:"));
        assert!(yaml.contains("jaegertracing/all-in-one"));
        assert!(yaml.contains("16686:16686"));   // UI
        assert!(yaml.contains("4318:4318"));     // OTLP HTTP
    }

    #[test] fn emits_budget_counter_with_cap_env_vars() {
        let yaml = emit_demo_yaml();
        assert!(yaml.contains("budget_counter:"));
        assert!(yaml.contains("AISLANG_HARD_CAP_PER_DAY_MICRO"));
        assert!(yaml.contains("AISLANG_HARD_CAP_PER_RUN_MICRO"));
        // Demo's $50/day → 50_000_000 micro_units
        assert!(yaml.contains("50000000"));
    }

    #[test] fn agent_runtime_depends_on_sidecars() {
        let yaml = emit_demo_yaml();
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
        let yaml = emit_demo_yaml();
        // Demo prompt is labelled "system" → AISLANG_PROMPT_HASH_SYSTEM
        assert!(yaml.contains("AISLANG_PROMPT_HASH_SYSTEM"));
    }

    #[test] fn agent_runtime_exposes_deploy_ports() {
        let yaml = emit_demo_yaml();
        // Demo's deploy.expose has { port = 8080 } → "8080:8080"
        assert!(yaml.contains("8080:8080"));
    }

    #[test] fn all_services_labelled_aislang() {
        let yaml = emit_demo_yaml();
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
        let yaml = emit_demo_yaml();
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
        let catalog = aislang_catalog::bundled().expect("bundled");
        let artifacts = emit(&resolved, &catalog, std::path::Path::new("/tmp"))
            .expect("emit");
        assert!(!artifacts.docker_compose.contains("agent_runtime:"),
            "expected no agent_runtime when project has no agents;\n{}",
            artifacts.docker_compose);
    }
}

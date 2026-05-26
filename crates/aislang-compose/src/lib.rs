//! Docker Compose emitter for aiSlang projects.
//!
//! Takes a [`ResolvedProject`] (post-validate, post-resolve, post-
//! lockfile-reconciliation) and emits the matching `docker-compose.yml`
//! content. v0.1 emits four service categories:
//!
//!   - one service per `mcp_server` block (image = resolved OCI digest
//!     when present, else `registry/repo:tag`; bind-mount the `mount`
//!     dir if set; env carried through)
//!   - `litellm` sidecar — central provider router; routing table
//!     mounted from a generated `litellm_config.yaml` (week 5)
//!   - `jaeger` sidecar — OTLP receiver + UI; placeholder for the
//!     week-9 OTel-GenAI instrumentation
//!   - `budget_counter` sidecar — alpine placeholder with cost-cap
//!     env vars; real circuit-breaker is week 7
//!
//! **No `agent_runtime` service in v0.1.** Week 6 locks the in-CLI
//! agent loop: `aislang chat <agent>` runs the loop in-process, dialing
//! the Compose-managed LiteLLM and MCP servers. The week-4 placeholder
//! service has been removed; the eventual containerized agent runtime
//! (Open WebUI + per-agent service) is v0.2. See PLAN-90-days.md
//! "Week 6 — Architectural decisions" for the rationale.
//!
//! `deploy.expose` ports are emitted as an informational comment at
//! the top of the YAML (no service binds them in v0.1) so the user
//! sees what their v0.2 agent service will reserve.
//!
//! All services labelled `aislang.component=…` + `aislang.generated=true`
//! so `docker compose ps` and other tooling can identify them.
//!
//! Output is built from typed Rust structs and serialised via
//! `serde_yaml::to_string` — no string templating. Service maps use
//! `BTreeMap` for deterministic key ordering (lockfile-style
//! reproducibility on unchanged input). A YAML comment block is
//! prepended to the serialized output (serde_yaml itself doesn't emit
//! comments).

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
    let body           = serde_yaml::to_string(&compose)?;
    let header         = build_yaml_header(resolved);
    let docker_compose = format!("{header}{body}");
    Ok(ComposeArtifacts {
        docker_compose,
        litellm_config,
        env_example,
    })
}

/// Top-of-file comment block prepended to the emitted compose YAML.
/// Documents the v0.1 agent-in-CLI choice + lists declared
/// `deploy.expose` ports (informational — no service binds them in
/// v0.1; the v0.2 containerized agent runtime will).
fn build_yaml_header(resolved: &ResolvedProject) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "# Generated by `aislang plan` — do not edit by hand.");
    let _ = writeln!(out, "#");
    let _ = writeln!(out, "# v0.1 ships the agent runtime *in the CLI* — `aislang chat <agent>`");
    let _ = writeln!(out, "# runs the chat loop in-process and dials the services below.");
    let _ = writeln!(out, "# v0.2 will add a per-agent container service plus an Open WebUI");
    let _ = writeln!(out, "# presentation layer; the .ais source already describes that surface.");

    // Collect declared expose ports for the comment. Stable order via
    // per-deploy iteration; ports inside a deploy use BTreeMap order.
    let mut declared: Vec<(String, u16)> = Vec::new();
    for d in &resolved.project.deploys {
        if let Some(expose) = &d.expose {
            for (name, port) in expose {
                declared.push((format!("deploy.{}.{name}", d.label), *port));
            }
        }
    }
    if !declared.is_empty() {
        let _ = writeln!(out, "#");
        let _ = writeln!(out, "# Declared deploy.expose ports (informational; not bound in v0.1):");
        for (path, port) in declared {
            let _ = writeln!(out, "#   {path} = {port}");
        }
    }
    let _ = writeln!(out, "#");
    out
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

    // (e) Agent runtime — REMOVED in week 6 Mon. v0.1 runs the agent
    // loop in the CLI (`aislang chat <agent>`); the prompt hashes that
    // used to flow through this service's env block are now consumed
    // by the CLI from the lockfile. v0.2's containerized agent service
    // re-introduces something at this layer.

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

// `build_agent_runtime_service` removed in week 6 Mon — see module-
// level doc comment for the rationale. The v0.2 successor will be a
// per-`agent`-block service that listens on `deploy.expose.port`,
// dials LiteLLM/MCP itself, and pairs with an Open WebUI service for
// presentation. The AST surface needed for that (`agent`, `deploy`,
// `mcp_server`) is already frozen in LANGUAGE-SPEC §4; no .ais source
// change is needed when v0.2 lands.

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

    // ------------------------------------------------------------------
    // Week 6 Mon: agent_runtime service removed from v0.1 output.
    // See module-level doc comment for the rationale (in-CLI agent loop).
    // These tests pin the new invariants so a future regression that
    // re-adds the placeholder fails loudly.
    // ------------------------------------------------------------------

    #[test] fn no_agent_runtime_service_in_v01_output() {
        let yaml = emit_demo_yaml();
        let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let services = value["services"].as_mapping().expect("services map");
        for (name, _) in services {
            let n = name.as_str().unwrap_or("");
            assert_ne!(n, "agent_runtime",
                "v0.1 must not emit an agent_runtime service; \
                 agent loop lives in `aislang chat`");
        }
        // Also assert no string-level mention (the YAML body should not
        // even contain the substring, comments aside — the comments
        // only appear in the header block, prefixed with `#`).
        let body_only: String = yaml.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!body_only.contains("agent_runtime"),
            "agent_runtime must not appear in the YAML body:\n{body_only}");
    }

    #[test] fn yaml_header_explains_v01_agent_in_cli() {
        // The prepended comment block tells the user where the agent
        // lives. Useful when reading the YAML standalone.
        let yaml = emit_demo_yaml();
        assert!(yaml.starts_with("# Generated by `aislang plan`"),
            "header must lead the file:\n{yaml}");
        assert!(yaml.contains("v0.1 ships the agent runtime *in the CLI*"),
            "header must explain v0.1 agent location:\n{yaml}");
        assert!(yaml.contains("`aislang chat <agent>`"),
            "header must name the chat command:\n{yaml}");
        assert!(yaml.contains("Open WebUI"),
            "header must mention the v0.2 presentation layer:\n{yaml}");
    }

    #[test] fn yaml_header_lists_declared_expose_ports() {
        // The demo declares `deploy "local" { expose = { port = 8080 } }`.
        // v0.1 surfaces it as a header comment (no service binds it).
        let yaml = emit_demo_yaml();
        assert!(yaml.contains("Declared deploy.expose ports"),
            "header must include the expose-ports section:\n{yaml}");
        assert!(yaml.contains("deploy.local.port = 8080"),
            "header must list the demo's expose port:\n{yaml}");
        // CRITICAL: 8080 is NOT bound on any service in v0.1.
        let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let services = value["services"].as_mapping().expect("services map");
        for (name, svc) in services {
            if let Some(ports) = svc["ports"].as_sequence() {
                for p in ports {
                    let s = p.as_str().unwrap_or("");
                    assert!(!s.starts_with("8080:"),
                        "v0.1 must NOT bind deploy.expose port 8080 \
                         (would mislead user); found on service `{}`: `{s}`",
                        name.as_str().unwrap_or("?"));
                }
            }
        }
    }

    #[test] fn no_prompt_hash_env_vars_in_v01_output() {
        // The week-4 design parked prompt hashes on agent_runtime's
        // env block. With agent_runtime gone, the CLI loads prompt
        // hashes from the lockfile directly — no env vars needed.
        let yaml = emit_demo_yaml();
        let body_only: String = yaml.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!body_only.contains("AISLANG_PROMPT_HASH_"),
            "v0.1 emits no AISLANG_PROMPT_HASH_* env vars (CLI reads \
             from lockfile); found in:\n{body_only}");
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

    #[test] fn empty_project_still_emits_clean_yaml() {
        // Synthetic resolved-project with no agents, no mcp_servers,
        // no deploys. Output must still parse and contain LiteLLM +
        // Jaeger + budget_counter (the always-on sidecars). No
        // agent_runtime; the header still leads.
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
        // Body invariants:
        assert!(!artifacts.docker_compose.contains("agent_runtime"),
            "no agent_runtime regardless of project shape:\n{}",
            artifacts.docker_compose);
        // Header still present even on a minimal project.
        assert!(artifacts.docker_compose.starts_with("# Generated by `aislang plan`"));
        // No expose ports → no "Declared deploy.expose ports" section.
        assert!(!artifacts.docker_compose.contains("Declared deploy.expose ports"));
        // YAML still parses cleanly.
        let _: serde_yaml::Value =
            serde_yaml::from_str(&artifacts.docker_compose).expect("parses back");
    }
}

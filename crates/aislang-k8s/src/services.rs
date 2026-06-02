//! Per-service Kubernetes resource builders — Phase 2 Week 14 Tue+Wed.
//!
//! Mirrors `aislang-compose::build_litellm_service` / `build_jaeger_
//! service` / `build_budget_counter_service` — same resolver inputs,
//! same image pins, same env-var contracts.  The K8s side ships
//! resources as Deployment + (Service when externally addressable) +
//! (ConfigMap when config-file-driven), instead of one Compose
//! `services:` entry per role.
//!
//! ## Mapping summary (Decisions 43.2 + 43.3 + 43.4, locked Phase 2 Sunday)
//!
//! | Component         | K8s shape                            | When | Notes |
//! |-------------------|--------------------------------------|------|-------|
//! | `litellm`         | Deployment + Service + ConfigMap     | Tue  | Router YAML → CM |
//! | `jaeger`          | Deployment + Service                 | Tue  | Bundled defaults — no CM |
//! | `budget_counter`  | Deployment only                      | Tue  | Placeholder — no incoming traffic |
//! | `agent_runtime`   | Deployment + Service + ConfigMap     | Wed  | `aislang mcp-serve --http`; project CM |
//!
//! The `agent-runtime` row is Decision 43.3: the same `aislang`
//! binary published to ghcr.io (Phase 2 Week 14 Mon's image job)
//! runs in a pod as `aislang mcp-serve --http 0.0.0.0:<expose.port>
//! /etc/aislang/project.ais`.  Its peer ConfigMap (`aislang-project`,
//! Decision 43.4) bakes the .ais source + `aislang.lock` + every
//! `prompt` body into a single mount at `/etc/aislang/`.  Pod-template
//! annotations carry per-prompt SHA-256 hashes so any body change
//! shifts the annotation → ReplicaSet rolls out → fresh prompts
//! without a manual restart.
//!
//! ## What's NOT here yet
//!
//! - `openwebui` Deployment + Service + PVC + opt-in Ingress — Thu (Decision 43.6).
//! - Init-container that waits on LiteLLM `/health/liveliness` — Thu.

use crate::k8s_types::*;
use crate::labels::{aislang_labels, component_selector};
use aislang_resolver::ResolvedProject;
use std::collections::BTreeMap;

// ============================================================================
// Image pins — match aislang-compose's pins so both targets ship the
// same container versions.  When a pin bumps in aislang-compose it
// MUST bump here too (audited by Fri's k8s-smoke CI job).
// ============================================================================

const LITELLM_IMAGE: &str = "ghcr.io/berriai/litellm:main-stable";
const JAEGER_IMAGE:  &str = "jaegertracing/all-in-one:latest";
const BUDGET_COUNTER_IMAGE: &str = "alpine:3.19";

/// Pinned image for the agent-runtime pod — same `aislang` binary
/// published by Phase 2 Week 14 Mon's release.yml `image` job.
/// `:latest` here is the v0.2 default; Thu's Helm chart promotes
/// this to `values.yaml::image.{repository,tag}` so users can
/// override per-deploy without touching the manifest.
///
/// Why `aislang/aislang` even though the actual ghcr owner varies
/// per-fork: the chart override path is the supported way to point
/// at a fork's own image; the manifest-only path (`kubectl apply
/// -f manifests/`) is a power-user fallback that's expected to
/// `sed -i` the image line.  Decision 43.7: the chart wraps four
/// templating points (image.tag, ingress.enabled, openwebui.
/// persistence, litellm.replicas); this is one of them.
pub const AGENT_RUNTIME_IMAGE: &str = "ghcr.io/aislang/aislang:latest";

/// Default port for the agent-runtime mcp-serve --http binding when
/// the `deploy { expose { port = N } }` field is absent.  8080 matches
/// the informational header line `aislang-compose` emits today
/// (`deploy.<label>.port = 8080`) so behaviour parity holds when a
/// .ais project switches `target = compose → kubernetes` without
/// touching `expose`.
pub const AGENT_RUNTIME_DEFAULT_PORT: u16 = 8080;

// ============================================================================
// Secret name — user-created (Decision 43.5).
// ============================================================================

/// Name of the user-created k8s Secret holding provider credentials.
/// The chart docs the `kubectl create secret generic aislang-providers
/// --from-literal=OPENAI_API_KEY=… --from-literal=ANTHROPIC_API_KEY=…`
/// recipe; we wire `envFrom: [{ secretRef: { name: aislang-providers } }]`
/// on every pod that dials a provider.
///
/// **Decision 43.5 rationale**: auto-creating a Secret from the user's
/// .env would either commit creds to git (chart values.yaml) or require
/// a credential-store sidecar (sealed-secrets, external-secrets) that's
/// out of v0.2 scope.  Documenting the recipe + relying on the user's
/// own `kubectl create secret` keeps v0.2's posture explicit + sets up
/// v0.3 for either sealed-secrets or a cloud-controller hook.
pub const PROVIDERS_SECRET_NAME: &str = "aislang-providers";

// ============================================================================
// OpenTelemetry pod-wiring (Phase 2 Week 15 Tue)
// ============================================================================

/// Default OTLP/HTTP endpoint pods dial to ship spans into the
/// in-cluster Jaeger pod's OTLP receiver.  `jaeger` resolves via
/// K8s DNS (same posture LiteLLM uses to expose its router on
/// `litellm:4000` to pod siblings).  Port 4318 is the OTLP/HTTP
/// receiver; Jaeger already exposes it as a named container port
/// (`otlp-http`) since Tue Week 14.
///
/// Chart users can override at install time:
///   `helm install --set observability.otlp_endpoint=https://<ext>`
/// — useful when they're running an external collector (Honeycomb,
/// Tempo, NewRelic) instead of the bundled Jaeger.  helm_templatize
/// substitutes this string with the Helm-templated `{{ .Values
/// .observability.otlp_endpoint }}` for the chart variant.
pub const OTEL_ENDPOINT_DEFAULT: &str = "http://jaeger:4318";

/// Default service-name prefix for the OpenTelemetry resource
/// attribute `service.name`.  Each pod's `OTEL_SERVICE_NAME` env
/// var becomes `<prefix>.<component>.<deploy-label>` —
/// e.g. `aislang.litellm.prod`, `aislang.agent_runtime.prod`.
///
/// Why prefix-then-component-then-deploy:
///   - Prefix groups all aislang-emitted services together in
///     Jaeger UI's service dropdown (a single `aislang.*` filter
///     surfaces everything).
///   - Component disambiguates within an aislang stack.
///   - Deploy label disambiguates across deploys when a single
///     Jaeger instance serves multi-tenant clusters (per-tenant
///     `helm install` with the chart per Decision 43.1).
pub const OTEL_SERVICE_NAME_PREFIX_DEFAULT: &str = "aislang";

/// OTLP protocol — `http/protobuf` is the v1 OTel standard
/// receiver Jaeger ships by default on 4318.  Pinned here so the
/// LiteLLM + agent-runtime pods agree with each other + with
/// Jaeger's enabled receivers (`COLLECTOR_OTLP_ENABLED=true`,
/// Tue Week 14).  Alternative is `grpc` on port 4317 — declined
/// because LiteLLM's Python SDK ships HTTP-only by default,
/// and gRPC adds a TLS-flag surface we don't need at v0.2.
pub const OTEL_PROTOCOL: &str = "http/protobuf";

/// Build the standard OTLP env-var block for a pod participating
/// in the aislang trace topology.  Three vars:
///
///   - `OTEL_EXPORTER_OTLP_ENDPOINT` — where to ship spans
///   - `OTEL_EXPORTER_OTLP_PROTOCOL` — wire format (http/protobuf)
///   - `OTEL_SERVICE_NAME`           — span `service.name` attribute
///
/// Service name pattern: `<prefix>.<component>.<deploy-label>`
/// (e.g. `aislang.litellm.prod`).  Used by `build_litellm` +
/// `build_agent_runtime`.  Jaeger itself is the receiver, NOT a
/// span producer, so it doesn't get an OTEL_SERVICE_NAME.
pub fn otel_env_vars(component: &str, deploy_label: &str) -> Vec<EnvVar> {
    vec![
        EnvVar {
            name:  "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
            value: OTEL_ENDPOINT_DEFAULT.to_string(),
        },
        EnvVar {
            name:  "OTEL_EXPORTER_OTLP_PROTOCOL".to_string(),
            value: OTEL_PROTOCOL.to_string(),
        },
        EnvVar {
            name:  "OTEL_SERVICE_NAME".to_string(),
            value: format!(
                "{OTEL_SERVICE_NAME_PREFIX_DEFAULT}.{component}.{deploy_label}"),
        },
    ]
}

// ============================================================================
// LiteLLM — Deployment + Service + ConfigMap
// ============================================================================

/// LiteLLM router pod + Service + ConfigMap.  Returns the three
/// resources as separate YAML strings so the caller (lib.rs `emit`)
/// can concatenate them with `---` separators into `litellm.yaml`.
///
/// The router YAML is generated by `aislang_compose::litellm::
/// generate_config` — same code path as the Compose target, single
/// source of truth.  It lands in the `litellm-config` ConfigMap as
/// the value of key `config.yaml`, mounted at `/app/config.yaml`
/// inside the container.
///
/// `envFrom: { secretRef: { name: aislang-providers } }` (Decision
/// 43.5) — every key in the user-created Secret becomes an env var
/// at pod startup.  The Secret MUST contain whatever vars LiteLLM's
/// config references (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
/// `OLLAMA_API_BASE`, …) — the chart's README enumerates them by
/// running `aislang_compose::litellm::referenced_env_vars` at chart-
/// emit time (Thu).
pub fn build_litellm(
    resolved:     &ResolvedProject,
    deploy_label: &str,
    router_yaml:  &str,
) -> (Deployment, Service, ConfigMap) {
    let labels   = aislang_labels("litellm", deploy_label);
    let selector = component_selector("litellm");

    // ── ConfigMap: holds the router YAML keyed as `config.yaml`.
    let mut cm_data = BTreeMap::new();
    cm_data.insert("config.yaml".to_string(), router_yaml.to_string());
    let cm = ConfigMap::new(
        ObjectMeta::new("litellm-config", labels.clone()),
        cm_data,
    );

    // ── Container: LiteLLM serving on 4000, config mounted from CM.
    let container = Container {
        name:  "litellm".to_string(),
        image: LITELLM_IMAGE.to_string(),
        ports: Some(vec![ContainerPort {
            container_port: 4000,
            name:           "http".to_string(),
            protocol:       None,                       // TCP
        }]),
        // AISLANG_CATALOG_VERSION pinned for traceability — same
        // role it plays in the Compose env (debug "which catalog
        // was this stack resolved against?").  Phase 2 Week 15
        // Tue: OTEL env vars appended so LiteLLM ships its
        // request-/response- spans into the in-cluster Jaeger
        // pod automatically — no manual env override required.
        env: Some({
            let mut env = vec![EnvVar {
                name:  "AISLANG_CATALOG_VERSION".to_string(),
                value: resolved.catalog_version.clone(),
            }];
            env.extend(otel_env_vars("litellm", deploy_label));
            env
        }),
        // Provider creds via envFrom (Decision 43.5).
        env_from: Some(vec![EnvFromSource {
            secret_ref:     Some(SecretEnvSource {
                name:     PROVIDERS_SECRET_NAME.to_string(),
                optional: None,                         // = false (loud failure)
            }),
            config_map_ref: None,
        }]),
        command: None,
        // CMD-style args: pod's entrypoint is the LiteLLM image's
        // default `litellm` binary; we pass `--config /app/config
        // .yaml --port 4000` exactly mirroring the Compose service's
        // command list.
        args: Some(vec![
            "--config".to_string(), "/app/config.yaml".to_string(),
            "--port".to_string(),   "4000".to_string(),
        ]),
        volume_mounts: Some(vec![VolumeMount {
            name:       "config".to_string(),
            mount_path: "/app/config.yaml".to_string(),
            read_only:  Some(true),
            // sub_path selects the `config.yaml` key as a single
            // file rather than mounting the whole CM as a directory.
            // Without sub_path, /app/config.yaml would BE the CM
            // directory (containing one file named config.yaml)
            // and LiteLLM would `open("/app/config.yaml")` and get
            // EISDIR.  sub_path makes the mount file-shaped.
            sub_path:   Some("config.yaml".to_string()),
        }]),
    };

    let pod_template = PodTemplate {
        // Pod-template metadata: only labels matter for Service
        // selector matching.  Name is set by the ReplicaSet K8s
        // controller; we don't need to specify here.
        metadata: ObjectMeta {
            name:        String::new(),
            namespace:   None,
            labels:      labels.clone(),
            annotations: BTreeMap::new(),
        },
        spec: PodSpec {
            containers:      vec![container],
            volumes:         Some(vec![Volume {
                name:                       "config".to_string(),
                config_map:                 Some(ConfigMapVolumeSource {
                    name:  "litellm-config".to_string(),
                    items: None,                       // Mount all keys as files
                }),
                persistent_volume_claim:    None,
            }]),
            init_containers: None,
        },
    };

    let deployment = Deployment::new(
        ObjectMeta::new("litellm", labels.clone()),
        DeploymentSpec {
            replicas: 1,
            selector: LabelSelector { match_labels: selector.clone() },
            template: pod_template,
        },
    );

    let service = Service::new(
        ObjectMeta::new("litellm", labels.clone()),
        ServiceSpec {
            selector,
            ports: vec![ServicePort {
                port:        4000,
                target_port: "http".to_string(),       // Named ContainerPort
                name:        "http".to_string(),
                protocol:    None,
            }],
            type_: None,                               // = ClusterIP
        },
    );

    (deployment, service, cm)
}

// ============================================================================
// Jaeger — Deployment + Service (no ConfigMap, bundled defaults)
// ============================================================================

/// Jaeger all-in-one pod + Service.  No ConfigMap — Jaeger's
/// defaults work out of the box for receive-spans-on-OTLP +
/// serve-UI-on-16686.  Same image pin as `aislang-compose`'s
/// jaeger service.
///
/// Service exposes 3 ports (UI + OTLP gRPC + OTLP HTTP); each
/// uses a named target_port that matches the ContainerPort name
/// inside the pod.  Multi-port Services REQUIRE per-port names
/// per K8s schema (the v1 API server rejects unnamed ports when
/// >1 is declared).
pub fn build_jaeger(deploy_label: &str) -> (Deployment, Service) {
    let labels   = aislang_labels("jaeger", deploy_label);
    let selector = component_selector("jaeger");

    let container = Container {
        name:  "jaeger".to_string(),
        image: JAEGER_IMAGE.to_string(),
        ports: Some(vec![
            ContainerPort {
                container_port: 16686,
                name:           "ui".to_string(),
                protocol:       None,
            },
            ContainerPort {
                container_port: 4317,
                name:           "otlp-grpc".to_string(),
                protocol:       None,
            },
            ContainerPort {
                container_port: 4318,
                name:           "otlp-http".to_string(),
                protocol:       None,
            },
        ]),
        env: Some(vec![EnvVar {
            name:  "COLLECTOR_OTLP_ENABLED".to_string(),
            value: "true".to_string(),
        }]),
        // No envFrom on Jaeger — it never dials providers.
        env_from:      None,
        command:       None,
        args:          None,
        volume_mounts: None,
    };

    let pod_template = PodTemplate {
        metadata: ObjectMeta {
            name:        String::new(),
            namespace:   None,
            labels:      labels.clone(),
            annotations: BTreeMap::new(),
        },
        spec: PodSpec {
            containers:      vec![container],
            volumes:         None,
            init_containers: None,
        },
    };

    let deployment = Deployment::new(
        ObjectMeta::new("jaeger", labels.clone()),
        DeploymentSpec {
            replicas: 1,
            selector: LabelSelector { match_labels: selector.clone() },
            template: pod_template,
        },
    );

    let service = Service::new(
        ObjectMeta::new("jaeger", labels),
        ServiceSpec {
            selector,
            ports: vec![
                ServicePort {
                    port:        16686,
                    target_port: "ui".to_string(),
                    name:        "ui".to_string(),
                    protocol:    None,
                },
                ServicePort {
                    port:        4317,
                    target_port: "otlp-grpc".to_string(),
                    name:        "otlp-grpc".to_string(),
                    protocol:    None,
                },
                ServicePort {
                    port:        4318,
                    target_port: "otlp-http".to_string(),
                    name:        "otlp-http".to_string(),
                    protocol:    None,
                },
            ],
            type_: None,                               // = ClusterIP
        },
    );

    (deployment, service)
}

// ============================================================================
// budget_counter — Deployment only (placeholder; no Service)
// ============================================================================

/// budget_counter placeholder Deployment.  Same `alpine:3.19`
/// posture as `aislang-compose::build_budget_counter_service` —
/// sleep-infinity loop, real circuit-breaker is v0.3+.  No
/// Service: budget_counter has no incoming traffic + no externally-
/// observable surface.
///
/// Cap env vars sourced from `resolved.project.budget`; if absent,
/// the pod still runs but with no caps set (degraded-but-running
/// posture, same as Compose).
pub fn build_budget_counter(
    resolved:     &ResolvedProject,
    deploy_label: &str,
) -> Deployment {
    let labels   = aislang_labels("budget_counter", deploy_label);
    let selector = component_selector("budget_counter");

    let mut env = Vec::new();
    if let Some(b) = &resolved.project.budget {
        env.push(EnvVar {
            name:  "AISLANG_HARD_CAP_PER_DAY_MICRO".to_string(),
            value: b.hard_cap_per_day.micro_units()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "unlimited".to_string()),
        });
        env.push(EnvVar {
            name:  "AISLANG_HARD_CAP_PER_RUN_MICRO".to_string(),
            value: b.hard_cap_per_run.micro_units()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "unlimited".to_string()),
        });
    }

    let container = Container {
        name:  "budget-counter".to_string(),
        image: BUDGET_COUNTER_IMAGE.to_string(),
        ports: None,
        env:   if env.is_empty() { None } else { Some(env) },
        // No envFrom — budget_counter doesn't need provider creds.
        env_from: None,
        command:  Some(vec!["sh".to_string(), "-c".to_string()]),
        args:     Some(vec![
            "echo 'budget-counter placeholder; real circuit-breaker arrives \
             v0.3+. Cap env vars are set; an external counter could read \
             them now.' && sleep infinity".to_string(),
        ]),
        volume_mounts: None,
    };

    let pod_template = PodTemplate {
        metadata: ObjectMeta {
            name:        String::new(),
            namespace:   None,
            labels:      labels.clone(),
            annotations: BTreeMap::new(),
        },
        spec: PodSpec {
            containers:      vec![container],
            volumes:         None,
            init_containers: None,
        },
    };

    Deployment::new(
        ObjectMeta::new("budget-counter", labels),
        DeploymentSpec {
            replicas: 1,
            selector: LabelSelector { match_labels: selector },
            template: pod_template,
        },
    )
}

// ============================================================================
// agent-runtime — Deployment + Service (Decision 43.3, Phase 2 Week 14 Wed)
// ============================================================================

/// Filenames inside the `aislang-project` ConfigMap.  Pinned so the
/// pod's `--config` / `--lockfile` paths (Wed onward) reference
/// stable mount points + so `kubectl describe configmap aislang-
/// project` shows expected keys.
pub const PROJECT_CM_AIS_KEY:      &str = "project.ais";
pub const PROJECT_CM_LOCKFILE_KEY: &str = "aislang.lock";

/// v0.3 Week 16 Thu — multi-module ConfigMap data key for the project
/// manifest (`aislang.toml`).  When the CM holds per-module sources
/// instead of a single `project.ais`, this key carries the manifest
/// body so `aislang mcp-serve` inside the pod can autodiscover modules
/// the same way the CLI does outside the cluster.
pub const PROJECT_CM_MANIFEST_KEY: &str = "aislang.toml";

/// Directory where the `aislang-project` ConfigMap mounts inside
/// the agent-runtime pod.  Pod command: `aislang mcp-serve --http
/// 0.0.0.0:<port> /etc/aislang/project.ais` (Decision 43.3).
pub const PROJECT_MOUNT_DIR: &str = "/etc/aislang";

/// Sub-directory inside [`PROJECT_MOUNT_DIR`] where per-prompt body
/// files land.  One file per `prompt` block keyed by `prompt.label`
/// (e.g. `/etc/aislang/prompts/system.md`).  The .ais source still
/// declares prompts by relative path under the user's source dir;
/// `aislang mcp-serve` doesn't (yet) override the lookup path inside
/// the pod — that's a v0.3 carry-over (the source's `./prompts/foo
/// .md` path won't resolve to /etc/aislang/prompts/foo.md by default).
/// For Wed, the CM keys + mount paths are emitted; rewiring mcp-serve
/// to consult `/etc/aislang/prompts/` first is tracked as Wed follow-up.
pub const PROJECT_PROMPTS_SUBDIR: &str = "prompts";

/// Per-prompt pod-template annotation prefix.  K8s pod-templates re-
/// roll the ReplicaSet whenever any pod-template byte changes, so
/// stamping `aislang.prompt-hash/<label>: <sha256>` on the template
/// means a prompt body edit (which shifts the hash in
/// `ResolvedProject.prompt_hashes`) automatically triggers a
/// rolling restart on the next `helm upgrade` / `kubectl apply`.
/// Decision 43.4 in one line of YAML per prompt.
///
/// Why per-prompt (not one aggregate hash): `kubectl describe pod
/// agent-runtime-…` then shows exactly which prompt changed since
/// last apply.  Operator debuggability over byte-thrift.
pub const PROMPT_HASH_ANNOTATION_PREFIX: &str = "aislang.prompt-hash/";

/// Annotation for the .ais source + lockfile fingerprint.  Same
/// rollout-trigger pattern as the per-prompt annotations: any
/// change to source or lockfile shifts this annotation → fresh
/// pods.  One annotation key, deterministic SHA-256 of the
/// concatenated `(.ais source || lockfile)` body.
pub const PROJECT_HASH_ANNOTATION: &str = "aislang.project-hash";

/// Resolve the agent-runtime listening port from the deploy block.
/// Returns the user-declared `expose.port` when present, else the
/// emitter's [`AGENT_RUNTIME_DEFAULT_PORT`] fallback.  Decision
/// 43.3: `expose.port` finally becomes functional under K8s
/// (Compose left it as a header comment only because no v0.1
/// service bound it).
pub fn agent_runtime_port(resolved: &ResolvedProject, deploy_label: &str) -> u16 {
    resolved.project.deploys.iter()
        .find(|d| d.label == deploy_label)
        .and_then(|d| d.expose.as_ref())
        .and_then(|e| e.port)
        .unwrap_or(AGENT_RUNTIME_DEFAULT_PORT)
}

/// Build the `aislang-project` ConfigMap holding the .ais source +
/// lockfile + per-prompt bodies (Decision 43.4).
///
/// Keys:
///   - `project.ais`      — full .ais source verbatim
///   - `aislang.lock`     — lockfile body (empty string if absent —
///                          `--no-lock` path, or a project that's
///                          never been `aislang plan`-ed before)
///   - `<prompt-label>`   — prompt body, one entry per `prompt` block
///                          (lives under the `prompts/` sub-directory
///                          via the items[] indirection on the
///                          ConfigMapVolumeSource — Wed wires that in
///                          `build_agent_runtime` below)
///
/// Determinism: BTreeMap iteration is sorted by key, so the emitted
/// ConfigMap YAML is byte-stable across plan runs.  Locked Decision
/// 43 determinism contract — re-running `aislang plan` on unchanged
/// inputs produces byte-identical artifacts.
pub fn build_aislang_project_configmap(
    deploy_label:  &str,
    ais_source:    &str,
    lockfile:      Option<&str>,
    prompt_bodies: &BTreeMap<String, String>,
) -> ConfigMap {
    build_aislang_project_configmap_inner(
        deploy_label,
        ais_source,
        lockfile,
        prompt_bodies,
        &BTreeMap::new(),     // no module sources
        None,                 // no manifest
    )
}

/// v0.3 Week 16 Thu — multi-module CM variant.  Same shape as the
/// single-file builder above but writes one data key per module
/// (`<name>.ais` → source bytes) + a `PROJECT_CM_MANIFEST_KEY`
/// entry for the `aislang.toml` body, INSTEAD OF the single
/// `PROJECT_CM_AIS_KEY` entry.  Lockfile + prompt bodies behave
/// identically.
///
/// Callers: the K8s `emit()` entry point — picks this variant
/// when `sources.is_multi_module()` returns true.
pub fn build_aislang_project_configmap_multi(
    deploy_label:    &str,
    module_sources:  &BTreeMap<String, String>,
    manifest:        Option<&str>,
    lockfile:        Option<&str>,
    prompt_bodies:   &BTreeMap<String, String>,
) -> ConfigMap {
    build_aislang_project_configmap_inner(
        deploy_label,
        "",                  // no single ais_source in multi-module mode
        lockfile,
        prompt_bodies,
        module_sources,
        manifest,
    )
}

/// Common backing implementation.  Single-file callers pass an
/// `ais_source` + empty module map; multi-module callers pass an
/// empty `ais_source` + populated module map.
///
/// The data layout pins the on-disk shape inside the pod's mounted
/// CM volume (one file per data key, plus the items[] remap to
/// drop prompt bodies into `prompts/<label>`).  When multi-module,
/// per-module `.ais` files land at `/etc/aislang/<name>.ais` and
/// `aislang.toml` at `/etc/aislang/aislang.toml`, mirroring exactly
/// what the dir-input CLI sees on the host filesystem.
fn build_aislang_project_configmap_inner(
    deploy_label:    &str,
    ais_source:      &str,
    lockfile:        Option<&str>,
    prompt_bodies:   &BTreeMap<String, String>,
    module_sources:  &BTreeMap<String, String>,
    manifest:        Option<&str>,
) -> ConfigMap {
    let labels = aislang_labels("agent_runtime", deploy_label);
    let mut data = BTreeMap::new();
    if module_sources.is_empty() {
        // Single-file (v0.2 baseline).
        data.insert(PROJECT_CM_AIS_KEY.to_string(), ais_source.to_string());
    } else {
        // Multi-module (v0.3 Thu).
        for (name, source) in module_sources {
            data.insert(format!("{name}.ais"), source.clone());
        }
        if let Some(m) = manifest {
            data.insert(PROJECT_CM_MANIFEST_KEY.to_string(), m.to_string());
        }
    }
    data.insert(PROJECT_CM_LOCKFILE_KEY.to_string(),
        lockfile.unwrap_or("").to_string());
    for (label, body) in prompt_bodies {
        // Prompt-label key.  Mounted as `/etc/aislang/prompts/<label>`
        // via items[] mapping in build_agent_runtime — see the
        // VolumeMount sub_path/items dance there.
        data.insert(label.clone(), body.clone());
    }
    ConfigMap::new(
        ObjectMeta::new("aislang-project", labels),
        data,
    )
}

/// Build the agent-runtime Deployment + Service.  The Deployment
/// runs the published aiSlang binary as `aislang mcp-serve --http
/// 0.0.0.0:<port> <PROJECT_MOUNT_DIR>/<PROJECT_CM_AIS_KEY>`; the
/// Service is a ClusterIP exposing the same port.  envFrom:
/// secretRef: aislang-providers (Decision 43.5) so the pod can
/// dial providers via LiteLLM exactly like the in-cluster LiteLLM
/// can.
///
/// Pod-template annotations carry one entry per prompt
/// (`aislang.prompt-hash/<label>: <sha256>`) plus a single
/// project-fingerprint annotation (`aislang.project-hash:
/// <sha256-of-(source||lockfile)>`).  When any of these shift,
/// K8s creates a new ReplicaSet → rolling restart picks up the
/// updated ConfigMap content automatically.  Decision 43.4 in
/// action.
pub fn build_agent_runtime(
    resolved:      &ResolvedProject,
    deploy_label:  &str,
    ais_source:    &str,
    lockfile:      Option<&str>,
) -> (Deployment, Service) {
    // v0.3 Week 16 Thu: backward-compat shim — single-file callers
    // pass through this 2-arg form unchanged; multi-module callers
    // go through [`build_agent_runtime_with_sources`].
    let sources = crate::ProjectSources {
        ais_source:    ais_source.to_string(),
        lockfile:      lockfile.map(str::to_string),
        prompt_bodies: BTreeMap::new(),    // annotations come from
                                           // `resolved.prompt_hashes`
                                           // directly; the bag's
                                           // prompt_bodies is for the
                                           // CM builder only.
        modules:       BTreeMap::new(),
        manifest:      None,
    };
    build_agent_runtime_with_sources(resolved, deploy_label, &sources)
}

/// v0.3 Week 16 Thu — full-fat agent-runtime builder.  Routes the
/// single-file vs multi-module branches based on
/// `sources.is_multi_module()`.  Single-file behaviour is byte-
/// identical to [`build_agent_runtime`] (regression-pinned via the
/// existing v0.2 tests + the v0.2 fixture comparison).
///
/// Multi-module changes:
///   - cm_items lists `<module>.ais` (one per declared module) +
///     `aislang.toml`, INSTEAD OF the single `project.ais`.
///   - Pod `args` point at `PROJECT_MOUNT_DIR` (the mount directory)
///     so `aislang mcp-serve` runs the same dir-vs-file dispatch
///     the CLI does outside the cluster.
///   - The `aislang.project-hash` annotation hashes the
///     deterministic concatenation of every module's source +
///     manifest + lockfile, so a byte change in ANY module rolls
///     the ReplicaSet (matches the lockfile reconciler's "any
///     module changed → re-resolve everything" v0.3 semantic).
pub fn build_agent_runtime_with_sources(
    resolved:      &ResolvedProject,
    deploy_label:  &str,
    sources:       &crate::ProjectSources,
) -> (Deployment, Service) {
    let labels   = aislang_labels("agent_runtime", deploy_label);
    let selector = component_selector("agent_runtime");
    let port     = agent_runtime_port(resolved, deploy_label);

    // ── Annotations on the pod template (NOT on the Deployment
    //    metadata — only pod-template changes trigger rollouts).
    let mut annotations = BTreeMap::new();
    for (label, hash) in &resolved.prompt_hashes {
        annotations.insert(
            format!("{PROMPT_HASH_ANNOTATION_PREFIX}{label}"),
            hash.clone(),
        );
    }
    annotations.insert(
        PROJECT_HASH_ANNOTATION.to_string(),
        if sources.is_multi_module() {
            sha256_hex_of_multi_module(sources)
        } else {
            sha256_hex_of_source_and_lockfile(
                &sources.ais_source, sources.lockfile.as_deref())
        },
    );

    // ── Volume mounts: aislang-project CM mounted at /etc/aislang.
    //
    // items[] selects + remaps keys: `project.ais` + `aislang.lock`
    // (single-file) OR `<module>.ais` + `aislang.toml` + `aislang.lock`
    // (multi-module) land at the mount root; each `<prompt-label>` key
    // lands inside the `prompts/` sub-directory.  Without items[],
    // every CM key (including prompt-label keys) would mount at the
    // root and overwrite each other if any label happened to be
    // `project.ais` (unlikely but possible) — explicit remapping is
    // safer + makes the on-disk layout match the .ais user's intuition.
    let mut cm_items = if sources.is_multi_module() {
        let mut v = Vec::with_capacity(sources.modules.len() + 2);
        for name in sources.modules.keys() {
            let path = format!("{name}.ais");
            v.push(KeyToPath { key: path.clone(), path });
        }
        v.push(KeyToPath {
            key:  PROJECT_CM_MANIFEST_KEY.to_string(),
            path: PROJECT_CM_MANIFEST_KEY.to_string(),
        });
        v.push(KeyToPath {
            key:  PROJECT_CM_LOCKFILE_KEY.to_string(),
            path: PROJECT_CM_LOCKFILE_KEY.to_string(),
        });
        v
    } else {
        vec![
            KeyToPath {
                key:  PROJECT_CM_AIS_KEY.to_string(),
                path: PROJECT_CM_AIS_KEY.to_string(),
            },
            KeyToPath {
                key:  PROJECT_CM_LOCKFILE_KEY.to_string(),
                path: PROJECT_CM_LOCKFILE_KEY.to_string(),
            },
        ]
    };
    // Sorted iteration over prompt labels for determinism.
    let mut prompt_labels: Vec<&String> =
        resolved.prompt_hashes.keys().collect();
    prompt_labels.sort();
    for label in prompt_labels {
        cm_items.push(KeyToPath {
            key:  label.clone(),
            path: format!("{PROJECT_PROMPTS_SUBDIR}/{label}"),
        });
    }

    // ── Container spec.
    let container = Container {
        name:  "agent-runtime".to_string(),
        image: AGENT_RUNTIME_IMAGE.to_string(),
        ports: Some(vec![ContainerPort {
            container_port: port,
            name:           "mcp-http".to_string(),
            protocol:       None,
        }]),
        // Provider creds via the same Secret LiteLLM uses (Decision
        // 43.5).  mcp-serve dials providers via LiteLLM but ALSO
        // takes provider creds directly when running .env-style
        // fallbacks; keeping the same envFrom posture means a single
        // Secret update covers both pods.
        env_from: Some(vec![EnvFromSource {
            secret_ref:     Some(SecretEnvSource {
                name:     PROVIDERS_SECRET_NAME.to_string(),
                optional: None,
            }),
            config_map_ref: None,
        }]),
        // Phase 2 Week 15 Tue: OTEL env vars so the agent-runtime
        // pod's `aislang mcp-serve --http` spans (Decision 18 /
        // Week 9 v0.1 work — OTel-GenAI semantic conventions on
        // chat_turn / eval / tool_use_iteration) flow into the
        // in-cluster Jaeger pod automatically.  Service name
        // pattern is `aislang.agent_runtime.<deploy-label>` —
        // distinguishes deploys when a single Jaeger sees traces
        // from multiple aislang stacks (multi-tenant clusters).
        env: Some(otel_env_vars("agent_runtime", deploy_label)),
        // Default image entrypoint is `aislang`; pass the mcp-serve
        // subcommand + transport flag as args.  `--http 0.0.0.0:<port>`
        // binds on every pod interface so the Service can reach it.
        //
        // **No trailing positional source-path argument.** The CLI's
        // `mcp-serve` subcommand doesn't accept one — clients pass
        // `source_path` / `source_dir` per-`aislang_chat_turn` call
        // in the MCP request, which is the same posture the local
        // `aislang chat … <project>` flow takes after Week 17 Tue's
        // dir-input dispatch.  An extra positional here crashes the
        // pod at startup with "unexpected argument", which is what
        // the v0.3 Week 18 Wed k8s-smoke CI failure surfaced.
        //
        // The CM-mounted project lives at `PROJECT_MOUNT_DIR`
        // (`/etc/aislang/`); MCP clients send that path explicitly
        // in their chat_turn args.  See `aislang-runtime::mcp_server::
        // chat_turn` for the per-call source_path/dir contract.
        command: None,
        args: Some(vec![
            "mcp-serve".to_string(),
            "--http".to_string(),
            format!("0.0.0.0:{port}"),
        ]),
        volume_mounts: Some(vec![VolumeMount {
            name:       "project".to_string(),
            mount_path: PROJECT_MOUNT_DIR.to_string(),
            read_only:  Some(true),
            // No sub_path here — we want the WHOLE CM mounted as a
            // directory at /etc/aislang/, with items[] selecting
            // which keys appear and where.
            sub_path:   None,
        }]),
    };

    let pod_template = PodTemplate {
        metadata: ObjectMeta {
            name:        String::new(),
            namespace:   None,
            labels:      labels.clone(),
            annotations,
        },
        spec: PodSpec {
            containers: vec![container],
            volumes:    Some(vec![Volume {
                name:                    "project".to_string(),
                config_map:              Some(ConfigMapVolumeSource {
                    name:  "aislang-project".to_string(),
                    items: Some(cm_items),
                }),
                persistent_volume_claim: None,
            }]),
            init_containers: None,
        },
    };

    let deployment = Deployment::new(
        ObjectMeta::new("agent-runtime", labels.clone()),
        DeploymentSpec {
            replicas: 1,
            selector: LabelSelector { match_labels: selector.clone() },
            template: pod_template,
        },
    );

    let service = Service::new(
        ObjectMeta::new("agent-runtime", labels),
        ServiceSpec {
            selector,
            ports: vec![ServicePort {
                port,
                target_port: "mcp-http".to_string(),
                name:        "mcp-http".to_string(),
                protocol:    None,
            }],
            type_: None,                                  // = ClusterIP
        },
    );

    (deployment, service)
}

/// SHA-256 of `<.ais source>||<lockfile-or-empty>` rendered as hex.
/// Drives the `aislang.project-hash` pod-template annotation —
/// any byte change in either input shifts the hash → ReplicaSet
/// rolls out.  Same `sha2` crate the resolver uses for prompt
/// hashing (shared dep transitively via aislang-resolver).
/// v0.3 Week 16 Thu — project-fingerprint hash for multi-module
/// projects.  Deterministically combines:
///
///   1. Each module's name + source bytes (BTreeMap order)
///   2. The `aislang.toml` manifest body (if present)
///   3. The lockfile body (if present)
///
/// Same `||` separator + per-key tagging style as
/// [`sha256_hex_of_source_and_lockfile`], so reverse-debug paths can
/// distinguish the inputs.  Any byte change in ANY module shifts the
/// hash → ReplicaSet rolls (matches the lockfile reconciler's "any
/// module changed → re-resolve" v0.3 semantic).
fn sha256_hex_of_multi_module(sources: &crate::ProjectSources) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for (name, body) in &sources.modules {
        h.update(b"module:");
        h.update(name.as_bytes());
        h.update(b"||");
        h.update(body.as_bytes());
        h.update(b"||");
    }
    if let Some(m) = &sources.manifest {
        h.update(b"manifest||");
        h.update(m.as_bytes());
        h.update(b"||");
    }
    if let Some(l) = &sources.lockfile {
        h.update(b"lockfile||");
        h.update(l.as_bytes());
    }
    let bytes = h.finalize();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes { out.push_str(&format!("{b:02x}")); }
    out
}

fn sha256_hex_of_source_and_lockfile(ais_source: &str, lockfile: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(ais_source.as_bytes());
    // `||` separator keeps source + lockfile unambiguously
    // distinguishable from a single concatenated string when the
    // hash is reverse-debugged.  Two pipe chars are unlikely in
    // .ais bodies (comments could carry them, but the separator
    // doesn't need to be input-free — it just needs to be present
    // when we KNOW what to look for).
    h.update(b"||");
    if let Some(l) = lockfile {
        h.update(l.as_bytes());
    }
    let bytes = h.finalize();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

// ============================================================================
// Open WebUI — Deployment + Service + PVC (Decision 43.6, Phase 2 Week 14 Thu)
// ============================================================================

/// Pinned image for the Open WebUI sidecar.  Same `:main` pin
/// `aislang-compose` uses — single source of truth across deploy
/// targets.  Open WebUI's docs recommend `:main` for self-hosted
/// installs (they cut versioned tags far less frequently than
/// upstream commits).
pub const OPENWEBUI_IMAGE: &str = "ghcr.io/open-webui/open-webui:main";

/// Open WebUI listens on 8080 inside the container.  Same number
/// `aislang-compose` exposes (it remaps to host 3000 via `3000:8080`).
/// Under K8s, the Service exposes 8080 directly; users either
/// `kubectl port-forward svc/openwebui 3000:8080` to mimic the
/// Compose URL, or hit the Ingress when one is emitted
/// (`expose.ingress = "<host>"`).
pub const OPENWEBUI_CONTAINER_PORT: u16 = 8080;

/// PVC default size — Open WebUI's SQLite + uploads grow slowly for
/// the demo path; 1Gi covers a few hundred conversation turns + a
/// dozen uploaded files.  Overridable at chart-render time via
/// `helm install --set openwebui.persistence.size=…`.  Bumped to
/// 5Gi+ by users running real workloads.
pub const OPENWEBUI_DEFAULT_PVC_SIZE: &str = "1Gi";

/// PVC name — pinned constant so the Deployment's volume reference
/// + the PVC's metadata.name + the chart's values-yaml override
/// path all line up without string-format drift.
pub const OPENWEBUI_PVC_NAME: &str = "openwebui-data";

/// Build the Open WebUI Deployment + Service + PVC bundle.
///
/// Env-var contract (mirrors `aislang-compose::build_openwebui_service`):
///   - `OPENAI_API_BASE_URL=http://litellm:4000/v1` — dials the
///     in-cluster LiteLLM via K8s DNS (no host-side port-forward
///     needed because pod-to-pod traffic resolves the litellm
///     Service name natively).
///   - `OPENAI_API_KEY=sk-aislang-litellm-internal` — any non-
///     empty string satisfies Open WebUI's client-side requirement;
///     LiteLLM accepts everything at the master-key layer (v0.2
///     doesn't enable LiteLLM master-key auth).
///   - `WEBUI_AUTH=true` (Open WebUI default) — keeps the local
///     sign-up flow; persisted under /app/backend/data on the PVC.
///
/// Service: ClusterIP-only.  External access is via Ingress
/// (emitted separately by `build_openwebui_ingress` when
/// `expose.ingress = "<host>"`) OR `kubectl port-forward`.  Not
/// emitting a NodePort because it's noisy on shared clusters +
/// would conflict between deploys in multi-tenant kind setups.
pub fn build_openwebui(deploy_label: &str, pvc_size: &str)
    -> (Deployment, Service, PersistentVolumeClaim)
{
    let labels   = aislang_labels("openwebui", deploy_label);
    let selector = component_selector("openwebui");

    // ── PVC: /app/backend/data persistent storage.
    let mut requests = BTreeMap::new();
    requests.insert("storage".to_string(), pvc_size.to_string());
    let pvc = PersistentVolumeClaim::new(
        ObjectMeta::new(OPENWEBUI_PVC_NAME, labels.clone()),
        PersistentVolumeClaimSpec {
            access_modes:      vec!["ReadWriteOnce".to_string()],
            resources:         PersistentVolumeClaimResources { requests },
            storage_class_name: None,  // = cluster default
        },
    );

    // ── Container.
    let mut env = Vec::new();
    env.push(EnvVar {
        name:  "OPENAI_API_BASE_URL".to_string(),
        // Same in-cluster LiteLLM target the Compose openwebui uses,
        // resolved via K8s service DNS (`litellm.<namespace>.svc
        // .cluster.local`, but the unqualified `litellm` name works
        // when both pods land in the same namespace — chart users
        // running `helm install -n <ns>` get this for free).
        value: "http://litellm:4000/v1".to_string(),
    });
    env.push(EnvVar {
        name:  "OPENAI_API_KEY".to_string(),
        value: "sk-aislang-litellm-internal".to_string(),
    });

    let container = Container {
        name:  "openwebui".to_string(),
        image: OPENWEBUI_IMAGE.to_string(),
        ports: Some(vec![ContainerPort {
            container_port: OPENWEBUI_CONTAINER_PORT,
            name:           "http".to_string(),
            protocol:       None,
        }]),
        env:      Some(env),
        // No envFrom — Open WebUI dials the in-cluster LiteLLM, not
        // providers directly, so the providers Secret isn't needed
        // on this pod.  Cleaner permissions surface (only LiteLLM +
        // agent-runtime can read the providers Secret).
        env_from: None,
        command:  None,
        args:     None,
        volume_mounts: Some(vec![VolumeMount {
            name:       "data".to_string(),
            mount_path: "/app/backend/data".to_string(),
            read_only:  None,         // = false (rw — SQLite needs write)
            sub_path:   None,
        }]),
    };

    let pod_template = PodTemplate {
        metadata: ObjectMeta {
            name:        String::new(),
            namespace:   None,
            labels:      labels.clone(),
            annotations: BTreeMap::new(),
        },
        spec: PodSpec {
            containers: vec![container],
            volumes:    Some(vec![Volume {
                name:                    "data".to_string(),
                config_map:              None,
                persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                    claim_name: OPENWEBUI_PVC_NAME.to_string(),
                }),
            }]),
            init_containers: None,
        },
    };

    let deployment = Deployment::new(
        ObjectMeta::new("openwebui", labels.clone()),
        DeploymentSpec {
            replicas: 1,
            selector: LabelSelector { match_labels: selector.clone() },
            template: pod_template,
        },
    );

    let service = Service::new(
        ObjectMeta::new("openwebui", labels),
        ServiceSpec {
            selector,
            ports: vec![ServicePort {
                port:        OPENWEBUI_CONTAINER_PORT,
                target_port: "http".to_string(),
                name:        "http".to_string(),
                protocol:    None,
            }],
            type_: None,                                  // = ClusterIP
        },
    );

    (deployment, service, pvc)
}

// ============================================================================
// Ingress — opt-in (Decision 43.6, Phase 2 Week 14 Thu)
// ============================================================================

/// Build the optional Ingress fronting the openwebui Service.
/// Returns `Some` only when both:
///   - the deploy block's `expose.web_ui = true` (so an openwebui
///     Service exists for the Ingress to route to), AND
///   - `expose.ingress = Some("<host>")` is set.
///
/// `None` in every other shape — including the cases where
/// `expose.ingress` is set but `web_ui = false` (which makes no
/// sense and is reported as a warning by the emitter's caller in
/// `lib.rs::emit`).
pub fn build_openwebui_ingress(deploy_label: &str, host: &str) -> Ingress {
    let labels = aislang_labels("openwebui", deploy_label);
    Ingress::new(
        ObjectMeta::new("openwebui", labels),
        IngressSpec {
            rules: vec![IngressRule {
                host: host.to_string(),
                http: IngressRuleHttp {
                    paths: vec![HttpIngressPath {
                        path:      "/".to_string(),
                        // `Prefix` matches every URL Open WebUI ever uses
                        // (root + /api/* + /static/* + websocket /ws/*).
                        path_type: "Prefix".to_string(),
                        backend: IngressBackend {
                            service: IngressServiceBackend {
                                name: "openwebui".to_string(),
                                port: IngressBackendServicePort {
                                    name: "http".to_string(),
                                },
                            },
                        },
                    }],
                },
            }],
        },
    )
}

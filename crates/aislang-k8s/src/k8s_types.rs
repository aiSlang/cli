//! Typed Kubernetes resource structs — minimal subset for the
//! aiSlang emitter.
//!
//! **Why typed structs and not string templates** (Decision 43, locked
//! Phase 2 Sunday): same posture as `aislang-compose`'s `ComposeFile`
//! / `Service` types — `serde_yaml::to_string` over Rust structs gives
//! deterministic field ordering, no quoting bugs, and the Rust type
//! system enforces schema-level correctness at compile time.  K8s YAML
//! is verbose but the struct surface stays manageable because we only
//! model the fields we actually emit.
//!
//! **Naming convention**: K8s YAML is camelCase; we use snake_case in
//! Rust with `#[serde(rename_all = "camelCase")]` per struct.  This
//! lets the code read like Rust while the wire format stays canonical
//! K8s.
//!
//! **`Option::is_none` skipping**: every optional field is
//! `#[serde(skip_serializing_if = "Option::is_none")]` so an absent
//! field doesn't emit `key: null` (which K8s admission webhooks
//! sometimes reject for fields they treat as a required-when-present
//! discriminant).
//!
//! **What's NOT here**: ReplicaSet, DaemonSet, StatefulSet, Job,
//! CronJob, Ingress, PVC, NetworkPolicy, RBAC.  Add when a Week 14
//! deliverable needs them — Thu adds Ingress + PVC for Open WebUI;
//! v0.3 may add others.  Keeping the surface small here keeps the
//! tests we owe small too.

use serde::Serialize;
use std::collections::BTreeMap;

// ============================================================================
// Common metadata
// ============================================================================

/// `metadata:` block shared by every K8s resource.  Namespace is
/// `None` by default — chart users override via `helm install
/// --namespace …` or `kubectl apply -n …`.  Embedding a namespace
/// in the manifest would conflict with that override.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectMeta {
    /// Skipped when empty so pod-template metadata (which doesn't
    /// need a name — the ReplicaSet controller generates pod names
    /// from the Deployment name + a random suffix) emits cleanly.
    /// Top-level resources always set this; the empty-skip just
    /// stops `name: ''` lines appearing in
    /// `spec.template.metadata`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    /// `aislang.generated=true` + `aislang.component=<name>` +
    /// `aislang.deploy=<label>` — built by `crate::labels::
    /// aislang_labels`.  K8s analogue of the Compose label
    /// convention.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,

    /// Annotation slot reserved for Wed's prompt-hash invalidation
    /// trick (Decision 43.4).  Empty here on Tue.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

impl ObjectMeta {
    /// Helper for the common case: a name + labels, no annotations,
    /// no namespace.  Most Tue resources use this shape.
    pub fn new(name: impl Into<String>, labels: BTreeMap<String, String>) -> Self {
        Self {
            name:        name.into(),
            namespace:   None,
            labels,
            annotations: BTreeMap::new(),
        }
    }
}

/// Selector used by Deployments + Services to match Pods by label.
/// `match_labels` is the simple `=`-only form; `match_expressions`
/// (NotIn, In, Exists) isn't in our v0.2 surface because the
/// emitter only ever produces single-pod selectors.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelector {
    pub match_labels: BTreeMap<String, String>,
}

// ============================================================================
// Deployment
// ============================================================================

#[derive(Serialize)]
pub struct Deployment {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind:        String,
    pub metadata:    ObjectMeta,
    pub spec:        DeploymentSpec,
}

impl Deployment {
    pub fn new(metadata: ObjectMeta, spec: DeploymentSpec) -> Self {
        Self {
            api_version: "apps/v1".to_string(),
            kind:        "Deployment".to_string(),
            metadata,
            spec,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentSpec {
    /// Defaults to 1 in K8s itself when absent, but we emit it
    /// explicitly so `helm template --debug` output is
    /// self-describing.  Thu lets `values.yaml` override per-pod
    /// (Decision 43.7).
    pub replicas: u32,
    pub selector: LabelSelector,
    pub template: PodTemplate,
}

#[derive(Serialize)]
pub struct PodTemplate {
    pub metadata: ObjectMeta,
    pub spec:     PodSpec,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PodSpec {
    pub containers: Vec<Container>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub volumes: Option<Vec<Volume>>,

    /// Init containers — used Thu for the
    /// `wget --spider http://litellm:4000/health/liveliness` block
    /// that gates the agent-runtime pod's startup.  Empty on Tue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_containers: Option<Vec<Container>>,
}

// ============================================================================
// Container
// ============================================================================

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    pub name:  String,
    pub image: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub ports: Option<Vec<ContainerPort>>,

    /// Per-var name/value pairs.  Static values only (literal
    /// strings); `valueFrom: { secretKeyRef: … }` lives on
    /// [`EnvFromSource`] below, which is the bulk path we use
    /// for provider creds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<EnvVar>>,

    /// Bulk env-injection from a Secret or ConfigMap.  Decision
    /// 43.5 wires `envFrom: [{ secretRef: { name: aislang-
    /// providers } }]` on LiteLLM + the agent-runtime pod so
    /// every key in the user-created Secret becomes an env var
    /// at pod startup.  No enumeration of expected vars in the
    /// manifest — the chart docs which vars the Secret must
    /// contain (driven by `litellm::referenced_env_vars`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_from: Option<Vec<EnvFromSource>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,

    /// Mounted volumes — used Tue for LiteLLM's `/app/config.yaml`
    /// (from the litellm-config ConfigMap), Wed for the
    /// agent-runtime pod's `/etc/aislang/` mount.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_mounts: Option<Vec<VolumeMount>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerPort {
    pub container_port: u16,

    /// Named ports let the Service's `target_port` reference by
    /// name instead of number — clearer in manifests and
    /// resilient to per-image port changes.  We name every port
    /// we emit.
    pub name: String,

    /// Defaults to TCP in K8s; emit explicitly when set to keep
    /// the wire format readable.  None ⇒ TCP.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

#[derive(Serialize)]
pub struct EnvVar {
    pub name:  String,
    pub value: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvFromSource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<SecretEnvSource>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_map_ref: Option<ConfigMapEnvSource>,
}

#[derive(Serialize)]
pub struct SecretEnvSource {
    pub name: String,

    /// `true` = pod starts even if the Secret doesn't exist (vars
    /// silently absent).  We set `false` (the K8s default) — the
    /// chart docs make Secret creation a prerequisite, and
    /// silent absence would cause LiteLLM to dial Anthropic with
    /// empty creds.  Loud failure (ImagePullBackOff-style) is
    /// the right UX.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional: Option<bool>,
}

#[derive(Serialize)]
pub struct ConfigMapEnvSource {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeMount {
    pub name:       String,
    pub mount_path: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,

    /// Mount a single key from a multi-key ConfigMap as a file at
    /// `mount_path`.  Used Wed: each prompt body is a separate
    /// ConfigMap key, mounted as `/etc/aislang/prompts/<label>.md`.
    /// Tue: LiteLLM mounts the whole `litellm-config` CM as
    /// `/app/config.yaml` via the ConfigMap items[]+key mechanism
    /// (see [`ConfigMapVolumeSource`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_path: Option<String>,
}

// ============================================================================
// Volume
// ============================================================================

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_map: Option<ConfigMapVolumeSource>,

    /// PersistentVolumeClaim source — Thu's Open WebUI shape uses
    /// this for `/app/backend/data` persistence.  None on Tue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistent_volume_claim: Option<PersistentVolumeClaimVolumeSource>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigMapVolumeSource {
    pub name: String,

    /// Selected keys → file-path mappings.  `None` = mount EVERY
    /// key in the CM as a file named after the key.  We use the
    /// items[] shape for the LiteLLM mount: one key → `config.yaml`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<KeyToPath>>,
}

#[derive(Serialize)]
pub struct KeyToPath {
    pub key:  String,
    pub path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistentVolumeClaimVolumeSource {
    pub claim_name: String,
}

// ============================================================================
// Service
// ============================================================================

#[derive(Serialize)]
pub struct Service {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind:        String,
    pub metadata:    ObjectMeta,
    pub spec:        ServiceSpec,
}

impl Service {
    pub fn new(metadata: ObjectMeta, spec: ServiceSpec) -> Self {
        Self {
            api_version: "v1".to_string(),
            kind:        "Service".to_string(),
            metadata,
            spec,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSpec {
    /// `selector` matches the Deployment's pod template labels.
    /// We use the same `aislang.component=<name>` label the
    /// Deployment sets — one component, one Service.
    pub selector: BTreeMap<String, String>,

    pub ports: Vec<ServicePort>,

    /// Defaults to `ClusterIP` (internal-only); set to `NodePort`
    /// or `LoadBalancer` for external access.  Tue: all internal.
    /// Thu may emit `NodePort` for Open WebUI when `expose.ingress`
    /// isn't set (lets `kubectl port-forward` users hit it).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServicePort {
    /// External port the Service exposes.
    pub port: u16,

    /// Container-side port — references a [`ContainerPort::name`]
    /// to stay resilient to port-number changes inside the image.
    pub target_port: String,

    /// Service-port name — required when a Service exposes >1
    /// port (which Jaeger does: UI + OTLP gRPC + OTLP HTTP).
    /// Always emit for parsability.
    pub name: String,

    /// Defaults to TCP.  Same posture as ContainerPort.protocol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

// ============================================================================
// ConfigMap
// ============================================================================

#[derive(Serialize)]
pub struct ConfigMap {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind:        String,
    pub metadata:    ObjectMeta,

    /// Key → file-body map.  We use BTreeMap so the YAML output
    /// is byte-deterministic across plan runs.  Tue: one CM
    /// (`litellm-config`) with one key (`config.yaml`); Wed adds
    /// the `aislang-project` CM with the .ais source +
    /// lockfile + per-prompt entries.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, String>,
}

impl ConfigMap {
    pub fn new(metadata: ObjectMeta, data: BTreeMap<String, String>) -> Self {
        Self {
            api_version: "v1".to_string(),
            kind:        "ConfigMap".to_string(),
            metadata,
            data,
        }
    }
}

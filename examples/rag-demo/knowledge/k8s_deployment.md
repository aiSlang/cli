# Kubernetes deployment (v0.2 Phase 2)

`deploy { target = kubernetes }` emits a Helm chart + raw
manifests + 5 pods: LiteLLM (router), Jaeger (tracing),
budget-counter (per-day cap), agent-runtime (mcp-serve --http),
and Open WebUI (optional chat UI when `web_ui = true`).

The chart works on both kind + k3s — CI smoke tests both axes
on every main-push.  StorageClass parity is preserved by
leaving `openwebui-data.spec.storageClassName` unset (cluster
default applies: `standard` on kind, `local-path` on k3s).

`aislang apply <project>` runs `helm upgrade --install`; `aislang
destroy <project>` runs `helm uninstall` + waits for full pod
termination before asserting clean.

Production image: `ghcr.io/aislang/aislang:<tag>` built on
`gcr.io/distroless/cc-debian13` (glibc 2.41 — covers symbols the
ubuntu-24.04 GitHub runner links against).

# aiSlang

> **Ship multi-agent systems, not glue code.** Your agent works in a notebook — but turning it into a deployed, *multi-agent* system (routing, isolation, budgets, observability) is weeks of Kubernetes-and-LiteLLM plumbing. aiSlang describes the whole system in one declarative file and runs it — locally (incl. against local LLMs) or on managed Kubernetes via [aiSlang.cloud](https://docs.aislang.cloud).

Agent frameworks make it easy to demo *one* agent. Real systems compose many — a `router` picks a specialist, an `orchestrator` delegates, a `reflector` critiques and retries — and need model fallback, spend caps, tracing, and a way to actually serve them. aiSlang makes **the system** the unit: declare the agents and how they combine, then `aislang apply` ships it.

**Status:** **v0.3.0-rc1** (Week 20 Fri close — DSL extensions + dogfooded agent inventory shipped: Phase 1 added composition primitives `pipeline` / `loop` / `branch` + `skill` block + MiniJinja templates + multi-module projects via `aislang.toml`; Phase 2 added `vector_store` + `embedding` blocks + `agent.knowledge` field + multi-module orchestrator dispatch + 6 dogfooded agents under `examples/agents/` — Architect / Coder / Code-reviewer / Tester / DB / Orchestrator, all with per-agent RAG corpora; Qwen3-8B + Qwen2.5-Coder-7B Ollama catalog entries land local-LLM-first integration; Haiku stays as the practical primary while CPU inference is the user-side reality.  Internal tag only — public-launch DoD remains v0.4 once Track C scaffolds (managed cloud, GitOps, multi-tenancy, billing) land per the strategic pivot from v0.2 close.  W20 Thu dogfood findings ([orchestrator/DOGFOOD-FINDINGS.md](examples/agents/orchestrator/DOGFOOD-FINDINGS.md)) record four v0.5+ Coder fine-tuning carry-overs.  v0.4 (active) targets the FIRST PUBLIC RELEASE — Track C managed cloud (`aislang.cloud`) + Stripe-metered billing + GitOps PR-plan-comment workflow + multi-tenant Postgres schema-per-tenant isolation + SOC2 scaffold; 5-week budget Weeks 21-25 per Decisions 46.1-46.8.  See [PLAN-v0_4.md](PLAN-v0_4.md) for the v0.4 plan (ACTIVE), [PLAN-v0_3.md](PLAN-v0_3.md) for the v0.3 plan + Phase 2 close totals + v0.3 retrospective, [PLAN-90-days-v0_2.md](PLAN-90-days-v0_2.md) for the v0.2 archive, [PLAN-90-days-v0_1.md](PLAN-90-days-v0_1.md) for the v0.1 archive, and [LANGUAGE-SPEC-v0.md](LANGUAGE-SPEC-v0.md) for the language reference.

## Install

```sh
# Public repo (post-v0.4 launch).  Detects OS+arch, fetches the
# matching tarball from GitHub Releases, verifies sha256, installs
# the `aislang` binary into $HOME/.local/bin.
curl -sSfL https://github.com/aislang/aislang/releases/latest/download/install.sh | sh
```

```sh
# Private repo / fork (current v0.2/v0.3 state — repo not yet public).
# Create a Classic PAT with `repo` scope OR a fine-grained PAT with
# read-access to the single repo: https://github.com/settings/tokens
GITHUB_TOKEN=ghp_XXXX REPO=aiSlang/cli \
  curl -sSfL -H "Authorization: Bearer $GITHUB_TOKEN" \
    "https://raw.githubusercontent.com/${REPO}/main/install.sh" \
  | GITHUB_TOKEN=$GITHUB_TOKEN REPO=$REPO sh
```

`GITHUB_TOKEN` (or `GH_TOKEN`) is honoured for all release-asset downloads.  `sh install.sh --check-deps` probes for runtime prerequisites (docker / docker compose / helm / kubectl / kind) and prints per-distro install hints without running the binary install.  Full install.sh option surface: see the script header.

## Quick taste

```hcl
project "support-agent" {
  budget {
    hard_cap_per_day = $50
    hard_cap_per_run = $0.50
    optimize_for     = cost     # v0.2 Tue follow-up: informational hint;
                                # resolver walks `prefer` in declared order
                                # — see LANGUAGE-SPEC §6
  }

  # The strategic / orchestration model — declared with `source =
  # subscription` so its tokens go through the user's Claude Pro or
  # Cursor Team subscription, NOT the API.  Cost-saving angle of the
  # v0.2 commercial moat: the expensive top-of-funnel reasoning lives
  # on a subscription tier; only deployed agents hit the per-token API.
  model "thinker" {
    requires    = [chat]
    context_min = 200k
    prefer      = [anthropic.claude_sonnet_4_6]
    source      = subscription          # v0.2 Week 12 Fri
  }

  # The deployed-agent model — fast + cheap + concrete-tool-using.
  # `source = api` (default) → routed through LiteLLM, hard budget cap
  # enforced at runtime via `~/.aislang/spend.json`.
  model "fast" {
    requires    = [chat, tool_use]
    context_min = 32k
    cost_max    = $0.001 per 1k_tokens
    prefer      = [anthropic.claude_haiku_4_5, openai.gpt_4o_mini]
    fallback    = [ollama.llama_3_1_8b]
  }

  # v0.2 Week 12 Fri: `main` block declares the user-facing LLM that
  # ORCHESTRATES the deployed agents (Claude Desktop / Cursor / etc.).
  # `pattern = orchestrator` is one of six closed patterns.  `aislang
  # status --main-spend` (Week 13 Thu) surfaces this entry's spend
  # alongside the deployed-agent cost.
  main "support_orchestrator" {
    model         = model.thinker
    chat_client   = claude_desktop
    pattern       = orchestrator
    system_prompt = prompt.system
  }

  agent "support" {
    model         = model.fast
    system_prompt = prompt.system
    tools         = [mcp_server.docs]
  }

  # v0.2 Week 13 Tue (Track A2): `expose { web_ui = true }` brings up an
  # Open WebUI sidecar at http://localhost:3000 wired to the in-stack
  # LiteLLM — browser chat for users without an MCP-aware client.
  deploy "local" {
    target = compose
    expose {
      port   = 8080      # informational; future agent-runtime port
      web_ui = true      # → http://localhost:3000 after `aislang apply`
    }
  }

  # ...
}
```

See [examples/support-agent.ais](examples/support-agent.ais) for the full reference example.

### Multi-module projects (v0.3 Week 16)

Split a project across multiple `.ais` files when one agent's resources should be reusable by another.  Drop an `aislang.toml` at the project root + reference sibling resources via `use <module>.<label>`:

```toml
# examples/multi-module-demo/aislang.toml
[project]
name    = "multi-module-demo"
version = "0.1.0"

[modules]
architect = "agents/architect.ais"
coder     = "agents/coder.ais"
```

```hcl
# examples/multi-module-demo/agents/architect.ais
project "architect-module" {
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.01
  }
  model "thinker" {                          # exported as `architect.thinker`
    requires    = [chat]
    context_min = 32k
    prefer      = [ollama.llama_3_1_8b]
    pin         = ollama.llama_3_1_8b
  }
  prompt "system" { file = "./system.md"  version = "1.0.0" }
  agent "architect" {
    model         = model.thinker            # same-module ref (v0.2 syntax)
    system_prompt = prompt.system
    tools         = []
  }
}
```

```hcl
# examples/multi-module-demo/agents/coder.ais
project "coder-module" {
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.01
  }
  prompt "system" { file = "./system.md"  version = "1.0.0" }
  agent "coder" {
    model         = use architect.thinker    # v0.3 cross-module ref
    system_prompt = prompt.system
    tools         = []
  }
}
```

**Dir-vs-file dispatch — same CLI surface for both shapes.** Every command that takes a positional `<file.ais>` argument (`validate`, `plan`, `apply`, `destroy`) accepts a project directory holding `aislang.toml` in its place — the CLI autodetects which shape it was handed.  Single-file projects work byte-stably; multi-module projects route through the manifest loader + cross-module checker + multi-file resolver.  The `chat` and `eval` subcommands stay file-only in v0.3 (in-pod `mcp-serve` needs multi-module awareness per Decision 44.1's deferred scope).

```sh
aislang validate examples/multi-module-demo/    # OK: 2 modules
aislang plan     examples/multi-module-demo/    # emits Compose + lockfile
                                                # with module-qualified keys
aislang apply    examples/multi-module-demo/    # bring stack up
aislang destroy  examples/multi-module-demo/    # tear it down
```

The lockfile gains a `modules` section recording per-module SHA-256 source hashes; any module's source change re-resolves the whole project on the next plan.  Full LANGUAGE-SPEC §4.11 documents the manifest + `use` syntax + scope deferrals.

The single-file commands below work identically — substitute a project directory anywhere `examples/support-agent.ais` appears:

```sh
# Stand up the stack (LiteLLM + Jaeger + budget counter) in Docker.
# Same command shape against either a .ais file (v0.2 baseline) or
# a project directory holding aislang.toml (v0.3 multi-module).
aislang apply examples/support-agent.ais

# Chat with the agent — `aislang chat` runs the loop in-CLI:
# the agent talks to the Compose-managed LiteLLM and spawns the
# declared MCP servers (e.g. `mcp/filesystem:1.0.2`) as stdio
# children for the duration of the session.
# Note: in v0.3, `chat` requires the .ais file directly; multi-
# module dir-input chat lands later in v0.3.
aislang chat support examples/support-agent.ais

# Run the declared evals against the deployed agent and print a
# JSON report (per-case scores + trace URLs that deep-link into
# the bundled Jaeger UI at localhost:16686 for each case).
# Same v0.3 carry-over as `chat` — file-only for now.
aislang eval examples/support-agent.ais

# When you're done, tear it all down (containers + volumes).
# Accepts dir input too — `aislang destroy examples/multi-module-demo/`.
aislang destroy examples/support-agent.ais
```

### Browser chat via Open WebUI (v0.2 Tue, Track A2)

Drop `web_ui = true` into your `deploy { expose { … } }` block to bring up an [Open WebUI](https://docs.openwebui.com/) sidecar pointed at the in-stack LiteLLM:

```hcl
deploy "local" {
  target = compose
  expose {
    web_ui = true     # → http://localhost:3000 after `aislang apply`
  }
}
```

After `aislang apply`, browse to **http://localhost:3000**, sign up (local SQLite — first user is admin), and pick any model alias visible in the dropdown.  All models LiteLLM is configured to route appear there — useful for debugging "is this agent's chosen model actually replying?" without writing client code.

`aislang destroy` runs `docker compose down -v` which wipes the `openwebui-data` named volume; chat history + local accounts go away alongside the rest of the stack.

### Deploy to Kubernetes (Helm) — v0.2 Phase 2 Week 14 (Track B)

Flip `target = compose` to `target = kubernetes` and `aislang apply` packages the same stack as a Helm chart and installs it via `helm upgrade --install`.  All five always-on infrastructure pods (LiteLLM router + Jaeger tracing + budget counter + `aislang mcp-serve` agent runtime + the `aislang-project` ConfigMap holding your .ais source + lockfile + prompts) plus the opt-in Open WebUI Deployment+Service+PVC.  Same labels (`aislang.generated=true` + `aislang.component=<name>` + `aislang.deploy=<label>`) so `kubectl get all -l aislang.generated=true` is the K8s analogue of `docker compose ls --filter "label=aislang.generated"`.

```hcl
deploy "prod" {
  target = kubernetes
  expose {
    port    = 8080                       # agent-runtime mcp-serve --http port
    web_ui  = true                       # → Open WebUI Deployment+Service+PVC
    ingress = "support.example.com"      # opt-in Ingress (omit for kubectl port-forward)
  }
}
```

#### `kind` quick-start on Ubuntu

For local testing, **[kind](https://kind.sigs.k8s.io/)** (Kubernetes in Docker) gives you a working cluster in ~30 seconds.  It needs Docker (already required by `target = compose`) plus the `kind` + `kubectl` + `helm` CLIs:

```sh
# 1. Docker — required prerequisite (same as target = compose)
sudo apt update && sudo apt install -y docker.io
sudo usermod -aG docker $USER && newgrp docker

# 2. kubectl — the K8s CLI
curl -sSL -o /tmp/kubectl "https://dl.k8s.io/release/$(curl -sSL https://dl.k8s.io/release/stable.txt)/bin/linux/amd64/kubectl"
sudo install -m 0755 /tmp/kubectl /usr/local/bin/kubectl

# 3. helm — chart installer
curl -sSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash

# 4. kind — local cluster
curl -sSL -o /tmp/kind https://kind.sigs.k8s.io/dl/v0.24.0/kind-linux-amd64
sudo install -m 0755 /tmp/kind /usr/local/bin/kind

# 5. Spin up a cluster (takes ~30s; default name `kind`)
kind create cluster

# 6. Sanity-check that kubectl talks to it
kubectl get nodes
# NAME                 STATUS   ROLES           AGE   VERSION
# kind-control-plane   Ready    control-plane   30s   v1.31.0
```

On macOS, swap `apt install` for `brew install docker kubectl helm kind` and ensure Docker Desktop is running.  See `install.sh --check-deps` (added Thu) for an automated probe of your local toolchain.

#### Apply, port-forward, destroy

```sh
# 1. Create the provider-creds Secret (Decision 43.5).  aislang doesn't
#    auto-create this — Secret bodies should never land in the chart
#    values.yaml.  Populate from your .env or paste keys directly.
kubectl create secret generic aislang-providers \
  --from-literal=ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" \
  --from-literal=OPENAI_API_KEY="$OPENAI_API_KEY" \
  --from-literal=OLLAMA_API_BASE="$OLLAMA_API_BASE"

# 2. Plan — writes manifests/ + chart/ but doesn't install yet.
aislang plan examples/support-agent-k8s.ais

# 3. Apply — `helm upgrade --install <deploy-label> chart/`.
aislang apply examples/support-agent-k8s.ais

# 4. Open WebUI is on a ClusterIP Service by default.  Either:
#    - port-forward (works on every cluster, no Ingress controller needed):
kubectl port-forward svc/openwebui 3000:8080
#    - or set `ingress = "<host>"` in your .ais (Decision 43.6) and point
#      DNS at your cluster's Ingress controller LB.

# 5. `aislang chat` from your laptop — needs LiteLLM reachable on
#    localhost:4000 (the CLI dials a fixed address; v0.3 makes it
#    topology-aware).  Port-forward LiteLLM in a second terminal:
kubectl port-forward svc/litellm 4000:4000
#    Then chat as normal:
aislang chat support examples/support-agent-k8s.ais

# 6. Tear it down — `helm uninstall <deploy-label>`.
aislang destroy examples/support-agent-k8s.ais
```

> **Why the port-forward**: under `target = compose`, LiteLLM publishes `4000:4000` to the host, so `aislang chat`'s local mcp-serve dials `http://127.0.0.1:4000` and it just works.  Under `target = kubernetes`, LiteLLM is a `ClusterIP` Service — reachable only from inside the cluster.  Until v0.3 teaches `aislang chat` to discover the K8s Service (via the lockfile or a topology field), the workaround is the one-line `kubectl port-forward svc/litellm 4000:4000`.  Open WebUI chat (browser via the port-forwarded Service or the Ingress) is unaffected — it dials LiteLLM by K8s DNS name from inside the cluster.

The chart's `values.yaml` exposes exactly four overrides (Decision 43.7 — minimal templating).  Override at install time without re-running `aislang plan`:

```sh
helm upgrade --install prod ./chart \
  --set image.tag=v0.2.0-rc2 \
  --set litellm.replicas=3 \
  --set openwebui.persistence.size=5Gi \
  --set ingress.host=support.example.com
```

#### What's pinned, what's user-controlled

| Concern | aiSlang emits | User overrides |
|---|---|---|
| Container images | `ghcr.io/aislang/aislang`, `ghcr.io/berriai/litellm:main-stable`, `jaegertracing/all-in-one:latest`, `ghcr.io/open-webui/open-webui:main` | `image.tag` (agent-runtime only via chart `values.yaml`) |
| Provider credentials | `envFrom: secretRef: aislang-providers` (Decision 43.5) | `kubectl create secret generic aislang-providers --from-literal=…` |
| Storage | `PersistentVolumeClaim` for Open WebUI (1Gi default, cluster default StorageClass) | `openwebui.persistence.size` |
| Ingress | Optional `Ingress` routing `/` to openwebui (no TLS in v0.2) | `expose.ingress = "<host>"` in .ais; `ingress.host` in chart values |
| Replicas | LiteLLM = 1 (templated), others fixed at 1 (stateful) | `litellm.replicas` |
| Namespace | None pinned — uses Helm's `--namespace` flag or default | `helm install --namespace <ns>` |

See `examples/support-agent-k8s.ais` for the canonical K8s demo and PLAN-90-days-v0_2.md §"Phase 2 — Track B foundation" for the eight locked architectural decisions (43.1–43.8) that drive the resource shapes.

**Deploying to managed K8s clouds (GKE / EKS / AKS / OKE)**: the chart works against managed-cloud K8s with three caveats that the local `kind` walkthrough above doesn't surface — Ingress class annotations, GHCR private-image pull, and (on GKE Autopilot) pod resource-request enforcement.  See [`docs/gke-rehearsal-procedure.md`](docs/gke-rehearsal-procedure.md) for the GKE-specific cookbook + the per-step gotcha list + the v0.3 chart-improvement carry-overs each gap maps to.  The procedure generalises to other managed-K8s providers — most of the gaps are managed-cloud-shaped rather than GKE-specific.

> **Heads-up on the agent-runtime image (pre-v0.2.0-rc2)**: the `agent-runtime` Deployment references `ghcr.io/aislang/aislang:latest`, which is published by the release workflow's `image` job (Mon's Phase 2 Week 14 commit).  Until v0.2.0-rc2 is tagged (Fri's Phase 2 Week 14 close), that image **doesn't exist in ghcr.io** — the agent-runtime pod will sit in `ImagePullBackOff`.  The rest of the stack (LiteLLM + Jaeger + budget-counter + Open WebUI) uses upstream public images and runs fine.  For early testing on a pre-rc2 build: either (a) chat through Open WebUI in the browser (it dials LiteLLM directly, no agent-runtime needed); (b) build + push `aislang:dev` to a registry you control and override with `helm install --set image.repository=… --set image.tag=dev`; or (c) `kind load docker-image aislang:dev` to side-load a locally-built image into kind without pushing.

> **Heads-up on `ollama.*` models under K8s**: under `target = compose`, LiteLLM dials the user's host-side Ollama via `OLLAMA_API_BASE=http://host.docker.internal:11434` + an `extra_hosts: host.docker.internal:host-gateway` mapping (Linux needs the explicit mapping; Mac/Windows wire it via Docker Desktop).  K8s has no equivalent — `host.docker.internal` isn't resolvable inside pods on most clusters.  Symptom: LiteLLM fallback to `ollama.*` fails with `OllamaException - Cannot connect to host localhost:11434`.  For Thu's K8s demo, recommended workarounds: (i) drop `ollama.*` from `prefer`/`fallback` when running `target = kubernetes` (the canonical demo uses `anthropic.claude_haiku_4_5` as primary + `openai.gpt_4o_mini` as fallback); (ii) deploy Ollama into the cluster too as a sibling Deployment; (iii) on kind specifically, configure `extraPortMappings` so `host.docker.internal` resolves inside the cluster.  v0.3 may emit an Ollama Deployment automatically when the chart detects an `ollama.*` model id — see PLAN §"Other v0.3 carry-overs".

### MCP integration — drive aiSlang from Claude Desktop / Cursor (v0.2 Thu, Track A)

`aislang mcp-serve` exposes the whole CLI surface as an MCP server, so MCP-aware LLM hosts can author, deploy, and chat with .ais agents without you touching a terminal.

**Claude Desktop one-time setup** — add to `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or the equivalent on Linux/Windows:

```json
{
  "mcpServers": {
    "aislang": {
      "command": "aislang",
      "args": ["mcp-serve", "--stdio"]
    }
  }
}
```

Restart Claude Desktop. The aislang tools appear under the 🔌 icon. Try:

- *"Validate /path/to/my.ais"* → calls `aislang_validate`, shows source-aware diagnostics.
- *"Apply the support-agent stack at examples/support-agent.ais"* → calls `aislang_apply` (Claude Desktop renders the destructive-hint confirmation dialog before bringing up the Compose stack).
- *"Run a chat turn against the support agent — ask it: what's the return policy?"* → calls `aislang_chat_turn` → progress notifications (week 13 Wed) render as the tool-use iterations unfold → final reply lands as the tool's response.

This is the **principal-pattern demo**: Claude Desktop is the orchestrator (`pattern = orchestrator`), the deployed agent (`agent "support"`) is one of its tools, mcp-serve glues them. The `main` block in your .ais source declares this topology explicitly (week 12 Fri); `aislang status --main-spend` (week 13 Thu) surfaces the cost-split between the host LLM's tokens and the deployed agents' tokens.

For HTTP-MCP hosts (k8s deployments, ChatGPT-with-MCP, custom rmcp clients): `aislang mcp-serve --http 0.0.0.0:8080` mounts the same toolset at `http://host:8080/mcp` (week 13 Mon).

### Observability

Every `aislang chat` and `aislang eval` invocation emits OpenTelemetry traces over OTLP/HTTP to the bundled Jaeger sidecar (`localhost:16686`). The trace tree mirrors the chat loop's structure — each user turn becomes one `aislang.turn` span, the tool-use loop fans out into `tool_use_iteration[N]` spans, and each iteration carries one `chat <model>` LLM call + zero-or-more `mcp <server>.<tool>` dispatch spans. Attributes follow the [OTel-GenAI semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/) (`gen_ai.system`, `gen_ai.request.model`, `gen_ai.usage.input_tokens`, …) plus our own `mcp.*` namespace for tool calls (sizes only, never argument content — PII-safe by construction).

After each command, the trace URL prints to stderr — paste it into a browser to see the full call hierarchy:

```
trace (eval `factual_accuracy`): http://localhost:16686/trace/7ca996386817d4859e6921c26245e4e3
```

`aislang eval`'s JSON report carries the same URLs as machine-readable fields so CI artifacts can link straight into Jaeger:

```sh
aislang eval examples/support-agent.ais | jq -r '.trace_url, .cases[].trace_url'
```

Override the Jaeger UI URL with `AISLANG_JAEGER_UI` (e.g. for a shared collector) or the OTLP endpoint with the standard `OTEL_EXPORTER_OTLP_ENDPOINT` env var. v0.1 uses 100 % head sampling — eval volumes are small enough that the debuggability win pays for itself.

#### Observability under K8s (Phase 2 Week 15 Tue)

The same OTel-GenAI spans flow into the in-cluster Jaeger pod automatically — no manual env config required.  Both the LiteLLM router pod and the `aislang mcp-serve` agent-runtime pod ship spans to `http://jaeger:4318` (K8s DNS — same posture LiteLLM uses to expose its own router endpoint).  Per-pod `OTEL_SERVICE_NAME` follows the pattern `aislang.<component>.<deploy-label>`:

| Component | `service.name` (example for `deploy "prod"`) |
|---|---|
| LiteLLM router | `aislang.litellm.prod` |
| Agent runtime  | `aislang.agent_runtime.prod` |
| Jaeger         | (receiver — doesn't produce its own spans in our topology) |

The `<deploy-label>` suffix lets a single Jaeger instance serve multi-tenant clusters cleanly — `aislang.litellm.tenant-a` and `aislang.litellm.tenant-b` show up as distinct rows in the Jaeger UI's service dropdown.

To view traces from a kind cluster, port-forward Jaeger's UI:

```sh
kubectl port-forward svc/jaeger 16686:16686
# Then browse http://localhost:16686.
```

After running `aislang chat support examples/support-agent-k8s.ais` against the cluster, the Jaeger UI's "Service" dropdown should show:

| `service.name`              | Produced by                                  |
|-----------------------------|----------------------------------------------|
| `aislang`                   | The **local** `aislang chat` process (laptop) |
| `aislang.litellm.prod`      | The **in-cluster** LiteLLM router pod         |
| `aislang.agent_runtime.<deploy>` | The **in-cluster** mcp-serve pod         |
| `jaeger-all-in-one`         | Jaeger's own self-instrumentation             |

If you only see `jaeger-all-in-one` after a successful chat turn, two common gaps:

1. **No `kubectl port-forward svc/jaeger 4318:4318`** — local `aislang chat` ships spans to `http://localhost:4318/v1/traces` by default; under Compose that lands in the bundled jaeger sidecar, under K8s nothing's listening on host port 4318 unless you forward it.  Add a second port-forward alongside the `svc/litellm 4000:4000` one chat already needs:
   ```sh
   kubectl port-forward svc/jaeger  4318:4318  &   # for local chat spans
   kubectl port-forward svc/jaeger 16686:16686 &   # for UI
   kubectl port-forward svc/litellm 4000:4000 &   # for chat itself
   ```
   Then re-run the chat — `service.name = aislang` should appear within seconds (simple-exporter ships per-span synchronously).
2. **agent-runtime pod stuck in `ImagePullBackOff`** — see the "Heads-up on the agent-runtime image" callout above.  When the pod can't pull its image, it never produces spans.  Override `image.repository` to your fork's published image (e.g. `--set image.repository=ghcr.io/<your-org>/aislang --set image.tag=v0.2.0-rc2`) or side-load via `kind load docker-image aislang:dev`.

**Use an external collector** (Honeycomb, Grafana Tempo, Datadog, NewRelic, OTel Collector) instead of the bundled in-cluster Jaeger:

```sh
helm upgrade --install prod ./chart \
  --set observability.otlp_endpoint=https://api.honeycomb.io:443
```

The override flows into both LiteLLM + agent-runtime pods via the chart's `observability.otlp_endpoint` value (5th templating point — see `chart/values.yaml`).  Cross-cluster TLS / auth headers (`OTEL_EXPORTER_OTLP_HEADERS=…`) aren't covered by the chart in v0.2 — patch the Deployments with a kustomize overlay if you need them; v0.3 may surface them as another values.yaml field.

## Commands

`aislang` exposes eight subcommands. Four are read-only / offline (`validate`, `plan`, `cost`, `status`); four shell out to `docker compose` and the running LiteLLM router (`apply`, `destroy`, `chat`, `eval`). Run `aislang <cmd> --help` for the full flag list — the table is a quick reference, the flowcharts below show each command's decision tree.

| Command | Synopsis | What it does |
|---|---|---|
| `validate` | `aislang validate [-v…] <file>` | Parse, type-check, and catalog-id-check the .ais source. No network, no writes. |
| `plan`     | `aislang plan [-v…] [--no-lock] [-o DIR] [--smoke-test] [--smoke-test-live] <file>` | Resolve models, reconcile `aislang.lock`, emit artifacts.  `target = compose`: Compose YAML on stdout + `litellm_config.yaml` + `.env.example`.  `target = kubernetes` (Phase 2): K8s manifests on stdout + `manifests/*.yaml` + Helm `chart/` on disk. |
| `apply`    | `aislang apply <file>` | Compose: write `stack.yml`, then `docker compose up -d` + record `.aislang/state.json`.  Kubernetes: write `manifests/` + `chart/`, then `helm upgrade --install <deploy-label> chart/`.  Both paths graceful-degrade when their backend CLI is missing. |
| `destroy`  | `aislang destroy [--force] <file>` | Compose: `docker compose down -v`.  Kubernetes: `helm uninstall <deploy-label>`.  Both idempotent — re-runs on a torn-down stack exit 0. |
| `chat`     | `aislang chat <agent> <file>` | Open an interactive REPL against the deployed LiteLLM, spawning declared MCP servers as stdio children for the session. Tool-use loop hard-capped at 8 iterations. |
| `eval`     | `aislang eval [-e <label>] <file>` | Run every `eval` block (or one named via `-e`) against the deployed agent. JSON `EvalReport` on stdout; per-case `trace_url`s deep-link into Jaeger. Exit 1 if any eval fails threshold. |
| `cost`     | `aislang cost <file>` | Print today's UTC spend (total + per-model) from `~/.aislang/spend.json` against the budget caps in the source. Read-only. |
| `status`   | `aislang status [--spend] [--model-spend[=MODEL]] <file>` | Render stack health (state file + `docker compose ps`), optionally with spend sections. Exit `0/1/2` for clean / drift / docker-unavailable. |

### Flowcharts

Per-command decision trees, in the tree style used elsewhere in the docs for `destroy`'s idempotency design.

#### `validate`

```
read source
└── aislang_core::validate                          (lex → parse → typecheck)
    └── aislang_resolver::check_against_bundled_catalog   (verify catalog IDs)
        ├── error  → render source-aware snippet → exit 1
        └── ok     → "OK: <file> parsed and type-checked"   (exit 0)
                     ├── -v    resource counts + stage timings (stderr)
                     ├── -vv   AST Debug + resource-graph Debug
                     └── -vvv  full AST + graph as JSON IR
```

#### `plan`

```
validate → catalog check → resolve → lockfile reconcile → emit
                                       ├── Match  → reuse cached bindings
                                       ├── Fresh  → write aislang.lock
                                       └── Stale  → rewrite (-v shows reasons)
                                                    (skipped under --no-lock)
   stdout: docker-compose YAML
   stderr: paths of <out>/litellm_config.yaml + .env.example + aislang.lock,
           warnings, optional verbose tail (-v stage timings;
           -vv ResolvedProject Debug; -vvv combined JSON IR)

   --smoke-test       → pipe YAML through `docker compose -f - config -q`
                        (docker missing → exit 0 "skipped")
   --smoke-test-live  → docker compose up -d
                        → poll :4000 readiness
                        → real /v1/chat/completions vs. first resolved model
                        → docker compose down -v   (ALWAYS, even on failure)
```

#### `apply`

```
plan internals  (validate → resolve → emit)
   └── write <source_dir>/stack.yml
       └── docker on PATH?
           ├── no   → exit 0, print manual "docker compose -f stack.yml up -d" hint
           └── yes  → docker compose -f stack.yml up -d
                       └── write .aislang/state.json
                              (applied_at = now, source_hash, catalog_version)
                           └── print "stack is up" + next-step `chat` / `destroy` hints
```

#### `destroy`

```
read .aislang/state.json
├── applied_at == None    → exit 0 "already torn down"      [no docker]
├── absent + no stack.yml → exit 0 "nothing to destroy"     [no docker]
├── absent + stack.yml present  (v0.0.x back-compat)  → proceed
├── applied_at == Some    → proceed
│       └── docker compose -f stack.yml down -v
│           └── mark_torn_down() → applied_at = None
└── --force               → skip everything above, just proceed
```

#### `chat`

```
validate → find agent  (Levenshtein "did you mean…?" on miss)
   └── resolve model + system_prompt
       └── verify prompt hash against aislang.lock  (when present)
           └── spawn MCP stdio children — one per agent.tools[mcp_server.<X>]
               └── list_tools per server → ToolRegistry
                   └── open Session → LiteLLM @ localhost:4000
                       └── stdin loop:
                           read line → turn → assistant reply
                                       └── tool-use loop, hard cap = 8 iters
                                                          (Decision 4)
                           /exit or EOF → shutdown MCP children, flush trace,
                                          print Jaeger URL on stderr
```

#### `eval`

```
validate → filter to requested eval(s)
              ├── -e <label>  → that one
              └── (no flag)   → every `eval` block
                  │
                  ▼ per eval (RootSpan = aislang.eval):
              resolve agent + prompt + budget + MCP servers
                  └── open Session
                      └── for each case in dataset (JSONL):
                          open `aislang.case` span → run turn
                              → score with metric (exact_match | f1)
                          aggregate (mean) vs. threshold
                              ├── ≥ → eval pass
                              └── < → eval fail
   stdout: EvalReport JSON  (cases[], trace_url, per-case trace_urls)
   stderr: per-case progress + PASS/FAIL summary
   exit:   0 iff every eval passed its threshold   (Decision 16)
```

#### `cost`

```
validate → open ~/.aislang/spend.json   (read-only)
   └── today's UTC total + by_model breakdown
       └── compare to source's `budget { hard_cap_per_run, hard_cap_per_day }`
           └── stdout: spend report + day-cap utilization %
                       (no network, no docker)
```

#### `status`

```
validate → read .aislang/state.json + check stack.yml
   ├── state missing            → "not applied"   (drift, exit 1)
   ├── applied_at == None       → "torn down"     (drift, exit 1)
   └── applied_at == Some
         └── docker on PATH?
             ├── no  → "(cannot query live status)"   (exit 2)
             └── yes → docker compose -f stack.yml ps --format json
                       ├── any service ≠ running     (drift, exit 1)
                       └── all running               (clean, exit 0)

   --spend                → append today's total + cap utilization %
   --model-spend[=MODEL]  → append today's by-model breakdown
                            (=MODEL filters to one; bare flag shows all)
```

## How it works — the pipeline

aiSlang is a *compile-then-deploy* tool, not an interpreter. A single `.ais` source file describes the whole AI infrastructure — models, agents, prompts, MCP servers, evals, budgets, deploy target. The CLI compiles it through four stages, then hands the resulting standard-format artifacts to your platform of choice.

```
        ┌──────────────────┐
        │   .ais source    │   one file = one project
        └────────┬─────────┘
                 │
                 ▼   parse  +  type-check  (validate)
        ┌──────────────────┐
        │   Project AST    │   typed tree (JSON-serializable —
        │   (typed tree)   │    see examples/*.expected-ast.json)
        └────────┬─────────┘
                 │
                 ▼   resolve  (plan)
   ┌─────────────┴─────────────┐         consult ──┐
   │                           │                   ▼
   │   • model selector picks one ModelId from the catalog
   │     per `model` block, subject to declared constraints
   │   • prompt files hashed; OCI image tags pinned to digests
   │   • result written/read from aislang.lock for reproducibility
   │                                              ┌──────────────┐
   └──────────────┬───────────────────────────────│ model catalog│
                  │                               │ + aislang.lock│
                  ▼                               └──────────────┘
        ┌──────────────────┐
        │   Resolved AST   │   the actual IR: every reference now
        │   (the real IR)  │   points to a concrete identity
        └────────┬─────────┘
                 │
                 ▼   emit  (plan output / apply input)
        ┌──────────────────┐
        │ Target artifacts │   v0.1: docker-compose.yml +
        │ (standard YAML / │          LiteLLM config + MCP
        │  manifests /     │          server configs + Jaeger +
        │  charts)         │          budget-enforcement sidecar
        └────────┬─────────┘   v0.2+: K8s manifests, Helm charts,
                 │             ECS task defs, ArgoCD applications,
                 │             Databricks bundles, MS Fabric, Spark
                 ▼   apply
        ┌──────────────────┐
        │  Running stack   │   agents addressable, OTel-GenAI traces
        │  on the chosen   │   emitted, evals runnable, cost caps
        │  platform        │   enforced fail-closed
        └──────────────────┘
```

### Stage-by-stage

| Stage | CLI command | What happens | Where it lives |
|---|---|---|---|
| 1. **Parse + type-check** | `aislang validate` | Lex source, build AST per LANGUAGE-SPEC §8 grammar, validate types and resource-shape semantics from §8.1 | `aislang-core::parser`, `aislang-core::ast` |
| 2. **Resolve** | `aislang plan` | For each `model` block: walk constraints (`requires`, `cost_max`, `latency_p95`, `residency`, `prefer`, `fallback`) against the bundled model catalog; pick the best satisfying entry. Hash prompt files. Pin image tags to digests. Reconcile with `aislang.lock`. Output the resolved IR | `aislang-core::resolver` (week 4) + `aislang-catalog` |
| 3. **Emit** | `aislang plan` / `aislang apply` | Translate the resolved IR to standard target artifacts. v0.1 emits a `stack.yml` (Compose) with LiteLLM as the central router, Jaeger for OTel-GenAI traces, and a budget-enforcement sidecar, plus a `litellm_config.yaml` (mounted into the LiteLLM container) and a `.env.example` (union of provider-derived credentials + env vars referenced by `secret` blocks — see [Secrets handling](#secrets-handling) below). MCP servers are declared in the `.ais` but **spawned as `docker run -i` stdio children by `aislang chat`**, not as long-running Compose services (the published MCP images are stdio-only — see PLAN-90-days-v0_1.md Decision 5; v0.2 changes this) | `aislang-compose` (v0.1); later: `aislang-k8s`, `aislang-helm`, `aislang-databricks`, etc. |
| 4. **Apply / operate** | `aislang apply` / `chat` / `eval` / `cost` / `destroy` | Hand artifacts to Docker / K8s / target platform. The running stack honours the declared budgets, exposes the declared port surface, runs declared evals on demand, and emits OTel traces | shells out to platform CLIs (`docker compose`, `kubectl apply`, etc.) |

### What's in the v0.1 scope vs. phased

- **v0.1** — Docker Compose target only. Catalog bundled in the binary (JSON, immutable per release). Single-file projects. Static prompts with a fixed set of template variables. No LSP yet. See [PLAN-90-days-v0_1.md](PLAN-90-days-v0_1.md) for the 12-week schedule and definition of done.
- **Phase 1** — Kubernetes + Helm targets; LSP via JSON Schema export of the AST; remote catalog updates; multi-file projects via `include`.
- **Phase 2** — Managed cloud (`aislang.cloud`), compliance pack (EU AI Act / ISO 42001 evidence export), ArgoCD / Flux GitOps wrappers, Databricks / MS Fabric / Spark targets, dynamic prompt composition (decorators / guardrails) if user demand pulls for it.

All Phase 1+ items are additive — they don't change the v0.1 grammar or AST. See [LANGUAGE-SPEC-v0.md](LANGUAGE-SPEC-v0.md) §7 for the forward-compatibility invariants that make this possible.

### Secrets handling

aiSlang never reads a real `.env` itself — secrets are the user's responsibility, handed to `docker compose` at runtime. `aislang plan` only emits a `.env.example` template (safe to commit), populated from **two sources merged into one alphabetized list**:

1. **Provider-derived credentials.** For each resolved primary model, a per-provider mapping contributes the env vars LiteLLM expects (e.g. `ANTHROPIC_API_KEY` for an anthropic primary, `OLLAMA_API_BASE` for an ollama primary).
2. **User-declared `secret` blocks.** Each `secret "<label>" { source = "env://VAR" … }` block contributes `VAR` to the same list. Non-`env://` sources (`file://`, future `vault://`) appear in an informational footer comment — `aislang` doesn't load them, but they're documented in one place. The `scope = [...]` field controls which containers receive each secret at compose time.

The two sources are deduplicated: declaring `secret { source = "env://OLLAMA_API_BASE" }` for a project that already resolves to Ollama yields one line, not two. This dual flow is **a v0.1 compromise** — v0.2 unifies it by making `secret` blocks the canonical credential surface (see [LANGUAGE-SPEC-v0.md §5.6](LANGUAGE-SPEC-v0.md)). The forward-compat invariant is that any v0.1 source with no `secret` blocks continues to work in v0.2 against a provider-creds fallback.

## Build

```sh
cargo build --release
./target/release/aislang --help
```

## Layout

```
crates/
  aislang-cli      — `aislang` binary (clap subcommands)
  aislang-core     — parser, AST, types, type-checker, resolver
  aislang-catalog  — model catalog (bundled JSON + Rust types)
  aislang-compose  — Docker Compose emitter
examples/
  support-agent.ais — the v0.1 demo fixture
```

## License

Apache-2.0.

# Deploying stacks

`aislang` is a compile-then-deploy tool: a single `.ais` source describes your
whole system — models, agents, prompts, MCP servers, evals, budgets — and
`aislang apply` turns it into a running stack. The `deploy` block picks the
target. This chapter covers the two self-hosted targets: **Docker Compose**
(the default) and **Kubernetes (Helm)**.

## Docker Compose (default)

With `target = compose`, `aislang apply` emits a `stack.yml` and brings it up
with `docker compose up -d`. The stack is a small, fixed set of services:

- **A LiteLLM router** — the central model gateway. Every agent's API calls go
  through it, which is where model routing, fallback, and per-model config
  live. Published to the host on `localhost:4000`.
- **A Jaeger sidecar** — receives OpenTelemetry traces from `chat` and `eval`;
  UI at `localhost:16686`.
- **A budget-enforcement counter** — accounts per-call spend against the caps
  declared in your `budget` block.
- **Your agents** — the deployed agents declared in the source.

MCP servers are **not** long-running Compose services. They are spawned as
stdio child processes by `aislang chat` for the duration of a session, then
torn down when the session ends.

Provider credentials never enter the emitted artifacts. `aislang plan` writes
only a safe `.env.example` template (safe to commit) listing the env vars
LiteLLM expects — for example `ANTHROPIC_API_KEY` for an Anthropic primary or
`OLLAMA_API_BASE` for an Ollama primary. You copy it to a local `.env`, fill in
real values, and `docker compose` loads it at runtime.

```hcl
deploy "local" {
  target = compose
}
```

```sh
# Copy the emitted template and fill in your keys.
cp .env.example .env
# edit .env — add ANTHROPIC_API_KEY, etc.

# Stand up the stack (LiteLLM + Jaeger + budget counter + agents) in Docker.
aislang apply examples/support-agent.ais

# Chat with a deployed agent. `aislang chat` runs the tool-use loop in-CLI,
# talking to the Compose-managed LiteLLM at localhost:4000 and spawning the
# declared MCP servers as stdio children for the session.
aislang chat support examples/support-agent.ais

# When you're done, tear it all down (containers + volumes).
aislang destroy examples/support-agent.ais
```

### Browser chat via Open WebUI

For users without an MCP-aware client, add `web_ui = true` to your deploy
block's `expose { … }` to bring up an [Open WebUI](https://docs.openwebui.com/)
sidecar wired to the in-stack LiteLLM:

```hcl
deploy "local" {
  target = compose
  expose {
    web_ui = true     # → http://localhost:3000 after `aislang apply`
  }
}
```

After `aislang apply`, browse to **http://localhost:3000**, sign up (local
SQLite — the first user becomes admin), and pick any model alias from the
dropdown. Every model LiteLLM is configured to route appears there — handy for
confirming an agent's chosen model actually replies without writing any client
code.

`aislang destroy` runs `docker compose down -v`, which wipes the
`openwebui-data` volume — chat history and local accounts go away with the rest
of the stack.

## Kubernetes (Helm)

Flip `target = kubernetes` and `aislang apply` packages the same stack as a
Helm chart and installs it with `helm upgrade --install`.

```hcl
deploy "prod" {
  target = kubernetes
  expose {
    web_ui  = true                     # → Open WebUI Service (ClusterIP)
    ingress = "support.example.com"    # opt-in Ingress; omit for port-forward
  }
}
```

### Prerequisites

You need `docker`, `kubectl`, and `helm`. For a local cluster,
[kind](https://kind.sigs.k8s.io/) (Kubernetes in Docker) gives you a working
cluster in about 30 seconds:

```sh
# Docker — required prerequisite (same as target = compose).
sudo apt update && sudo apt install -y docker.io
sudo usermod -aG docker $USER && newgrp docker

# kubectl — the Kubernetes CLI.
curl -sSL -o /tmp/kubectl "https://dl.k8s.io/release/$(curl -sSL https://dl.k8s.io/release/stable.txt)/bin/linux/amd64/kubectl"
sudo install -m 0755 /tmp/kubectl /usr/local/bin/kubectl

# helm — chart installer.
curl -sSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash

# kind — local cluster.
curl -sSL -o /tmp/kind https://kind.sigs.k8s.io/dl/v0.24.0/kind-linux-amd64
sudo install -m 0755 /tmp/kind /usr/local/bin/kind

# Spin up a cluster and sanity-check it.
kind create cluster
kubectl get nodes
```

On macOS, swap the `apt install` step for `brew install docker kubectl helm
kind` and make sure Docker Desktop is running.

### Provider credentials

`aislang` never writes secret bodies into the chart values. You create the
provider-creds Secret yourself; the chart references it via
`envFrom: secretRef: aislang-providers`:

```sh
kubectl create secret generic aislang-providers \
  --from-literal=ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" \
  --from-literal=OPENAI_API_KEY="$OPENAI_API_KEY" \
  --from-literal=OLLAMA_API_BASE="$OLLAMA_API_BASE"
```

### Plan, apply, port-forward, destroy

```sh
# Plan — writes manifests/ + a Helm chart/ to disk, but installs nothing.
aislang plan examples/support-agent-k8s.ais

# Apply — runs `helm upgrade --install <deploy-label> chart/`.
aislang apply examples/support-agent-k8s.ais
```

Open WebUI is a `ClusterIP` Service by default — reachable only from inside the
cluster. Either port-forward it (works on any cluster, no Ingress controller
needed) or set `ingress = "<host>"` in your source and point DNS at your
cluster's Ingress controller:

```sh
kubectl port-forward svc/openwebui 3000:8080
```

`aislang chat` runs from your machine and dials LiteLLM at a fixed
`localhost:4000`. Under Kubernetes, LiteLLM is a ClusterIP Service, so
port-forward it in a second terminal before chatting:

```sh
kubectl port-forward svc/litellm 4000:4000
aislang chat support examples/support-agent-k8s.ais
```

Tear it down with either command — both are idempotent:

```sh
aislang destroy examples/support-agent-k8s.ais   # runs `helm uninstall <deploy-label>`
# or directly:
helm uninstall <deploy-label>
```

### Overriding chart values

The chart exposes a small set of install-time overrides you can apply without
re-running `aislang plan`:

```sh
helm upgrade --install prod ./chart \
  --set litellm.replicas=3 \
  --set openwebui.persistence.size=5Gi \
  --set ingress.host=support.example.com
```

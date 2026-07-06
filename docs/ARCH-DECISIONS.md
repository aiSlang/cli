# aiSlang — Architectural Decisions

This reference compiles all 69 architectural decisions for aiSlang, a Rust CLI for declarative AI infrastructure. They were extracted from the v0.1–v0.4 planning history (`PLAN-90-days-v0_1.md`, `PLAN-90-days-v0_2.md`, `PLAN-v0_3.md`, `PLAN-v0_4.md`) and are cited by number ("Decision N" / "Decision N.M") throughout the codebase, docs, and examples. Each entry summarizes what was decided and why, faithful to the original planning rationale.

## v0.1 (Decisions 1–38)

### Decision 1 — Agent runtime lives in the CLI (Option A)
For v0.1, `aislang chat <agent>` runs the agent loop in-process inside the Rust binary. The Compose stack supplies only long-running supporting services (LiteLLM, MCP servers, Jaeger, budget counter); the week-4 `agent_runtime` placeholder service is dropped. `deploy.expose` ports have no service to bind to, so they are emitted only as an informational YAML header comment while the AST field is preserved. The eventual v0.2 shape is a dedicated containerized agent service per `agent` block, fronted by Open WebUI, with `aislang chat --remote` dialing it.

### Decision 2 — MCP transport is stdio via the official Rust SDK (`rmcp`)
Although SSE was originally planned, the 2026 MCP ecosystem is overwhelmingly stdio-first: the official Anthropic/Docker-curated server images spawn per-session over stdio. v0.1 therefore uses `rmcp`'s stdio child-process transport, its most-tested code path. SSE becomes the v0.2 path once aiSlang owns its own gateway (see Decision 5).

### Decision 3 — Non-streaming chat completions
v0.1 returns one full response per turn with no token-by-token streaming. The chat client targets demos and stakeholder walkthroughs, not production UX. Streaming is deferred to v0.2 as an isolated change inside `aislang-runtime::completion`.

### Decision 4 — Tool-use loop capped at 8 iterations per turn
If the model keeps emitting `tool_call`s past 8 iterations without a final assistant message, the loop returns `"max tool calls exceeded (8)"` and hands control back to the user. This prevents infinite loops during demos and is written into the agent-runtime contract so the future containerized runtime inherits the same cap.

### Decision 5 — MCP servers are CLI-spawned stdio children; in-house gateway is the v0.2 SSE bridge
Locked after a real `docker compose up` revealed the placeholder MCP image did not exist and that even the real `mcp/filesystem:1.0.2` image is stdio-only, not a network service. Rather than wrap servers in supergateway (Option A, rejected for its npm dependency and grammar growth), v0.1 spawns each declared MCP server as a `docker run -i` stdio child at chat start and kills it on exit (Option C); the Compose emitter lists them only as informational header comments. v0.2 will ship an in-house `aislang-mcp-gateway` Rust crate/image that bridges stdio to JSON-RPC-over-SSE with secret scope-injection, OTel spans, and per-agent network allowlists.

### Decision 6 — Budget enforcement lives in `aislang-runtime`, not LiteLLM `max_budget`
LiteLLM's built-in budget feature requires virtual-key auth plus a Postgres/SQLite state store; v0.1 keeps enforcement simple in Rust. Catalog cost-per-1k-tokens times usage stats accumulates a running total in `Session`, persisted to a small JSON file. A `--remote-budget` flag may arrive in v0.2 if multi-machine accounting is needed.

### Decision 7 — Per-day spend in `~/.aislang/spend.json`
Spend is tracked in plain JSON, written atomically (tempfile + rename) and keyed by `(project_name, utc_date)`, so it survives `kill -9`. v0.2 Phase-1 may promote this to a remote backend; the v0.1 schema reads cleanly into any additive-only v0.2 schema.

### Decision 8 — v0.1 secret sources are `env://VAR` and `file://path` only
Only these two zero-dependency schemes are supported (`env::var` + `std::fs::read`); `vault://`, `gcp-secret://`, `aws-sm://`, etc. hard-fail at parse time with a pointer to LANGUAGE-SPEC §5.7. Cloud secret backends are Phase-1 work behind a feature flag.

### Decision 9 — Runtime secret scoping enforced by construction
The `spawn(SpawnConfig)` helper only receives the env pairs the caller passes, and the CLI builds that list by walking a server's `mcp_server.secrets` refs through `project.secrets`. Because nothing else is ever plumbed into the spawn, there is nothing to filter — eliminating a whole class of "did we forget to scope this?" bugs.

### Decision 10 — `unlimited` is a `Money` literal
`hard_cap_per_day = unlimited` reads honestly, whereas an omitted field would look accidental. `Money` becomes an enum, `Capped { currency, micro_units } | Unlimited`, with the literal parser accepting the bare keyword. Cost arithmetic treats `Unlimited` as the absorbing top so comparisons always pass.

### Decision 11 — Enterprise secret providers are v0.2; `secret_provider` is a reserved keyword
Reserving `secret_provider` now guarantees existing `.ais` files keep parsing when a future grammar adds a `secret_provider "…" { type=vault, … }` block that secrets reference via `provider = secret_provider.X`. Vault auth flows, secret refresh, and audit trails are Phase-1+. Direction documented in LANGUAGE-SPEC §5.7.

### Decision 12 — Eval invocation surface `aislang eval [<eval-label>] <file>`
The label is optional: omitted, it runs every `eval` block in the project; supplied, only the named one. The overall exit code is 0 only when every eval passes its threshold — mirroring `aislang chat`.

### Decision 13 — Eval agent resolution
The `eval` AST carries an optional `agent` ref. When set, the eval runs against that agent; when unset, it runs against the project's single `agent` block and errors if there are zero or two-or-more agents, forcing the user to disambiguate.

### Decision 14 — JSONL dataset, minimal schema
Each dataset line is `{"input": "...", "expected": "..."}` with an optional `"id"`. Empty lines and `#`-prefixed comment lines (post-trim) are silently skipped, matching the `.ais` comment convention for inline annotation. Extra JSON fields are ignored for forward-compat (`tags`, `weight`, `metadata` are reserved for v0.2 datasets).

### Decision 15 — v0.1 metrics are `exact_match` and `f1` only
ExactMatch normalizes (trim + lowercase) then compares to 0.0/1.0; F1 is a bag-of-tokens precision/recall harmonic mean. The other AST metric variants (`bleu`, `rouge_l`, `llm_judge`) parse for forward-compat but hard-error at run time with `"metric not yet implemented (v0.2)"`. The v0.2 direction is a single unified catalog where any entry can act as an `llm_judge` (a.k.a. "oracle"), with an optional informational `llm_judge: bool` catalog flag.

### Decision 16 — Eval exit codes 0/1/2
`aislang eval` returns 0 when all evals pass threshold (CI-green), 1 when at least one fails threshold (CI-fail), and 2 on configuration error (missing file, unknown agent, malformed dataset, unimplemented metric). This lets CI distinguish "your evals regressed" from "your project is broken."

### Decision 17 — Sequential eval case execution
Dataset cases run sequentially through one `Session`. Parallel execution is a v0.2 perf optimization; concurrent budget-state mutation is non-trivial and 100-case datasets are tolerable (~3 min on a real LLM).

### Decision 18 — OTel Rust crates `opentelemetry` + `opentelemetry-otlp` + `opentelemetry_sdk`
The official Rust SDK is used, with OTel-GenAI semantic conventions stabilized in the `gen_ai.*` namespace as of spec 1.27+. No competing crate warranted consideration for v0.1.

### Decision 19 — Exporter is OTLP HTTP, not gRPC
HTTP matches Jaeger's already-exposed `4318/http` port, whereas gRPC would need an extra port mapping. Default endpoint `http://localhost:4318/v1/traces`.

### Decision 20 — 100% trace sampling in v0.1
Eval volumes (5–100 cases) and chat volumes (single-digit turns) are tiny enough that head-sampling everything pays for itself in debuggability. Ratio-sampling awaits high-volume scenarios in v0.2+.

### Decision 21 — Trace ID propagation, one trace per CLI invocation
The root span is `aislang chat <agent>` or `aislang eval <label>`, with child spans per LLM call and per MCP tool call. All tool-use-loop iterations share one trace ID, so clicking through shows the whole turn, not a fragment.

### Decision 22 — Trace URL emission to stderr
After every span-producing run, a single `trace: http://localhost:16686/trace/<trace-id>` breadcrumb prints to stderr while stdout stays pipe-clean. The retrospective confirmed this single-breadcrumb UX was the right call.

### Decision 23 — EvalReport gains `trace_url` per case and at report level
The report-level `trace_url` links to the run's root trace; the per-case field is a Jaeger span deep-link (`/trace/<id>?uiFind=<span-id>`) into the same trace, operationalizing the audit-trail story.

### Decision 24 — Zero-config local trace default
The OTLP endpoint defaults to `http://localhost:4318/v1/traces`; users wanting a remote collector set the standard `OTEL_EXPORTER_OTLP_ENDPOINT` env var. No new `.ais` grammar fields in v0.1.

### Decision 25 — State file location `<source_dir>/.aislang/state.json`
The apply-time state file lives in a hidden dir alongside `aislang.lock` and `stack.yml`. v0.2 may grow per-environment states (`.aislang/state.<env>.json`).

### Decision 26 — State schema (v0.1)
A schema-version-prefixed JSON (same pattern as lockfile and spend.json) recording `applied_at` (RFC3339 UTC), `source_hash`, `catalog_version`, `stack_yaml_path`, and `project_name`. A null `applied_at` means torn-down, with the file preserved for audit-trail continuity.

### Decision 27 — Destroy idempotency
When both state and stack.yml are absent, destroy exits 0 ("nothing to destroy"); when `applied_at` is None it exits 0 ("already torn down"). An exit-1 drift error is reserved only for the state-present-but-stack.yml-missing case, pointing the user back at apply.

### Decision 28 — `aislang status` subcommand
Three composable sections gated by flags: default (stack), `--spend` (today's total), and `--model-spend [=MODEL]` (per-model breakdown with optional filter). Exit codes are 0 clean / 1 drift / 2 docker-unavailable. `--model-spend` uses `require_equals` to avoid clashing with the positional FILE argument.

### Decision 29 — `destroy --force` flag
`--force` bypasses state-file checks for users who manually deleted their `.aislang/` directory.

### Decision 30 — State file is distinct from the lockfile
They have separate lifecycles: the lockfile pins resolution-time decisions, while the state file records apply-time facts. Keeping them separate keeps the plan/apply boundary clean.

### Decision 31 — `status` ships in v0.1
The status command is in-scope with no escape hatch or deferral.

### Decision 32 — spend.json schema extension for per-model spend
`DayEntry` gains a `by_model: BTreeMap<String, i64>` field with `#[serde(default, skip_serializing_if)]`. Existing files load with an empty map and no schema-version bump, since the change is purely additive.

### Decision 33 — SQuAD-style F1 tokenization
The naïve whitespace tokenizer was rewritten so every non-alphanumeric character is a token separator, because the old split scored factually-correct markdown-bold/punctuated answers at F1=0. The wire-contract floor is that factually-correct answers must never score zero; `llm_judge` will subsume this in v0.2.

### Decision 34 — Repo stays private through v0.1 and v0.2
The repo flips public on v0.3 launch day, to avoid fork-and-out-ship by better-resourced teams before the commercial moat exists. The `install.sh` one-liner is wired into the release workflow but distributes via authenticated `gh release download`; the public URL only goes live with v0.3.

### Decision 35 — CI matrix `ubuntu-latest` + `macos-latest`, stable Rust
No Windows in v0.1 (Compose targets Linux/macOS; Windows goes via WSL) and no MSRV pinning beyond the workspace `rust-version`.

### Decision 36 — Release target matrix
Four tarballs per release — `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin` — plus a `checksums.txt` for integrity. A Homebrew tap is deferred to v0.3 as launch-surface, not core.

### Decision 37 — `install.sh` semantics
The script detects OS/arch via `uname`, fetches the matching tarball from the latest GitHub release, verifies its SHA-256 against `checksums.txt`, and installs to `${PREFIX:-$HOME/.local/bin}/aislang` with `chmod +x`. No sudo prompts (users set `PREFIX` for `/usr/local/bin` and elevate themselves); Windows and unsupported arches get a clear bail message.

### Decision 38 — Cold-VM rehearsal before tagging
v0.1.0 ships only after `install.sh` runs end-to-end on a fresh Ubuntu VM and a fresh macOS VM — not merely when CI goes green. CI verifies the build; the rehearsal verifies the actual user surface.

## v0.2 (Decisions 39–42 and 43.1–43.8)

### Decision 39 — MCP transport rollout
`aislang mcp-serve` uses rmcp's stdio transport in Week 12, with HTTP/SSE added in Week 13 via the same crate (an additional transport binding, not a new dependency).

### Decision 40 — MCP tool annotations
`destructiveHint: true` is set on `aislang_apply`, `aislang_destroy`, and `aislang_cloud_push` (the last being Track C). All other exposed tools are `readOnlyHint: true` or unannotated.

### Decision 41 — `main` block parsing
The `main { … }` block reuses the v0.1 `agent`-block converter template, and its `pattern` field is a closed enum parsed through the existing enum_member machinery.

### Decision 42 — `source = subscription | api` on `model`
An optional `source` field is added to the `model` block, defaulting to `api`, so existing v0.1 files continue to validate unchanged (forward-compat).

### Decision 43.1 — One Helm chart per `deploy` block
Each chart is named after its deploy label. Chart-per-deploy (rather than chart-per-project) gives `helm upgrade --install <release> chart/` a clean release boundary mirroring the Compose stack name, avoiding re-templating for single-resource changes.

### Decision 43.2 — Supporting services as Deployment + Service (+ ConfigMap)
LiteLLM, Jaeger, and budget_counter emit as Deployment + Service, with LiteLLM also getting a ConfigMap — mirroring the Compose service shape 1-for-1 so the resolver/catalog/litellm_config code paths are shared across both emitters. No StatefulSet: LiteLLM is stateless given an external Redis, which v0.2 does not ship.

### Decision 43.3 — Agent runtime is `aislang mcp-serve --http` in a Deployment + ClusterIP Service
The agent pod runs `aislang mcp-serve --http 0.0.0.0:<expose.port>`. The `.ais` source, `aislang.lock`, and every `prompt` body are baked into an `aislang-project` ConfigMap mounted at `/etc/aislang/`, so the pod boots with exactly the inputs a local `aislang chat` would consume.

### Decision 43.4 — Prompt blocks become individually hashed ConfigMap entries
Each `prompt` block becomes a ConfigMap entry labelled with its lockfile body hash. A body change shifts the hash, re-creating the ConfigMap and triggering a Deployment rolling-restart — free cache invalidation without baking prompts into the image.

### Decision 43.5 — Provider-credential secrets are NOT auto-created
The chart documents the `kubectl create secret generic aislang-providers --from-literal=…` recipe and `envFrom`s it onto the agent-runtime and LiteLLM pods. Auto-creation would either commit secrets to git or require an out-of-scope credential-store sidecar. Sealed-secrets / external-secrets integration is v0.3.

### Decision 43.6 — `deploy.expose.web_ui = true` emits Open WebUI
Setting `web_ui = true` emits an Open WebUI Deployment + Service + PersistentVolumeClaim (1 GiB default, configurable via `values.yaml`). Ingress is opt-in (`expose.ingress = "<host>"`) so the chart works on bare kind without a load-balancer, with `kubectl port-forward` as the default demo path.

### Decision 43.7 — Minimal-templating Helm chart
The chart wraps raw manifests as templates exposing only `image.tag`, `replicas`, `ingress.host`, and `openwebui.persistence.size` — no `{{ if }}` gymnastics. Because the `.ais` source already declares "with web_ui or without," the emitter simply chooses which templates to write, keeping the chart legible to operators new to aiSlang.

### Decision 43.8 — Two views of the same resources
The emitter produces both `<output_dir>/manifests/*.yaml` (raw `kubectl apply -f`-able) and `<output_dir>/chart/` (`helm upgrade --install`-able). `aislang apply` uses the chart path so `aislang destroy` can `helm uninstall` cleanly, while manual users can take the manifests path.

## v0.3 (Decisions 44.1–44.8)

Reserved: 45.x slots stay reserved for v0.4 Track C control-plane scaffolds and carry no concrete content in v0.3.

### Decision 44.1 — Module system: `aislang.toml` root, `*.ais` modules
`aislang.toml` marks a project root; single-file projects (everything v0.2 shipped) work unchanged because the parser auto-detects the absence of `aislang.toml` and treats a lone `.ais` file as a one-file project. Multi-file projects declare `[project]` metadata and a `[modules]` table, and reference cross-file resources via `use <module>.<resource>` (file name = module name by convention). Cross-file resolution runs in the validator before the resolver. Cross-module access control (private vs public resources) is deferred to v0.5+.

### Decision 44.2 — Patterns runtime lives in `aislang-runtime`, not `aislang-cli`
The CLI merely dispatches; the runtime owns each pattern's state machine — orchestrator (plan → dispatch → aggregate), supervisor (monitor + intervene), router (classify-and-forward), reflector (critique-and-retry), planner_executor (tree-search plan + execute), and coordinator (peer-to-peer). Each pattern is roughly 100–200 LoC plus 5–10 tests.

### Decision 44.3 — MiniJinja for template backing
MiniJinja (~5K LoC, Jinja2-compatible, used by `uv`/`maturin`/`ruff`) backs prompt templating, chosen over the more verbose Handlebars and less-maintained Tera. It provides variable substitution, `{% if %}`/`{% for %}` blocks, and filters (`|upper`, `|json`, etc.); no sandboxing is needed since prompts are author-controlled strings, not user input.

### Decision 44.4 — Embedded vector DB: `usearch` first, `hnsw_rs` fallback
The embedded vector index uses the `usearch` Rust crate (falling back to `hnsw_rs` if usearch's distance-metric set proves too narrow), avoiding a separate Chroma/Qdrant service. The index persists as a single file at `<source_dir>/.aislang/vector/<store-label>.usearch`, wiped by `aislang destroy -v`. (Note: v0.3 shipped a brute-force cosine scan behind a usearch-compatible API to avoid the C++ build dep; usearch can drop in later.)

### Decision 44.5 — Skills compose into pipelines via shell + MCP-tool blend
Skills are first-class blocks like `prompt`/`mcp_server`, e.g. `skill "search_web" { exec = "curl -s …" parse = json output = list<string> }`. Agents reference them via `agent.skills = [...]` alongside the existing `tools = [...]`, and skills compose into pipelines mixing shell execution and MCP tools.

### Decision 44.6 — RAG: `vector_store` + `embedding` as first-class resources
`embedding "<label>" { provider = … model = … }` and `vector_store "<label>" { embedding = embedding.<label> source = path|url|crawl }` are first-class blocks; agents attach corpora via `agent.knowledge = [vector_store.<label>]`. At apply time `aislang plan` computes the index; at runtime a RAG-aware system-prompt template auto-includes retrieved chunks. The `knowledge` list mirrors the `tools` list and is byte-stable for existing agents (absent → no RAG).

### Decision 44.7 — Annotations deferred to v0.5
Annotations are pure syntactic sugar that nothing in v0.3 requires, so they are deferred to v0.5, the next odd (DSL-focused) milestone.

### Decision 44.8 — Local-LLM scope: Qwen3-7b + Qwen2.5-Coder-7b via Ollama, no fine-tuning
Both models fit on consumer GPUs (12–24 GB VRAM), and Qwen2.5-Coder is the current open-source coding leader on HumanEval. Catalog entries land and a dogfood eval compares them against Haiku/Sonnet on aiSlang-development tasks. LoRA and distillation are deferred to v0.5+ because v0.3 lacks time to build training infrastructure.

## v0.4 (Decisions 46.1–46.11)

Reserved: 47.x slots stay reserved for the v0.5+ DSL-extension surface (annotations / private resources / structured pipeline types) and the v0.5 managed tier; they carry no concrete content in v0.4. Decisions 46.1–46.8 were previewed as open questions at v0.3's Sunday pre-lock (PLAN-v0_3.md §"Decisions 46.x stubs"); the authoritative wording below is from v0.4, where they were locked. Note that Decision 46.3's per-1k-tokens billing intent is later revised by Decision 46.10.

### Decision 46.1 — Control-plane endpoint shape: REST over HTTPS, bearer-token auth
The control plane is REST over HTTPS with JSON bodies and per-tenant bearer tokens, chosen as lowest-friction for the v0.4 audience of operators and GitOps tooling (gRPC would add toolchain cost without payoff). All endpoints live under `/v1/`; tokens are issued per-tenant at signup, with rotation via a `/v1/tokens` endpoint deferred to v0.5+ (v0.4 ships a single per-tenant token). Originated as an open question at v0.3's pre-lock.

### Decision 46.2 — Tenant isolation boundary: Postgres schema-per-tenant
Isolation uses one Postgres schema per tenant — not shared-schema-with-RLS, and not separate databases — giving strong isolation without per-tenant DB ops cost and aligning with the v0.5+ `store.db` migration plan. The tenant slug becomes the schema name, with a per-tenant connection pool issuing `SET search_path TO <slug>` on checkout. Originated as an open question at v0.3's pre-lock.

### Decision 46.3 — Billing surface: usage-metered via Stripe metering API
Billing meters per-1k-tokens for inference (mirroring v0.1 spend.json micro-units) plus per-SP-equivalent for control-plane operations (each `apply`/`destroy`/`plan` counts as 1 SP-equiv). The free tier rate-limits control-plane endpoints to 50 SP-equiv/day; paid tiers unlock higher volume; seat-based billing is deferred to v0.5+. Originated as an open question at v0.3's pre-lock. This billing intent is revised by Decision 46.10, which drops token metering from v0.4.0 in favor of BYO-key.

### Decision 46.4 — GitOps webhook trigger: PR-open → auto-plan → PR comment
A PR-open event triggers an automatic `aislang plan` whose result is posted as a PR comment — not auto-apply on push. Auto-apply is too dangerous given Decision 44.4's `destroy -v` semantics, and PR-comment matches existing norms (Terraform Cloud, Atlantis). Originated as an open question at v0.3's pre-lock.

### Decision 46.5 — Multi-tenancy gate: `X-Aislang-Tenant` header, not a `tenant {}` block
Tenant context is carried by an `X-Aislang-Tenant: <slug>` header on the control-plane API rather than a first-class `tenant { … }` grammar block. This keeps the `.ais` surface stable for self-hosted users (zero-impact migration to multi-tenant cloud) and treats tenancy as a deployment concern; self-hosted single-tenant deployments simply omit the header. Originated as an open question at v0.3's pre-lock.

### Decision 46.6 — W20 sketch: partial landing
The JSON inter-agent contract and tool-call structured-output mode land in v0.4 Phase 1 (required for agents to author v0.4 code), while the PostToolUse hook surface and conditional hook engagement defer to v0.5+ as their own quarter of work. Originated as an open question at v0.3's pre-lock.

### Decision 46.7 — Dogfood-findings carry-overs: partial landing
Tool-call parser-converter templating, the `aislang rag verify` CLI, and the Reviewer-corpus parity rewrite land in v0.4 Phase 1; negative-shape eval blocks defer to v0.5+ as a quality lift after the structural fixes land. Originated as an open question at v0.3's pre-lock.

### Decision 46.8 — External quality-gate tools via MCP: deferred to v0.5+
SonarQube/SonarCloud, deeper Clippy, `cargo audit`, and license-compliance integrations remain out of scope, unchanged from the v0.3 position. Originated as an open question at v0.3's pre-lock.

### Decision 46.9 — Primary customer interface: `aislang --target` CLI + OpenAPI/Swagger
Every existing CLI verb (`validate`/`plan`/`apply`/`destroy`/`status`/`chat`) gains a `--target=<URL>` flag delegating the operation to the control plane over the Decision-46.1 REST surface, so self-hosted and cloud users run the same binary and differ only by `--target`. An OpenAPI 3 spec (generated via `utoipa` from the axum handlers) and a Swagger UI at `/docs` give integrators a typed contract. An MCP-cloud bridge (`mcp__aislang__*` tools dialing the managed plane) defers to v0.4.1 as a thin re-target that does not gate launch. Locked in Phase-3 scoping (W24 Fri).

### Decision 46.10 — v0.4.0 billing model: BYO-key + SP-equivalent metering only
This revises Decision 46.3: in v0.4.0 customers bring their own provider keys (via the `aislang-providers` Secret already wired by the K8s emitter, Decision 43.5), so aislang.cloud meters only control-plane SP-equivalents (`apply`/`destroy`/`plan` = 1 SP-equiv each) and not inference tokens, which customers pay their provider directly. The free tier stays 50 SP-equiv/day. Managed inference — where aislang.cloud holds the keys and bills tokens with a markup — defers to a v0.5 managed tier, keeping v0.4.0's billing to the single meter event the pipeline already ships. Locked in Phase-3 scoping (W24 Fri).

### Decision 46.11 — Managed AWS: control-plane self-deploy AND per-tenant execution ship at v0.4.0
Originally per-tenant execution was deferred to v0.5 on a BYO-key/self-host reading, but the operator's W25 integration testing found that plan-only cloud `apply` (emitting artifacts without running anything) does not deliver "managed cloud," so per-tenant execution was pulled into the launch-blocking critical path. The v0.4.0 model is one shared EKS cluster with per-tenant namespaces (not per-tenant clusters/VPCs): the operator self-deploys the control plane, `apply` emits K8s via the existing `aislang-k8s` emitter and `helm install`s the stack into a `t_<slug>` namespace with a NetworkPolicy + ResourceQuota, and `destroy` uninstalls it. Billing stays BYO-key + SP-equiv (46.10) with no compute markup. Still deferred to v0.5: managed inference (markup on tokens) and per-tenant dedicated clusters/VPCs. Locked W25, revised W25 Wed.

# Locked architectural decisions (Decisions 44.1-44.8)

These are PRE-LOCKED for v0.3 Sunday and shape every new resource
type the Architect agent designs.  Cross-reference these in
EVERY design — if a proposed block conflicts with a locked
decision, surface that conflict explicitly in the design's
"Decision implications" section.

## 44.1 — Module system

`aislang.toml` at the project root + `*.ais` files as modules.
Single-file projects (no aislang.toml) work unchanged.  Multi-file:
modules export resources via `use <module>.<label>` cross-file refs.
File-name = module-name convention.

**Implication for new resource types**: every resource must work in
BOTH single-file AND multi-module mode (cross-module refs follow
the same shape).

## 44.2 — Patterns runtime sits in `aislang-runtime`, NOT `aislang-cli`

Runtime owns the six pattern state machines (orchestrator,
supervisor, router, reflector, planner_executor, coordinator).
Each pattern is ~100-200 LoC + 5-10 tests.

**Implication**: if a new resource adds runtime behaviour, the
business logic goes in `aislang-runtime`; the CLI just dispatches.

## 44.3 — MiniJinja for template backing

Dynamic prompts + (future) skill arg substitution use MiniJinja
(Jinja2-compatible).  Variable substitution + `{% if %}`/`{% for %}`
blocks + filters; HTML-escape ON by default with `|safe` opt-out.

**Implication**: new resource types that need user-templated
strings (prompts, skill exec commands, vector_store URLs) route
through MiniJinja — don't reinvent.

## 44.4 — Embedded vector DB: usearch (deferred to v0.5+)

usearch first; hnsw_rs as fallback.  Index persisted as a single
file under `<source_dir>/.aislang/vector/<store-label>.usearch`
(v0.3 Wed ships brute-force cosine; usearch swap-in is v0.5+).

**Implication**: new resources that need persistent on-disk
state put it under `<source_dir>/.aislang/` — never write outside.
`aislang destroy -v` wipes that subtree.

## 44.5 — Skills compose into pipelines; shell + MCP-tool blend

`skill` is first-class (alongside `prompt`/`mcp_server`).  Agents
reference skills via `agent.skills = [...]` (future) alongside
`tools = [...]`.  Output types are a closed set: string | json |
list_string.

**Implication**: new resources that produce typed output use the
same closed-enum shape (avoid introducing parameterised generics
in v0.3).

## 44.6 — RAG: vector_store + embedding as first-class resources

`embedding "<l>" { provider = X  model = "Y" }` + `vector_store
"<l>" { embedding = embedding.<l>  source = path | url | crawl }`.
Agents reference via `agent.knowledge = [vector_store.<l>]`.

**Implication**: model-like resources should mirror this provider+
model shape; resolver validates against catalog.

## 44.7 — Annotations DEFERRED to v0.5

Pure syntactic sugar; nothing in v0.3 needs them.  Decorators-on-
blocks (`@requires-tool`, `@audit-trail`) revisit at v0.5.

**Implication**: do NOT design annotation-style decorators in v0.3
— recommend `agent.<field>` boolean / list fields instead.

## 44.8 — Local-LLM scope: Qwen3 + Qwen2.5-Coder via Ollama

Catalog entries land for both 7B variants (actual: Qwen3 8B since
Ollama doesn't ship 7B; Qwen2.5-Coder 7B verbatim).  No fine-
tuning in v0.3; LoRA + distillation deferred to v0.5+.

**Implication**: new resources that interact with LLMs should
work with EITHER paid (anthropic/openai) OR local (ollama) backends
— don't bake provider-specific assumptions into the grammar.

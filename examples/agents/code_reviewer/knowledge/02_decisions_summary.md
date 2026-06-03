# Decisions 44.x — review-relevant summary

The full text lives in the repo's PLAN-v0_3.md.  This is a
review-focused condensation.

## 44.1 — multi-module via `aislang.toml`

A project may be one .ais file OR a directory with
`aislang.toml` + multiple `.ais` modules.  Cross-module refs
use `use <module>.<label>`.

**Review impact:** if the patch adds a new ref kind, make
sure cross-module resolution (`resolve_multi`) handles it.

## 44.2 — 6 composition patterns

orchestrator / router / supervisor / reflector /
planner_executor / coordinator.  All implemented as runtime
patterns; the grammar surface is `pipeline / loop / branch`.

**Review impact:** new blocks should NOT add a 7th pattern at
the grammar layer.  v0.5+ may revisit.

## 44.3 — MiniJinja templates, HTML auto-escape default

Templates use MiniJinja with HTML auto-escape on by default.

**Review impact:** any new field that takes a template string
follows the same rendering pipeline (`render_template`
helpers) — no parallel templating engine.

## 44.4 — `aislang destroy -v` byte-stability + persistent
## state under `<source_dir>/.aislang/<subtree>/`

Two-part decision:
1. v0.2-shaped projects round-trip identically after the
   parser learns the v0.3 surface (serde `default` +
   `skip_serializing_if`).
2. Any persistent state lives under
   `<source_dir>/.aislang/<subtree>/` so `destroy -v` finds
   and removes it.

**Review impact:** this is the discipline-most-violated
decision.  Reject any patch missing the serde attrs OR
writing state outside `.aislang/`.

## 44.5 — composition primitives = pipeline + loop + branch;
## no parameterised generics in the type system

Closed list of composition blocks (no extension at v0.3).
Simple types only — String, Path, SemVer, Size, Duration,
Money, Bool, Capability, Ref<X>, List<T> for a single
non-recursive T, closed enums.

**Review impact:** if the patch introduces `Map<K,V>` or
nested generics, reject.  If it adds a new field with type
`List<List<String>>`, reject — Decision 44.5 says no.

## 44.6 — `vector_store` + `embedding` first-class blocks

RAG resources live as their own blocks, not as ad-hoc
fields on `agent`.

**Review impact:** if the DESIGN introduces a new
RAG-adjacent concept, it should be a block of its own, not a
field on `vector_store`.

## 44.7 — control plane decisions reserved for v0.4+

Track C (control plane) decisions are out of v0.3 scope.
Any DESIGN that triggers a v0.4-scope decision should defer
the implementation.

**Review impact:** if the patch implements a v0.4-scope
feature, reject with "defer to v0.4 per Decision 44.7".

## 44.8 — Qwen3 8B + Qwen2.5-Coder 7B as the catalog Qwen
## entries; provider stays Ollama

Locked catalog shape: `ollama.qwen3_8b` (reasoning) +
`ollama.qwen2_5_coder_7b` (code-tier).

**Review impact:** patches that add catalog entries follow
the same `{ id, provider, litellm_name, capabilities,
context_max, ... }` shape.

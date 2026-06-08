# Inter-agent JSON contract (v0.4 W21 Thu — Decision 46.6)

This directory holds the JSON Schema (draft 2020-12) files that
pin the wire format between the dogfooded agents.  v0.3 used
markdown headers (`## Block name`, `## Patch`, etc.) as the
inter-agent contract; v0.4 migrates to JSON via tool-call
structured-output mode (LiteLLM passes the tool schema through
to the model so output is schema-validated at generation time).

## Why JSON

W20 Thu dogfood findings (`examples/agents/orchestrator/
DOGFOOD-FINDINGS.md`) lesson #4: "the Reviewer ACCEPTed a
hallucinated patch on first pass because the model was free
to emit plausible-looking but wrong code in any markdown
section."  Tool-call structured output mode forces the model
to commit to a structured shape at generation time —
schema-validated, parseable without LLM-as-judge prompts on
the receiver, and trivially convertible to Rust types.

Pairs with the W21 Mon `propose_block_converter` tool-call
spike: that fix targeted the Coder's parser-converter section
specifically; this Thu commit generalises the shape to ALL
inter-agent transitions in the orchestrator chain.

## Schemas

| File | Producer agent | Consumer agent(s) | Section equivalent (v0.3) |
|---|---|---|---|
| [`architect_output.schema.json`](architect_output.schema.json) | Architect | Coder, DB | `## Block name` / `## Purpose` / `## Fields` / `## Grammar example` / `## LANGUAGE-SPEC §4.<N> snippet` / `## Decision implications` |
| [`coder_output.schema.json`](coder_output.schema.json) | Coder | Reviewer, Tester | `## Patch` / `## Test coverage` / `## Notes` |
| [`reviewer_verdict.schema.json`](reviewer_verdict.schema.json) | Code-reviewer | Tester, Coder (revision turn) | `ACCEPT` line / `FAIL: …` feedback |
| [`tester_output.schema.json`](tester_output.schema.json) | Tester | (loop terminator / human) | `## Test plan` / `## Skill invocation` / `## Verdict` |

All 4 reserve a `## ERROR:` exit equivalent via the canonical
`{"error": { "code": "...", "message": "..." }}` envelope —
see [`error_envelope.schema.json`](error_envelope.schema.json).
The orchestrator runtime detects the `error` key at the top
level + aborts the pipeline (single source of truth shared
with the W21 Mon `is_hard_error_signal` CLI helper for
mixed-mode chains where some agents still emit markdown).

## Validation

Schemas are loaded + structurally validated at agent-startup
time (no runtime perf hit per-turn) via the
`aislang-runtime::schemas` module.  Unit tests in
`crates/aislang-runtime/src/schemas.rs` round-trip each
schema against:

- A canonical happy-path payload (must validate).
- A negative payload missing a required field (must reject).
- An error-envelope payload (must validate against
  `error_envelope.schema.json`).

This gives the contract a compile-time test surface without
requiring a full jsonschema crate dependency (the validation
is structural: required-field presence + type tags).  Full
draft-2020-12 validation via the `jsonschema` crate lands
in v0.4 Phase 2 if needed.

## Migration shape

The agent prompts (in `examples/agents/<agent>/prompts/<agent>
.md`) are updated to instruct the model to emit a tool call
whose args match these schemas.  LiteLLM's tool-call mode
forwards the schema to the model; the model's reply is
validated by LiteLLM at the wire boundary.  The orchestrator
runtime (`aislang-runtime::session`) extracts the tool call
args + threads them as a JSON string into the next step's
input.  Decision 44.5's "string-only handoff between steps"
remains in force — JSON strings carry the structure.

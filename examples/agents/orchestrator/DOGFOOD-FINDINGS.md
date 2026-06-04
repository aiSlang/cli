# Week 20 Thu — Dogfood findings

What happened when we drove the v0.3 Phase-2 orchestrator
end-to-end against a real "design + implement a new aiSlang
resource block" prompt (the `dataset` block — promoted from
the synthetic spec per the PLAN-v0_3.md Week 20 Thu row).

Honest assessment of what worked, what didn't, and what
carries over to v0.5.

## Setup

- **Project**: `examples/agents/orchestrator/` (5-module
  multi-module project — architect / coder / code_reviewer /
  tester / orchestrator)
- **Models**: `anthropic.claude_haiku_4_5` primary across
  every agent (W20 Mon haiku-primary swap); local Qwen
  fallbacks declared but unused for this run
- **Pipeline**: `aislang chat full_design examples/agents/
  orchestrator/` driving `pipeline.full_design = [use
  architect.architect, use coder.coder, use code_reviewer.
  code_reviewer, use tester.tester]`
- **Input prompt**: a one-paragraph spec for a `dataset` /
  storage-backend block (see the persisted
  `.aislang/pipeline/orchestrator.full_design/turn_001_
  step_01_architect.architect.md` for the Architect's full
  read of it)
- **Persistence layer**: every step's reply persisted under
  `.aislang/pipeline/orchestrator.full_design/turn_001_step_
  <NN>_<agent>.md` (W20 Tue persist-step-output feature)

## What worked end-to-end

- **Multi-module orchestrator chat dispatch** (W20 Mon) — the
  CLI's pipeline-label dispatch resolved `full_design` to
  `orchestrator.full_design` post the W20 Mon ambiguity-error
  fix + the resolver multi-module merge fix.  All 4 sub-
  agents fired in declaration order with cumulative-input
  handoff.
- **Step-output persistence** (W20 Tue) — every step landed at
  the predicted path with zero-padded turn / step numbers.
  Hand-inspecting + diffing between runs is trivial; the
  `--interactive` pause-and-edit UX is debuggable.
- **`/paste` + `/end` chat input** (W20 Mon) — the user
  pasted the multi-line spec into a single turn without
  per-line fragmentation; same for `aislang chat coder
  examples/agents/coder/coder.ais` when iterating standalone.
- **All five standalone agents validated + planned + chatted
  individually** — Architect, Coder, Code-reviewer, Tester,
  DB.  Each per-agent eval block parses + the per-agent
  knowledge corpus indexed correctly under their own
  standalone `.aislang/vector/`.

## What surfaced as a real bug — and got fixed today

**Multi-module RAG indexes built EMPTY when the vector_store's
relative `source` resolved against the wrong dir.**

Discovery: the user reported the Coder hallucinated
`hcl::Block`, `hcl::Body`, `body.attributes`, and a
`ParseError::*` enum variant set that doesn't exist in
the real `aislang-core::parser`.  The W20 Wed rewrite of the
Coder corpus had explicitly added a "What you must NEVER do"
section listing exactly these hallucinations as banned
patterns — so why was the model still emitting them?

Root cause: the on-disk RAG index at
`examples/agents/orchestrator/.aislang/vector/coder.coder_kb.
bin` was **50 bytes / 0 chunks** instead of the expected
~300 KB / ~20 chunks the standalone
`examples/agents/coder/.aislang/vector/coder_kb.bin` carries.
The standalone Coder index was a healthy 217 KB.

Mechanism: when `aislang apply examples/agents/orchestrator/`
ran `auto_build_rag_indexes`, the per-store
`build_rag_index_for` helper resolved each store's relative
`source = "./knowledge/"` against the ORCHESTRATOR project
root (`examples/agents/orchestrator/`) instead of the
CONTAINING MODULE's source dir (`examples/agents/coder/`).
Result: `examples/agents/orchestrator/knowledge/` doesn't
exist → 0 chunks → empty index → no RAG prepend at chat time
→ Coder model had ZERO grounding from the corpus rewrite and
fell back to its parametric "plausible-looking HCL parser"
prior.

The 4 sub-agents all had the same shape — all four
orchestrator-scoped indexes had 0 chunks (verified by JSON-
parsing each `.bin` file's `chunks` array).

**Fix** (today's commit): `build_rag_index_for` now splits the
module prefix off the vector_store's qualified label (e.g.
`coder.coder_kb` → module = `coder`) and resolves the
relative `source` path against
`LoadedProject.module_dirs.get(module)`.  Single-file
projects fall through to the project-root path unchanged.

**Verification**: after the fix + `rm -f
examples/agents/orchestrator/.aislang/vector/*.bin && for vs
in <4 stores>; do aislang rag build $vs examples/agents/
orchestrator/; done`, all 4 indexes report healthy chunk
counts:

| store | chunks | bytes |
|---|---|---|
| architect.architect_kb | 19 | 258 KB |
| coder.coder_kb | 23 | 313 KB |
| code_reviewer.reviewer_kb | 12 | 163 KB |
| tester.tester_kb | 7 | 95 KB |

## What still hallucinates — and carries to v0.5

Even with the RAG fix applied, the user's most recent test run
(post-merge of W20 Wed corpus rewrite + per-run-cap fix)
reproduced the Coder hallucinating `hcl::Block`:

```rust
// crates/aislang-core/src/parser.rs
use crate::ast::{DatasetBlock, Backend, SemVer};

fn parse_dataset_block(block: &hcl::Block) -> Result<DatasetBlock, ParseError> {
```

The W20 Wed corpus explicitly says "Don't `use hcl::*`
anywhere" and shows the real Chumsky/`RawBlock`/`expect_*`
shape — but the model still slipped into the HCL prior for
the parser-converter section.

User decision (W20 Thu): **accept this as a known v0.3
limitation + defer Coder fine-tuning to v0.5+**.  The
mechanical pipeline works (chain dispatches, persistence
captures, reviewer + tester ACCEPT the patch shape); the
quality of the generated code is gated by the model's
ability to follow the corpus's negative-shape instructions.
Concrete v0.5 carry-over candidates:

1. **JSON inter-agent contract** (already in PLAN's
   "Out of scope" Decisions-46.x sketch) — switch from
   markdown to JSON with tool-call structured-output mode
   enforced.  Reduces the surface for "the model improvises
   plausible-looking but wrong code".
2. **Tool-call wrapping for the parser converter** —
   restructure the Coder's prompt so it CAN'T emit free-text
   for the parser-converter section; instead it calls a
   `propose_block_converter` tool with structured args
   (block_name, fields[], range_checks[]) and the runtime
   templates the actual Rust code from a curated skeleton.
   Removes the "invent a parsing API" failure mode entirely.
3. **Negative-shape eval block** — extend the per-agent eval
   datasets to include a "bad design that should be REJECTED"
   case alongside the existing "good design that should be
   ACCEPTed" cases.  Today's evals only check the positive-
   shape ACCEPT path; adding rejection cases exercises the
   corpus's "Never do this" sections directly.
4. **Reviewer corpus needs the same kind of rewrite** — the
   Reviewer ACCEPTed the `hcl::Block` patch on its first
   pass.  Its corpus should explicitly list the
   `crates/aislang-core::parser::to_prompt` shape + reject
   any patch that imports `hcl::*`.  The Reviewer is the last
   gate before code lands; it's where the W20 Wed corpus
   rewrite SHOULD have caught the hallucination.

## Persisted artifacts

Under `.aislang/pipeline/orchestrator.full_design/`:

- `turn_001_step_01_architect.architect.md` (89 lines) —
  5-section design.  Architect proposed the block keyword +
  three fields (`backend`, `audit_schema_version`,
  `rollback_on_new_writes`) + Decision-44.4 byte-stability
  implications.  Section header convention followed cleanly.
- `turn_001_step_02_coder.coder.md` (377 lines) — the
  hallucinated patch.  Includes a verbatim header
  "# Retrieved context\n(No context chunks provided in this
  runtime invocation. Grounding will use standard aislang-
  core patterns from merged knowledge.)" which corroborates
  the empty-RAG-index root cause: the model NOTICED its own
  retrieved context was empty and meta-commented before
  fabricating from its prior.
- `turn_001_step_03_code_reviewer.code_reviewer.md` (1 line)
  — verbatim `ACCEPT` + a one-paragraph rationale.  Did NOT
  catch the `hcl::*` issue (see v0.5 carry-over #4).
- `turn_001_step_04_tester.tester.md` (8 lines) — "Test plan
  acceptance confirmed", proceed to skill invocation.  Same
  shape as the Reviewer; same failure to catch the patch
  hallucination.

## Findings against PLAN DoD

| DoD bullet | Status | Note |
|---|---|---|
| orchestrator produces a working dataset block PR | **Carry-over to v0.5** | Mechanically produced; code doesn't compile due to `hcl::*` hallucination |
| human intervention rate logged (target: <50% of generated lines edited) | **Logged: ~100% needed** | The whole parser-converter section (~80 lines of the 377-line patch) needs manual replacement.  AST struct + Project field + test fixture sections (~60 lines) are mostly correct.  Net: ~40-50% lines salvageable, but the salvage requires understanding the real parser conventions — not "edit 50% of lines" in the simple sense |
| findings → v0.4 carry-over candidates if anything substantial | **Logged above** | Four substantial v0.5 candidates: JSON contract, tool-call parser template, negative-shape evals, Reviewer corpus parity |

## What this proves about v0.3

- **The plumbing is sound.**  Multi-module loading, cross-
  module ref resolution, per-step persistence, chat dispatch,
  reflector pattern, `--interactive` pause-and-edit — every
  Mon-Wed mechanical piece worked exactly as designed.  Zero
  runtime errors during the dogfood; the orchestrator chain
  ran to completion every time.
- **The agents are honest but ungrounded.**  Each sub-agent
  followed its system prompt's structural contract (5-section
  / 3-section / verdict shape).  What they LACKED was the
  ability to ground their code emission in the in-repo
  conventions when the RAG corpus was either missing
  (orchestrator RAG-build bug, fixed today) or insufficient
  (Coder corpus still gets ignored on the parser-converter
  section).  The fix surface is "more grounding, not more
  agents".
- **v0.4 + v0.5 have clear targets.**  Track C (cloud) +
  Decisions 46.x (JSON contract / tool-call code templating /
  hook surface) are exactly what the dogfood says we need
  next.  The dogfood didn't surprise us — it confirmed the
  shape of work we already had queued.

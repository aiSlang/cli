# aiSlang Agents Catalog

> **Generated** — do not edit by hand. Rebuild with:
> `python3 tools/catalog/scan_catalog.py && python3 tools/catalog/render_catalog.py`.

Scanned `examples/` on 2026-07-22T11:46:10+00:00 — **16 agents / multi-agent projects**, **26 agents** in total.

Each entry is a self-contained aiSlang project: declarative capability-based model selection, prompts, agents, deterministic compute skills, RAG knowledge, MCP tools, evals, and a deploy target — all reconstructed straight from the `.ais` / `aislang.toml` source.

## Index

| Agent / Project | Kind | Agents | Skills | Highlights |
|---|---|:-:|:-:|---|
| [**orchestrator-agent**](#orchestrator-agent) | multi-module | 4 | 1 | 🤝 multi-agent 📚 RAG 🛠 skills ➡️ pipeline 🔁 reflector loop 🐳 compose ✅ evals |
| [**multi-module-demo**](#multi-module-demo) | multi-module | 2 | 0 | 🤝 multi-agent 🔌 MCP tools |
| [**patterns-router-demo**](#patterns-router-demo) | single-file | 4 | 0 | 🤝 multi-agent 🔌 MCP tools ➡️ pipeline |
| [**patterns-orchestrator-demo**](#patterns-orchestrator-demo) | single-file | 2 | 0 | 🤝 multi-agent 🔌 MCP tools ➡️ pipeline 🐳 compose |
| [**portfolio-pnl**](#portfolio-pnl) | single-file | 2 | 3 | 🤝 multi-agent 📚 RAG 🛠 skills ➡️ pipeline 🐳 compose |
| [**qwen-smoke**](#qwen-smoke) | single-file | 2 | 0 | 🤝 multi-agent 🐳 compose |
| [**template-demo**](#template-demo) | single-file | 2 | 0 | 🤝 multi-agent 🐳 compose |
| [**comment-clamper-agent**](#comment-clamper-agent) | single-file | 1 | 0 | ➡️ pipeline 🐳 compose |
| [**db-agent**](#db-agent) | single-file | 1 | 0 | 📚 RAG ➡️ pipeline 🐳 compose ✅ evals |
| [**e2e-stack**](#e2e-stack) | single-file | 1 | 0 | ☸️ kubernetes |
| [**free-local**](#free-local) | single-file | 1 | 0 | 🐳 compose |
| [**rag-demo**](#rag-demo) | single-file | 1 | 0 | 📚 RAG 🐳 compose |
| [**support-agent (paid)**](#support-agent-paid) | single-file | 1 | 0 | 🔌 MCP tools 🐳 compose ✅ evals |
| [**support-agent**](#support-agent) | single-file | 1 | 0 | 🔌 MCP tools 🐳 compose ✅ evals |
| [**support-agent-k8s**](#support-agent-k8s) | single-file | 1 | 0 | 🔌 MCP tools ☸️ kubernetes ✅ evals |
| [**skill-demo**](#skill-demo) | single-file | 0 | 5 | 🛠 skills 🔌 MCP tools |

---

<a id="orchestrator-agent"></a>
## orchestrator-agent

*multi-module* · v0.1.0 · 🤝 multi-agent 📚 RAG 🛠 skills ➡️ pipeline 🔁 reflector loop 🐳 compose ✅ evals

v0.3 Week 20 Mon — Orchestrator agent (multi-module project).

**Agents**

| Agent | Module | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|---|
| **architect** | `architect` | `architect_brain` | `architect_sys` | — | `architect_kb` |
| **code_reviewer** | `code_reviewer` | `reviewer_brain` | `reviewer_sys` | — | `reviewer_kb` |
| **coder** | `coder` | `coder_brain` | `coder_sys` | — | `coder_kb` |
| **tester** | `tester` | `tester_brain` | `tester_sys` | — | `tester_kb` |

**Skills** (deterministic compute — the model never does this work)

| Skill | Kind | Command / server | Output |
|---|---|---|---|
| **run_tests** | exec | `cargo test --workspace --no-fail-fast` | `string` |

**Composition**

- **`review_once`**: `code_reviewer`
  - in: Coder-emitted Patch + Test coverage markdown; out: ACCEPT or specific revision feedback
- **`from_locked_design`**: `coder`
  - in: 5-section markdown design (Architect's output); out: fenced Rust patch + test coverage bullets
- **`full_design`**: `use architect.architect` → `use coder.coder` → `use code_reviewer.code_reviewer` → `use tester.tester`
  - in: natural-language feature spec for a new resource block; out: Tester's Test plan + cargo test invocation + ACCEPT-or-FAIL verdict
- **`coder_review_cycle`**: `use coder.coder` → `use code_reviewer.code_reviewer`
  - in: Architect's 5-section design markdown; out: Reviewer's verdict (ACCEPT or revision feedback)
- **`test_once`**: `tester`
  - in: Coder's Patch + Test coverage + Reviewer's verdict; out: Test plan + per-skill recommended invocations
- **`review_cycle`** (loop): body `coder_review_cycle`, until `{{ output \| startswith('ACCEPT') \| not }}`, max 3

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **architect_brain** | `chat` | `anthropic.claude_haiku_4_5` | `ollama.qwen3_8b` |
| **reviewer_brain** | `chat`, `reasoning` | `anthropic.claude_haiku_4_5` | `ollama.qwen3_8b` |
| **coder_brain** | `chat` | `anthropic.claude_haiku_4_5` | `ollama.qwen2_5_coder_7b` |
| **tester_brain** | `chat`, `reasoning` | `anthropic.claude_haiku_4_5` | `ollama.qwen3_8b` |

**RAG knowledge**

- Vector store(s): `architect_kb` (`./knowledge/`), `reviewer_kb` (`./knowledge/`), `coder_kb` (`./knowledge/`), `tester_kb` (`./knowledge/`)

**Tooling & delivery**

- Evals: `structured_output` (f1 ≥ 80%), `verdict_shape` (f1 ≥ 80%), `rust_patch_format` (f1 ≥ 80%), `test_plan_shape` (f1 ≥ 80%)
- Deploy: `compose`, `compose`, `compose`, `compose`

**Source & docs**

- Source: [`examples/agents/orchestrator/aislang.toml`](https://github.com/aiSlang/cli/blob/main/examples/agents/orchestrator/aislang.toml) (dir: [`examples/agents/orchestrator`](https://github.com/aiSlang/cli/blob/main/examples/agents/orchestrator))
- Modules: `architect` (architect), `code_reviewer` (code_reviewer), `coder` (coder), `orchestrator`, `tester` (tester)

---

<a id="multi-module-demo"></a>
## multi-module-demo

*multi-module* · v0.1.0 · 🤝 multi-agent 🔌 MCP tools

v0.3 Week 16 Mon scaffold — see the sibling `aislang.toml`.

**Agents**

| Agent | Module | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|---|
| **architect** | `architect` | `thinker` | `system` | — | — |
| **coder** | `coder` | `thinker` | `system` | `docs` | — |

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **thinker** | `chat` | `anthropic.claude_haiku_4_5` | `ollama.llama_3_1_8b` |

**RAG knowledge**

- Corpus: `README.md`, `company-info.md`

**Tooling & delivery**

- MCP tools: `docs` → `docker.io/mcp/filesystem:1.0.2`, `docs` → `docker.io/mcp/filesystem:1.0.2`

**Source & docs**

- Source: [`examples/multi-module-demo/aislang.toml`](https://github.com/aiSlang/cli/blob/main/examples/multi-module-demo/aislang.toml) (dir: [`examples/multi-module-demo`](https://github.com/aiSlang/cli/blob/main/examples/multi-module-demo))
- Modules: `architect` (architect), `coder` (coder)

---

<a id="patterns-router-demo"></a>
## patterns-router-demo

*single-file* · 🤝 multi-agent 🔌 MCP tools ➡️ pipeline

v0.3 Week 17 Fri — worked router-pattern demo.

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **classifier** | `thinker` | `classifier_sys` | — | — |
| **billing** | `thinker` | `billing_sys` | — | — |
| **technical** | `thinker` | `technical_sys` | — | — |
| **general** | `thinker` | `general_sys` | — | — |

**Composition**

- **`handle_billing`**: `billing`
  - in: the user's billing question; out: a 1-3 sentence billing reply
- **`handle_technical`**: `technical`
  - in: the user's technical question; out: a 2-4 sentence technical reply
- **`handle_general`**: `general`
  - in: the user's general/greeting question; out: a 1-2 sentence conversational reply

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **thinker** | `chat` | `anthropic.claude_haiku_4_5` | `ollama.llama_3_1_8b` |

**RAG knowledge**

- Corpus: `README.md`, `company-info.md`

**Tooling & delivery**

- MCP tools: `docs` → `docker.io/mcp/filesystem:1.0.2`
- Prompts: `billing_agent.md`, `general_agent.md`, `router_classifier.md`, `technical_agent.md`

**Source & docs**

- Source: [`examples/patterns/router-demo/router-demo.ais`](https://github.com/aiSlang/cli/blob/main/examples/patterns/router-demo/router-demo.ais) (dir: [`examples/patterns/router-demo`](https://github.com/aiSlang/cli/blob/main/examples/patterns/router-demo))

---

<a id="patterns-orchestrator-demo"></a>
## patterns-orchestrator-demo

*single-file* · 🤝 multi-agent 🔌 MCP tools ➡️ pipeline 🐳 compose

v0.3 Week 17 Thu — worked orchestrator-pattern demo.

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **researcher** | `thinker` | `researcher_sys` | `docs` | — |
| **summarizer** | `thinker` | `summarizer_sys` | — | — |

**Composition**

- **`research_then_summarize`**: `researcher` → `summarizer`
  - in: the user's question — e.g. 'Where is Acme HQ?'; out: one-sentence answer drawn from the knowledge base

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **thinker** | `chat` | `anthropic.claude_haiku_4_5` | `ollama.llama_3_1_8b` |

**RAG knowledge**

- Corpus: `README.md`, `company-info.md`

**Tooling & delivery**

- MCP tools: `docs` → `docker.io/mcp/filesystem:1.0.2`
- Deploy: `compose`
- Prompts: `researcher.md`, `summarizer.md`

**Source & docs**

- Source: [`examples/patterns/orchestrator-demo/orchestrator-demo.ais`](https://github.com/aiSlang/cli/blob/main/examples/patterns/orchestrator-demo/orchestrator-demo.ais) (dir: [`examples/patterns/orchestrator-demo`](https://github.com/aiSlang/cli/blob/main/examples/patterns/orchestrator-demo))

---

<a id="portfolio-pnl"></a>
## portfolio-pnl

*single-file* · 🤝 multi-agent 📚 RAG 🛠 skills ➡️ pipeline 🐳 compose

A local-first, multi-agent portfolio analysis app built on [aiSlang](../../README.md). It helps one person understand a personal investment portfolio and make deliberate re-balancing decisions — and it runs **entirely on your own machine**.

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **strategic** | `reasoner` | `strategic_sys` | — | `strategy_kb` |
| **orchestrator** | `reasoner` | `orchestrator_sys` | — | `strategy_kb` |

**Skills** (deterministic compute — the model never does this work)

| Skill | Kind | Command / server | Output |
|---|---|---|---|
| **fetch_prices** | exec | `python3 ../../../scripts/fetch.py` | `json` |
| **analyze** | exec | `python3 ../../../scripts/analyze.py --persist` | `json` |
| **visualize** | exec | `python3 ../../../scripts/render_dashboard.py` | `string` |

**Composition**

- **`advise`**: `strategic` → `orchestrator`
  - in: the analyze JSON (analytic + strategic metric blocks); out: a keep/rebalance recommendation with rationale

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **reasoner** | `chat`, `reasoning` | `anthropic.claude_haiku_4_5` | `ollama.qwen3_8b` |

**RAG knowledge**

- Vector store(s): `strategy_kb` (`./knowledge/`)
- Corpus: `01_antifragility.md`, `02_barbell.md`, `03_fragility_detection.md`, `04_rebalancing.md`, `05_reading_the_metrics.md`

**Tooling & delivery**

- Deploy: `compose`
- Prompts: `orchestrator.md`, `strategic.md`

**Source & docs**

- Source: [`examples/portfolio-pnl/portfolio.ais`](https://github.com/aiSlang/cli/blob/main/examples/portfolio-pnl/portfolio.ais) (dir: [`examples/portfolio-pnl`](https://github.com/aiSlang/cli/blob/main/examples/portfolio-pnl))
- README: [`examples/portfolio-pnl/README.md`](https://github.com/aiSlang/cli/blob/main/examples/portfolio-pnl/README.md)
- DESIGN: [`examples/portfolio-pnl/DESIGN.md`](https://github.com/aiSlang/cli/blob/main/examples/portfolio-pnl/DESIGN.md)

---

<a id="qwen-smoke"></a>
## qwen-smoke

*single-file* · 🤝 multi-agent 🐳 compose

v0.3 Week 18 Fri — worked Qwen-on-Ollama smoke demo.

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **thinker** | `qwen3` | `qwen3_sys` | — | — |
| **coder** | `qwen2_5_coder` | `coder_sys` | — | — |

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **qwen3** | `chat` | `ollama.qwen3_8b` | — |
| **qwen2_5_coder** | `chat` | `ollama.qwen2_5_coder_7b` | — |

**Tooling & delivery**

- Deploy: `compose`
- Prompts: `coder_sys.md`, `qwen3_sys.md`

**Source & docs**

- Source: [`examples/qwen-smoke/qwen-smoke.ais`](https://github.com/aiSlang/cli/blob/main/examples/qwen-smoke/qwen-smoke.ais) (dir: [`examples/qwen-smoke`](https://github.com/aiSlang/cli/blob/main/examples/qwen-smoke))

---

<a id="template-demo"></a>
## template-demo

*single-file* · 🤝 multi-agent 🐳 compose

v0.3 Week 18 Tue — worked dynamic-prompts demo.

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **static_explainer** | `thinker` | `static_sys` | — | — |
| **dynamic_explainer** | `thinker` | `dynamic_sys` | — | — |

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **thinker** | `chat` | `anthropic.claude_haiku_4_5` | `ollama.llama_3_1_8b` |

**Tooling & delivery**

- Deploy: `compose`
- Prompts: `dynamic_sys.md`, `static_sys.md`

**Source & docs**

- Source: [`examples/template-demo/template-demo.ais`](https://github.com/aiSlang/cli/blob/main/examples/template-demo/template-demo.ais) (dir: [`examples/template-demo`](https://github.com/aiSlang/cli/blob/main/examples/template-demo))

---

<a id="comment-clamper-agent"></a>
## comment-clamper-agent

*single-file* · ➡️ pipeline 🐳 compose

v0.4 Week 26 Mon — comment-clamper dogfood agent.

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **comment_clamper** | `clamp_brain` | `clamp_sys` | — | — |

**Composition**

- **`clamp_once`**: `comment_clamper`
  - in: one verbose Rust doc-comment / module header; out: the clamped comment, same prefix, code untouched

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **clamp_brain** | `chat` | `anthropic.claude_haiku_4_5` | `ollama.qwen3_8b` |

**Tooling & delivery**

- Deploy: `compose`
- Prompts: `comment_clamper.md`

**Source & docs**

- Source: [`examples/agents/comment_clamper/comment_clamper.ais`](https://github.com/aiSlang/cli/blob/main/examples/agents/comment_clamper/comment_clamper.ais) (dir: [`examples/agents/comment_clamper`](https://github.com/aiSlang/cli/blob/main/examples/agents/comment_clamper))

---

<a id="db-agent"></a>
## db-agent

*single-file* · 📚 RAG ➡️ pipeline 🐳 compose ✅ evals

v0.3 Week 20 Wed — DB agent.

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **db** | `db_brain` | `db_sys` | — | `db_kb` |

**Composition**

- **`brief_once`**: `db`
  - in: high-level migration goal (string); out: Feature spec (5-section markdown matching Architect's input shape)

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **db_brain** | `chat`, `reasoning` | `anthropic.claude_haiku_4_5` | `ollama.qwen3_8b` |

**RAG knowledge**

- Vector store(s): `db_kb` (`./knowledge/`)
- Corpus: `01_current_schemas.md`, `02_proposed_sqlite_schema.md`, `03_reversibility_contract.md`

**Tooling & delivery**

- Evals: `feature_spec_shape` (f1 ≥ 80%)
- Deploy: `compose`
- Prompts: `db.md`

**Source & docs**

- Source: [`examples/agents/db/db.ais`](https://github.com/aiSlang/cli/blob/main/examples/agents/db/db.ais) (dir: [`examples/agents/db`](https://github.com/aiSlang/cli/blob/main/examples/agents/db))

---

<a id="e2e-stack"></a>
## e2e-stack

*single-file* · ☸️ kubernetes

examples/e2e-stack/main.ais — self-contained K8s stack for the managed-cloud multi-tenant e2e (deploy/eks/40-e2e-two-tenants.sh).

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **support** | `fast` | `system` | — | — |

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **fast** | `chat` | `anthropic.claude_haiku_4_5`, `openai.gpt_4o_mini` | `ollama.llama_3_1_8b` |

**Tooling & delivery**

- Deploy: `kubernetes`
- Prompts: `system.md`

**Source & docs**

- Source: [`examples/e2e-stack/main.ais`](https://github.com/aiSlang/cli/blob/main/examples/e2e-stack/main.ais) (dir: [`examples/e2e-stack`](https://github.com/aiSlang/cli/blob/main/examples/e2e-stack))

---

<a id="free-local"></a>
## free-local

*single-file* · 🐳 compose

examples/free-local.ais — the "no caps, free local model" demo.

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **assistant** | `local` | `system` | — | — |

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **local** | `chat` | `ollama.llama_3_1_8b` | — |

**RAG knowledge**

- Corpus: `README.md`, `company-info.md`

**Tooling & delivery**

- Deploy: `compose`
- Prompts: `system.md`

**Source & docs**

- Source: [`examples/free-local.ais`](https://github.com/aiSlang/cli/blob/main/examples/free-local.ais) (dir: [`examples`](https://github.com/aiSlang/cli/blob/main/examples))

---

<a id="rag-demo"></a>
## rag-demo

*single-file* · 📚 RAG 🐳 compose

v0.3 Week 19 Wed — worked RAG demo end-to-end against bge-m3.

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **doc_helper** | `thinker` | `doc_helper_sys` | — | `aislang_docs` |

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **thinker** | `chat` | `anthropic.claude_haiku_4_5` | `ollama.llama_3_1_8b` |

**RAG knowledge**

- Vector store(s): `aislang_docs` (`./knowledge/`)
- Corpus: `aislang_overview.md`, `composition_primitives.md`, `k8s_deployment.md`, `rag.md`

**Tooling & delivery**

- Deploy: `compose`
- Prompts: `doc_helper.md`

**Source & docs**

- Source: [`examples/rag-demo/rag-demo.ais`](https://github.com/aiSlang/cli/blob/main/examples/rag-demo/rag-demo.ais) (dir: [`examples/rag-demo`](https://github.com/aiSlang/cli/blob/main/examples/rag-demo))

---

<a id="support-agent-paid"></a>
## support-agent (paid)

*single-file* · 🔌 MCP tools 🐳 compose ✅ evals

examples/support-agent-paid.ais — the v0.1 paid-tier demo variant.

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **support** | `fast` | `system` | `docs` | — |

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **fast** | `chat`, `tool_use` | `anthropic.claude_haiku_4_5` | `openai.gpt_4o_mini` |

**RAG knowledge**

- Corpus: `README.md`, `company-info.md`

**Tooling & delivery**

- MCP tools: `docs` → `docker.io/mcp/filesystem:1.0.2`
- Evals: `factual_accuracy` (f1 ≥ 75%)
- Deploy: `compose`
- Prompts: `system.md`

**Source & docs**

- Source: [`examples/support-agent-paid.ais`](https://github.com/aiSlang/cli/blob/main/examples/support-agent-paid.ais) (dir: [`examples`](https://github.com/aiSlang/cli/blob/main/examples))

---

<a id="support-agent"></a>
## support-agent

*single-file* · 🔌 MCP tools 🐳 compose ✅ evals

examples/support-agent.ais — the v0.1 canonical reference example.

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **support** | `fast` | `system` | `docs` | — |

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **fast** | `chat`, `tool_use` | `anthropic.claude_haiku_4_5`, `openai.gpt_4o_mini` | `ollama.llama_3_1_8b` |

**RAG knowledge**

- Corpus: `README.md`, `company-info.md`

**Tooling & delivery**

- MCP tools: `docs` → `docker.io/mcp/filesystem:1.0.2`
- Evals: `factual_accuracy` (f1 ≥ 50%)
- Deploy: `compose`
- Prompts: `system.md`

**Source & docs**

- Source: [`examples/support-agent.ais`](https://github.com/aiSlang/cli/blob/main/examples/support-agent.ais) (dir: [`examples`](https://github.com/aiSlang/cli/blob/main/examples))

---

<a id="support-agent-k8s"></a>
## support-agent-k8s

*single-file* · 🔌 MCP tools ☸️ kubernetes ✅ evals

examples/support-agent-k8s.ais — Phase 2 Week 14 Tue.

**Agents**

| Agent | Model | Prompt | Tools | Knowledge |
|---|---|---|---|---|
| **support** | `fast` | `system` | `docs` | — |

**Models**

| Model | Requires | Prefer | Fallback |
|---|---|---|---|
| **fast** | `chat`, `tool_use` | `anthropic.claude_haiku_4_5`, `openai.gpt_4o_mini` | `ollama.llama_3_1_8b` |

**RAG knowledge**

- Corpus: `README.md`, `company-info.md`

**Tooling & delivery**

- MCP tools: `docs` → `docker.io/mcp/filesystem:1.0.2`
- Evals: `factual_accuracy` (f1 ≥ 50%)
- Deploy: `kubernetes`
- Prompts: `system.md`

**Source & docs**

- Source: [`examples/support-agent-k8s.ais`](https://github.com/aiSlang/cli/blob/main/examples/support-agent-k8s.ais) (dir: [`examples`](https://github.com/aiSlang/cli/blob/main/examples))

---

<a id="skill-demo"></a>
## skill-demo

*single-file* · 🛠 skills 🔌 MCP tools

v0.3 Week 18 Wed-Thu — worked `skill { … }` block demo.

**Skills** (deterministic compute — the model never does this work)

| Skill | Kind | Command / server | Output |
|---|---|---|---|
| **echo_greeting** | exec | `echo 'hello from the aislang skill runtime'` | `string` |
| **describe_self** | exec | `echo eyJydW50aW1lIjoiYWlzbGFuZyIsIm1vZGUiOiJzaGVsbCJ9 | base64 -d` | `json` |
| **list_cwd_files** | exec | `ls -1` | `list_string` |
| **evil_sudo** | exec | `sudo cat /etc/shadow` | `string` |
| **read_notes** | mcp | `docs.tool.read_text_file` | `string` |

**RAG knowledge**

- Corpus: `notes.md`

**Tooling & delivery**

- MCP tools: `docs` → `docker.io/mcp/filesystem:1.0.2`

**Source & docs**

- Source: [`examples/skill-demo/skill-demo.ais`](https://github.com/aiSlang/cli/blob/main/examples/skill-demo/skill-demo.ais) (dir: [`examples/skill-demo`](https://github.com/aiSlang/cli/blob/main/examples/skill-demo))

---

_Built by `tools/catalog` — the source of truth for docs.aislang.org's agents catalog page._

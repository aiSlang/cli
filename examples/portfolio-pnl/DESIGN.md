# portfolio-pnl — Design

A local-first, multi-agent portfolio analysis app built on aiSlang. It helps
one person understand a personal investment portfolio and make deliberate
re-balancing decisions — **entirely on your own machine**. Your holdings never
leave your computer: prices are fetched from public sources, but the composition
of your portfolio (what you own, at what cost, since when) stays local.

The system is a pipeline of specialized agents that fetch prices, compute P&L
and risk, evaluate anti-fragility (in the Nassim Taleb sense), render a
dashboard, and finally present a **keep-or-rebalance recommendation**. That
recommendation is decided **deterministically, by a rule engine** — the same
metrics always yield the same verdict and the same sized actions; the language
model only *explains* it in plain prose (decision 4). The system recommends; it
never trades. A human stays in the loop for every decision.

---

## Table of contents

1. [Overview](#overview)
2. [The agent pipeline](#the-agent-pipeline)
3. [Data flow](#data-flow)
4. [Four key architecture decisions](#four-key-architecture-decisions)
5. [Persistence](#persistence)
6. [Inter-agent JSON contracts](#inter-agent-json-contracts)
7. [Metrics catalog](#metrics-catalog)
8. [Visualization](#visualization)
9. [Defaults and assumptions](#defaults-and-assumptions)
10. [Build plan and status](#build-plan-and-status)

---

## Overview

The app is expressed as an aiSlang **orchestrator** project: a `main` entry
point that plans the task and dispatches to a sequence of deployed agents, with
deterministic Python **compute skills** doing all the arithmetic and database
writes. The agents decide *what* to compute and *interpret* the results; the
math itself is never left to the language model.

Two stores back the system, each chosen for what it is good at:

- **SQLite** holds the structured, numeric side — instruments, the price
  time-series, and every computed metric snapshot. Exact lookups, time-range
  queries, and aggregation are what this data needs.
- **A vector store** holds only the *unstructured* strategy knowledge corpus
  under `knowledge/` (Taleb principles, re-balancing heuristics, and the
  owner's own investment thesis), which the Strategic and Orchestrator agents
  retrieve from to ground their reasoning.

Holdings are declared in a structured `holdings.json` config — symbols, lots
(quantity + cost basis + acquisition date), asset class, and price source —
rather than being baked into any system prompt.

---

## The agent pipeline

The orchestrator runs four agents in sequence and then synthesizes their
outputs.

| Agent | Role |
|---|---|
| **Fetcher** *(skill)* | Pulls fresh prices and writes them to SQLite. Each position selects its adapter via its `source` field. Default key-free sources: **Yahoo Finance** for equities and ETFs, **CoinGecko** for crypto. For symbols the APIs miss, a **Playwright MCP** fallback renders the quote from a **TradingView** widget (`source: "tradingview"` + a `tv_symbol` like `NASDAQ:INTC`). *(Stooq was the original default; its public endpoint now sits behind a browser challenge, so it's kept only as a legacy adapter.)* |
| **Analytic** | Computes P&L, returns, and risk metrics from holdings + prices; persists per-position and portfolio snapshots to SQLite. |
| **Strategic** | Computes anti-fragility metrics (convexity / stress asymmetry, barbell structure, tail shape) and interprets them. Grounded by the RAG knowledge corpus under `knowledge/`. |
| **Visualizer** | Renders a single self-contained `dashboard.html` with a top-to-detail drill-down. |
| **Orchestrator** | Reads the **deterministic** keep / rebalance recommendation (decided by the `recommend()` rule engine over the Analytic + Strategic metrics) and emits a clear, RAG-grounded **explanation** of it as JSON. That JSON is merged back onto the canonical row (the engine keeps the verdict / actions / sizes; the LLM contributes only the prose). It does not decide the verdict. |

The recommendation is advisory and rule-decided. It never places or simulates a
live trade — the owner reviews the suggested actions and decides.

### Skills vs. agents

In aiSlang an *agent* is a language-model node whose tools are MCP servers; a
*skill* is a standalone deterministic unit. So the deterministic stages here —
**Fetcher**, **Analytic**, and **Visualizer** — are aiSlang **skills** (the
`exec` Python under `scripts/`, wired in `portfolio.ais`), not language-model
agents: there is nothing for a model to decide when fetching a quote or computing
a Sharpe ratio, and keeping them as skills is what guarantees the arithmetic is
correct (decision 2). The **Strategic** and **Orchestrator** stages are the
actual **LLM agents**, because that is where RAG-grounded *interpretation* lives —
but note the keep/rebalance **decision itself is also code** (the `recommend()`
rule engine, decision 4): the agents explain that fixed recommendation, they do
not make it. *(A later increment can package the skills as an MCP tool server so a
single agent calls them in a tool-use loop; skills-plus-agents is the local-first
v1.)*

---

## Data flow

```
                       holdings.json (symbols, lots, asset_class, source)
                             │
                             ▼
          knowledge/    ┌──────────┐   Yahoo / CoinGecko / Playwright MCP
          (RAG corpus)  │ Fetcher  │◄──────── public price sources
               │        └────┬─────┘
               │             │ writes prices
               │             ▼
               │        ┌──────────────────────────────────────────┐
               │        │                SQLite                     │
               │        │  instruments · lots · prices · snapshots  │
               │        │  position_metrics · risk_metrics          │
               │        │  fragility_metrics · recommendations      │
               │        └──────────────────────────────────────────┘
               │             ▲            ▲            ▲        ▲
               │             │ writes     │ writes     │ writes │ reads
               │        ┌────┴─────┐ ┌────┴──────┐ ┌───┴──────┐ │
               │        │ Analytic │ │ Strategic │ │ recommend│ │
               │        │ (skill)  │ │ (skill)   │ │ () engine│ │
               │        └────┬─────┘ └────┬──────┘ └───┬──────┘ │
               │             │            │            │        │
               │  analytic_output  strategic_output  recommendation
               │             │            │            │ (deterministic verdict)
               │             ▼            ▼            ▼        │
               │        ┌────────────┐              ┌──────────┴─┐
               │        │ Visualizer │              │Orchestrator│  reads the fixed
               │        │  (skill)   │              │  + Strateg.│  recommendation,
               │        └─────┬──────┘              │(LLM agents)│  RAG-grounded, and
               │              │                     └─────┬──────┘  emits a JSON
               ▼              ▼                           ▼         EXPLANATION,
          knowledge/    dashboard.html            JSON explanation  merged back onto
          (RAG corpus)  (L1 → L4 drill-down)      (persisted to DB) the canonical row
```

The keep/rebalance **verdict, actions, and sizes are decided by the deterministic
`recommend()` engine** (in `scripts/portfolio.py`), run inside `analyze.py`, which
persists them to the `recommendations` table and feeds them to the dashboard. The
SQLite database is the shared source of truth. The Strategic and Orchestrator LLM
agents read that fixed recommendation and the metrics and only *explain* it — they
pass each other compact summaries, never full datasets, and never change the call.

---

## Four key architecture decisions

### 1. SQLite for prices and metrics; the vector store is only for knowledge

Prices and computed metrics are structured numeric time-series. They need exact
lookups (the price of `AAPL` on a given date), time-range queries (the last
90 daily bars), and aggregation (portfolio value across positions at a
timestamp). A relational store answers all of these directly and exactly.

A vector database is the **wrong** store for prices — nearest-neighbour
similarity search over embeddings cannot answer "what was the closing price on
2024-06-03", and it introduces approximation where the whole point is exactness.
So the vector store is used **only** for the unstructured strategy *knowledge*
corpus (Taleb's principles, re-balancing heuristics, the owner's thesis) that
the Strategic and Orchestrator agents retrieve from. Numbers go in SQLite;
prose goes in the vector store.

### 2. The language model never does the arithmetic

Language models are unreliable calculators. Every P&L, risk, and fragility
computation — and every database write — is performed by **deterministic Python
compute skills** (in `scripts/`, invoked as aiSlang `exec` skills). The agents
choose *which* computations to run and *interpret* the results in context, but
the numbers themselves come from code that is testable and reproducible. This
keeps the financial output correct and auditable, and keeps the agents focused
on judgement rather than arithmetic.

### 3. Holdings live in a structured config, not in prompts

The portfolio is described in `holdings.json`: each position carries a `symbol`,
a list of `lots` (each with `quantity`, `cost_basis`, and `date`), an
`asset_class`, a `currency`, and a price `source`. Keeping this in a structured
config — rather than embedding it in a system prompt — means the compute skills
can parse it exactly, cost basis can be tracked per lot for FIFO accounting,
and updating a position is a data edit rather than a prompt rewrite.

### 4. The recommendation is decided by a rule engine; the LLM only explains it

Extending decision 2 from *arithmetic* to the *decision itself*: the
keep/rebalance verdict, the specific actions, and their sizes are produced by a
deterministic rule engine — `portfolio.recommend(analytic, strategic, buckets)`
in `scripts/portfolio.py` — a pure function with tunable thresholds (`REC_RULES`:
single-name concentration cap, fragile-middle target, minimum safe / maximum
aggressive sleeve, and a fragility-score trip). The same metrics in always yield
the same recommendation out; it is unit-tested and reproducible.

This was a direct fix for observed **non-determinism**: when the language model
made the call, identical inputs drifted between `rebalance` (~80% of runs) and
`keep` (~20%) over a 10–15-minute window, and it occasionally invented sizes
(e.g. quoting a dollar figure for what was a percentage-of-portfolio move).
aiSlang exposes no temperature knob, so LLM prose will always vary — which is
exactly why the *decision* must be code. `analyze.py` runs the engine, persists
the result to the `recommendations` table, and the dashboard renders it with no
model in the loop. The **Strategic** and **Orchestrator** agents then read that
fixed recommendation and produce an owner-facing *explanation* of it, grounded in
the Taleb corpus — the Orchestrator emits it as JSON (downstream-consumable),
which `persist_recommendation.py` merges back onto the canonical row: the engine
keeps the verdict, actions, and sizes; the LLM contributes only the prose
(summary, per-action rationale, explanation). The agents never re-derive,
re-order, add, drop, or resize an action. Judgement-flavoured prose stays with
the LLM; the number that matters stays deterministic — so it is safe to *apply*
the persisted actions to a virtual portfolio downstream.

---

## Persistence

The SQLite schema (`db/schema.sql`) defines the structured store. All timestamps
are ISO-8601 UTC strings (`YYYY-MM-DD` for daily bars, full datetime for run
stamps) so they sort lexically. `journal_mode = WAL` and `foreign_keys = ON` are
set at the top.

| Table | Holds |
|---|---|
| `instruments` | Every instrument held or referenced, including the benchmark: `symbol`, `name`, `asset_class`, `currency`, `source`, plus a `meta_json`. |
| `lots` | One row per cost-basis lot from `holdings.json` — `quantity`, `cost_basis` (per-unit), `lot_date`. One row per lot enables FIFO accounting. |
| `prices` | The price time-series, keyed by `(symbol, ts)`. This table is the concrete reason for using SQL. |
| `snapshots` | Portfolio-level valuation over time: `total_value`, `total_cost`, `cash`, `unrealized_pnl`, `realized_pnl`, `pnl_pct`, keyed by `ts`. |
| `position_metrics` | Per-position metrics at a snapshot: `quantity`, `price`, `value`, `cost`, `weight`, `unrealized_pnl`, `unrealized_pct`, `contrib_to_risk`, keyed by `(ts, symbol)`. |
| `risk_metrics` | Portfolio risk at a snapshot: `vol_ann`, `sharpe`, `sortino`, `max_drawdown`, `var95`, `cvar95`, `hhi`, `eff_n`, `beta`, keyed by `ts`. |
| `fragility_metrics` | Anti-fragility metrics at a snapshot: `convexity`, `stress_asymmetry`, `fragility_score`, `classification`, the three barbell weights, `skew`, `excess_kurtosis`, `tail_ratio`, `es95`, and a `details_json` (convexity curve, per-shock stress), keyed by `ts`. |
| `recommendations` | The deterministic `recommend()` engine's output (written by `analyze.py`, not the LLM): `verdict` (`keep` / `rebalance`), `summary`, `actions_json`, `grounded_refs`, keyed by `ts`. |

Two indexes support the common access patterns: `idx_prices_symbol_ts` on
`prices(symbol, ts)` and `idx_posmetrics_ts` on `position_metrics(ts)`.

### Holdings ↔ DB sync

`holdings.json` is the source of truth for *what you own*; the `instruments` and
`lots` tables are a **re-projection** of it, not a second copy you edit by hand.
`db.sync_holdings()` runs at the top of every analyze/agent run and, idempotently,
UPSERTs `instruments` and does `DELETE FROM lots; INSERT` the file's lots — so the
DB is always in sync with the file:

- **Add an instrument** → append a position → next run it appears (the Fetcher
  starts pulling its price; history-based risk metrics degrade gracefully until
  enough bars accrue).
- **Buy more** → append a lot `{quantity, cost_basis, date}` to that position →
  next run re-syncs, with FIFO lot order preserved.
- `prices` are independent (upserted by the Fetcher, keyed by `(symbol, ts)`), so
  changing holdings never disturbs price history — historical bars are kept even
  after you exit a position.

Each `lots` entry is one **acquisition event** (quantity + per-unit cost + date);
several buys of the same symbol = several lots. This models **open** positions.
Sells / **realized** P&L are not auto-recorded: on a sale you trim the lots (FIFO)
and realized gain is computed on demand via `portfolio.realized_pnl()`. The clean
upgrade for a full audit trail is an append-only **transactions log** (buys/sells)
from which both open lots and realized P&L are *derived* — deferred to a later
increment; v1 keeps `holdings.json` as the current open positions.

---

## Inter-agent JSON contracts

Agents hand off to one another using aiSlang's **JSON inter-agent contract** —
schema-validated tool-call output. There is one schema per handoff, stored under
`_schemas/`:

| Schema | Producer → consumer | Carries |
|---|---|---|
| `fetch_report` | Fetcher → Analytic | Which symbols were priced, from which sources, freshness / any failures. |
| `analytic_output` | Analytic → Strategic / engine | Compact P&L and risk summary for the run. |
| `strategic_output` | Strategic → Orchestrator / engine | Compact anti-fragility summary and interpretation. |
| `recommendation` | `recommend()` engine → owner | The `keep` / `rebalance` verdict, summary, sized actions, and the rule flags that fired — decided deterministically; the Orchestrator explains it, never changes it. |

Because the SQLite database is the shared source of truth, these payloads are
**compact summaries**, not full datasets. An agent that needs detail reads it
from SQLite; the JSON handoff only carries what the next agent needs to make its
decision, keeping the contracts small and the token cost low.

---

## Metrics catalog

All of the following are computed by deterministic Python skills and persisted to
the tables noted above. Formulas are sketched briefly.

### P&L (`snapshots`, `position_metrics`)

- **Position value** — `quantity × current price`.
- **Cost basis (FIFO)** — lots consumed oldest-first; per-position cost is the
  sum of remaining lot `quantity × cost_basis`.
- **Unrealized P&L** — `value − cost`, reported in absolute terms and as a
  percentage (`unrealized_pct`).
- **Realized P&L** — booked against FIFO lots when a position is reduced.
- **Portfolio total** — sum of position values plus `cash`.
- **Period / time-weighted return** — return over a window, chaining
  sub-period returns to neutralize the effect of cash flows.
- **Per-position contribution** — each position's share of total portfolio P&L.

### Risk (`risk_metrics`)

- **Annualized volatility** (`vol_ann`) — standard deviation of portfolio
  returns scaled by √(periods per year).
- **Max drawdown** (`max_drawdown`) — most negative peak-to-trough decline, as
  a fraction (e.g. `-0.23`).
- **Historical VaR(95)** (`var95`) — the 5th-percentile 1-day return (negative).
- **CVaR(95) / expected shortfall** (`cvar95`) — the mean return in the tail
  beyond VaR(95).
- **Sharpe** (`sharpe`) — excess return over risk-free, per unit of volatility.
- **Sortino** (`sortino`) — like Sharpe, but penalizes only downside deviation.
- **Beta vs benchmark** (`beta`) — sensitivity of portfolio returns to the
  benchmark (SPY): `cov(port, bench) / var(bench)`.
- **Correlation matrix** — pairwise return correlations across positions.
- **Concentration** — Herfindahl index `hhi` = Σ(weightᵢ²), and effective number
  of positions `eff_n` = `1 / hhi`.

### Anti-fragility — Taleb (`fragility_metrics`)

- **Convexity** (`convexity`) — payoff curvature under a symmetric price shock.
  Note: this is approximately **zero for a purely linear, long-only stock book**
  — real convexity comes from optionality. For a linear book, the anti-fragility
  read therefore rests on the items below rather than on convexity itself.
- **Stress asymmetry** (`stress_asymmetry`) — `(loss under −shock) + (gain under
  +shock)`. A value below zero means the left tail is worse than the right —
  concave, i.e. fragile.
- **Barbell adherence** — the weight fractions in three sleeves: `barbell_safe`
  (safe), `barbell_mid` (the fragile middle), and `barbell_aggr`
  (aggressive / convex). Bucket membership comes from `barbell_buckets` in
  `holdings.json`; asset classes not listed as safe or aggressive fall in the
  middle.
- **Tail shape** — `skew`, `excess_kurtosis`, `tail_ratio`
  (`|p95 return| / |p05 return|`), and expected shortfall `es95` (the same value
  as `cvar95`, kept here for the fragility view).
- **Concentration** — carried over from the risk metrics (HHI, effective N).
- **Composite fragility score** (`fragility_score`) — a `0..100` roll-up where
  higher means more fragile — plus a `classification` of **fragile**,
  **robust**, or **antifragile**.

### Recommendation (`recommendations`) — the rule engine

`portfolio.recommend()` turns the metrics above into a keep/rebalance call
deterministically. The tunable thresholds live in `REC_RULES`:

- **`concentration_cap`** (0.25) — a single *middle-sleeve* name above this
  weight is trimmed back toward it. (Applied only to the fragile middle; a large
  *safe* sleeve is exactly what the barbell wants, so the cap does not touch it.)
- **`target_mid_max`** (0.40) — a fragile middle heavier than this is trimmed
  proportionally across its names.
- **`min_safe` / `max_aggr`** (0.25 / 0.35) — sleeve guardrails; an aggressive
  sleeve above `max_aggr` gets its biggest name trimmed, and freed weight rotates
  to `CASH` (the safe sleeve).
- **`fragility_rebalance`** (55.0) — a `fragility_score` at or above this trips a
  `rebalance` on its own.

The verdict is `rebalance` when a material sized action fires or the fragility
trip hits, else `keep` (with an empty `actions` list). Each action's `size` is a
change in **portfolio weight** (percentage points, plus the from→to weights),
never a dollar amount — the app converts weight to dollars deterministically.

---

## Visualization

The Visualizer renders a single self-contained `dashboard.html` with a
**top-to-detail drill-down**:

- **L1 — Overview.** Total portfolio value and P&L, a value sparkline, an
  allocation donut, a health scorecard (Sharpe, max drawdown, fragility score,
  and a barbell-adherence gauge), and top movers.
- **L2 — Positions table.** Each position's weight, value, cost, P&L %, and
  contribution-to-risk.
- **L3 — Per-instrument view.** The price time-series for a single instrument,
  overlaid with its cost-basis line.
- **L4 — Risk / anti-fragility.** The convexity curve, the barbell bar,
  a correlation heatmap, a drawdown chart, and a return distribution with the
  tails shaded.

The output is a single HTML file with everything inline, so it can be opened
locally without a server and without any external asset fetch.

---

## Defaults and assumptions

The first version makes deliberately narrow choices so the pipeline is simple
and reliable:

| Area | Default (v1) |
|---|---|
| Asset classes | Equities, ETFs, and crypto (no options yet). |
| Price sources | Yahoo Finance (equities / ETFs) and CoinGecko (crypto) — both key-free; a Playwright MCP fallback that renders a TradingView widget (`source: "tradingview"` + `tv_symbol`) for symbols the APIs miss. |
| Base currency | USD. |
| Cost basis | FIFO. |
| Benchmark | SPY. |
| Runtime | Local-first — runs via `aislang chat`, compute via Python `exec` skills. |
| Cadence | On-demand runs. |

Explicitly deferred to a later version:

- Options and other derivatives (and the real convexity they bring).
- A deployable MCP-server compute layer (v1 runs compute as local `exec`
  skills).
- Scheduled / automated runs (v1 is on-demand).

---

## Build plan and status

The project is being built in increments. The first three are landing now; the
agents and their supporting artifacts come next.

| # | Increment | Delivers | Status |
|---|---|---|---|
| 0 | Scaffolding | Project layout, config stubs, `.env.example`. | **Done** |
| 1 | SQLite schema + `db.py` | `db/schema.sql` and the database access layer. | **Done** |
| 2 | Python compute core + tests | `portfolio.py` (P&L / risk / fragility math) plus `tests/`. | **Done** |
| 3 | Fetcher (skill) | `fetch.py` (Yahoo / CoinGecko) + `fetch_report` schema, wired as an aiSlang skill. | **Done** |
| 4 | Analytic (skill) | `analyze.py` compute + snapshots; `analytic_output` + `strategic_output` schemas. | **Done** |
| 5 | Strategic agent + RAG | LLM agent + `knowledge/` corpus (Taleb) grounding the anti-fragility read. | **Done** |
| 6 | Visualizer (skill) | `render_dashboard.py` -> self-contained `dashboard.html` (L1-L4 drill-down). | **Done** |
| 7 | Orchestrator agent | `advise` pipeline (Strategic -> Orchestrator) + `recommendation` schema + `run.sh`. | **Done** |
| 8 | Evals | Deterministic golden eval for the recommendation engine (`scripts/eval_recommend.py` + `evals/recommend_cases.jsonl`, in pytest) + agent-faithfulness `eval` blocks (`evals/advise.jsonl`, `evals/strategic_reads.jsonl`). | **Done** |
| 9 | Packaging | Final polish and distribution. | Next |

---

## Project layout

```
portfolio-pnl/
├── holdings.json        # the portfolio: symbols, lots, asset classes, sources
├── .env.example         # safe-to-commit env template (no real secrets)
├── db/
│   └── schema.sql       # the SQLite schema
├── scripts/             # deterministic Python compute
│   ├── db.py            # database access layer
│   ├── portfolio.py     # P&L / risk / fragility math
│   └── …                # skill entrypoints
├── tests/               # tests for the compute core
├── knowledge/           # RAG corpus (Taleb, heuristics, the owner's thesis)
├── agents/              # the .ais agent modules
├── _schemas/            # JSON inter-agent contracts
├── orchestrator.ais     # the orchestrator entry point
└── aislang.toml         # module manifest
```

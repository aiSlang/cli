# portfolio-pnl

A local-first, multi-agent portfolio analysis app built on
[aiSlang](../../README.md). It helps one person understand a personal
investment portfolio and make deliberate re-balancing decisions — and it runs
**entirely on your own machine**.

> **Work in progress.** Landed: the SQLite store, the Python compute core (with
> tests), the **Fetcher** and **Analytic** skills (live prices via Yahoo /
> CoinGecko), the **Visualizer** skill (a self-contained `dashboard.html`), and
> the **Strategic** + **Orchestrator** LLM agents with their Taleb RAG corpus and
> the `advise` pipeline. Still to come: an eval suite. See
> [DESIGN.md](./DESIGN.md) for the full architecture and build plan.

## What it is

`portfolio-pnl` is a pipeline of specialized agents that:

1. **Fetch** fresh prices from public sources,
2. **Compute** your P&L, returns, and risk metrics,
3. **Evaluate** how *anti-fragile* the portfolio is (in Nassim Taleb's sense —
   convexity, stress asymmetry, barbell structure, tail shape),
4. **Render** a single self-contained dashboard, and
5. **Recommend** whether to keep the portfolio as-is or re-balance it.

The recommendation is advisory. The orchestrator recommends; **it never
trades**. A human stays in the loop for every decision.

## Privacy — local-first

Your holdings never leave your machine. Prices are pulled from public sources,
but *what you own, at what cost, and since when* stays local:

- The portfolio itself lives in a local `holdings.json` config.
- Prices and every computed metric are stored in a local **SQLite** database.
- Strategy knowledge is indexed in a local vector store.
- The whole thing runs on your own machine via `aislang chat`, with the math
  performed by deterministic local Python skills.

There is no account, no upload, and no third party that sees your positions.

## Project layout

```
portfolio-pnl/
├── holdings.json        # the portfolio: symbols, lots, asset classes, sources
├── .env.example         # safe-to-commit env template (no real secrets)
├── db/
│   └── schema.sql       # the SQLite schema
├── scripts/             # deterministic Python compute (db.py, portfolio.py, …)
├── tests/               # tests for the compute core
├── knowledge/           # RAG corpus (Taleb principles, heuristics, your thesis)
├── agents/              # the .ais agent modules
├── _schemas/            # JSON inter-agent contracts
├── orchestrator.ais     # the orchestrator entry point
└── aislang.toml         # module manifest
```

## Defaults

The first version keeps the scope narrow and reliable:

| Area | Default |
|---|---|
| Asset classes | Equities, ETFs, and crypto (no options yet) |
| Price sources | Yahoo Finance (equities / ETFs) and CoinGecko (crypto) — both key-free; Playwright as a fallback |
| Base currency | USD |
| Cost basis | FIFO |
| Benchmark | SPY |
| Runtime | Local — via `aislang chat`, compute as local Python skills |
| Cadence | On-demand runs |

Options and derivatives, a deployable compute layer, and scheduled runs are
planned for a later version.

## How to run it

The quickest path is the `Makefile` (run `make help` for all targets):

```sh
make demo      # offline: seed prices -> analyze -> render generated/dashboard.html
make eval      # grade the deterministic recommendation engine (no LLM, no network)
make test      # unit tests + golden eval (sets up a throwaway venv with pytest)
make run       # full advisory pipeline (needs Docker + a chat model)
make clean     # remove all regenerable artifacts
```

Under the hood these are plain scripts. The deterministic core needs nothing but
`python3` (standard library only — no `pip install`):

```sh
python3 scripts/seed_demo.py            # synthetic prices (offline), OR:
python3 scripts/fetch.py                # live prices (Yahoo / CoinGecko, key-free)
python3 scripts/analyze.py --persist    # P&L / risk / anti-fragility -> JSON + DB
python3 scripts/render_dashboard.py     # -> generated/dashboard.html (open in a browser)
python3 scripts/eval_recommend.py       # golden eval: grade the deterministic recommendation engine
python3 -m pytest tests -q              # unit tests + the golden eval
```

The same skills run through aiSlang:

```sh
aislang validate portfolio.ais
aislang skill run fetch_prices portfolio.ais
aislang skill run analyze      portfolio.ais
aislang skill run visualize    portfolio.ais   # -> generated/dashboard.html
aislang eval  portfolio.ais                    # score the agent explanations (needs the model stack up)
```

### Evals

Two layers of graded checks:

- **Deterministic** — `scripts/eval_recommend.py` runs golden scenarios
  (`evals/recommend_cases.jsonl`) through the rule engine and checks the verdict
  + sized actions exactly. No LLM, no network; also runs inside `pytest`.
- **Agent** — `aislang eval portfolio.ais` scores the LLM explanations: the
  Orchestrator must carry the fixed verdict / action symbols through faithfully
  (`evals/advise.jsonl`), and the Strategic brief must ground its read in the
  anti-fragility vocabulary (`evals/strategic_reads.jsonl`).

The full advisory pipeline — fetch → analyze → render → **Strategic →
Orchestrator** — is one command. `run.sh` calls `aislang apply` to start the
LiteLLM model router (a Docker Compose service the agents route through) and
build the RAG index, then chats the `advise` pipeline:

```sh
./run.sh                 # live prices
./run.sh --offline       # seed synthetic prices instead
SKIP_APPLY=1 ./run.sh    # skip `apply` when LiteLLM is already up
```

Prerequisites for the advisor step: **Docker** running, a chat model
(`ANTHROPIC_API_KEY` in `./.env`, or local Ollama `qwen3_8b`), and — *optional* —
Ollama serving `bge-m3` so the Taleb RAG index builds (without it the agents
still run, just **ungrounded**). Tear the stack down afterwards with
`aislang destroy portfolio.ais`.

It prints the Orchestrator's **keep / rebalance** recommendation (JSON per
[`_schemas/recommendation.schema.json`](./_schemas/recommendation.schema.json)),
grounded by the Taleb knowledge corpus under `knowledge/`. `run.sh` also renders
`generated/dashboard.html` — a self-contained, drill-down view (overview →
positions → per-instrument prices → risk & anti-fragility) you open in a browser.

All regenerable artifacts (the SQLite DB, the rendered dashboard) are written
under `generated/` (git-ignored), keeping the project root to source only.

## Sample portfolio

The included [`holdings.json`](./holdings.json) is a small sample portfolio
(cash plus a short-Treasury ETF, an S&P 500 ETF, two equities, and a bit of
Bitcoin) with a barbell bucketing that groups asset classes into a *safe* sleeve,
an *aggressive* sleeve, and the fragile *middle*. Point it at your own positions
by editing that file — symbols, lots (quantity, cost basis, date), asset class,
and price source.

## Learn more

- **[DESIGN.md](./DESIGN.md)** — the full architecture: the agent pipeline and
  data flow, the persistence model, the inter-agent JSON contracts, the complete
  metrics catalog, the dashboard drill-down, and the build plan.
- **[aiSlang docs](../../docs-site/src/language.md)** — the language and CLI this
  project is built on.

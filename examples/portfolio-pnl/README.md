# portfolio-pnl

A local-first, multi-agent portfolio analysis app built on
[aiSlang](../../README.md). It helps one person understand a personal
investment portfolio and make deliberate re-balancing decisions — and it runs
**entirely on your own machine**.

> **Work in progress.** Landed: the SQLite store, the Python compute core (with
> tests), the **Fetcher** and **Analytic** skills (live prices via Yahoo /
> CoinGecko), and the **Strategic** + **Orchestrator** LLM agents with their
> Taleb RAG corpus and the `advise` pipeline. Still to come: the **Visualizer**
> (`dashboard.html`) and an eval suite. See [DESIGN.md](./DESIGN.md) for the full
> architecture and build plan.

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

The deterministic core needs nothing but `python3` (stdlib only):

```sh
python3 scripts/seed_demo.py            # synthetic prices (offline), OR:
python3 scripts/fetch.py                # live prices (Yahoo / CoinGecko, key-free)
python3 scripts/analyze.py --persist    # P&L / risk / anti-fragility -> JSON + DB
python3 -m pytest tests -q              # 24 unit tests over the compute core
```

The same skills run through aiSlang:

```sh
aislang validate portfolio.ais
aislang skill run fetch_prices portfolio.ais
aislang skill run analyze      portfolio.ais
```

The full advisory pipeline — fetch → analyze → **Strategic → Orchestrator** — is
one command (needs a chat model via `ANTHROPIC_API_KEY` or local Ollama
`qwen3_8b`, plus Ollama `bge-m3` for the RAG index):

```sh
./run.sh              # live prices
./run.sh --offline    # seed synthetic prices instead
```

It prints the Orchestrator's **keep / rebalance** recommendation (JSON per
[`_schemas/recommendation.schema.json`](./_schemas/recommendation.schema.json)),
grounded by the Taleb knowledge corpus under `knowledge/`. The **Visualizer**
(`dashboard.html`) is the next increment.

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

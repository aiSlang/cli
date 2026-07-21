# portfolio-pnl

A local-first, multi-agent portfolio analysis app built on
[aiSlang](../../README.md). It helps one person understand a personal
investment portfolio and make deliberate re-balancing decisions — and it runs
**entirely on your own machine**.

> **Work in progress.** The scaffolding, the SQLite schema, and the Python
> compute core (with tests) have landed. The aiSlang agents that drive the
> pipeline are the next increment. See [DESIGN.md](./DESIGN.md) for the full
> architecture and the build plan.

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
| Price sources | Stooq (equities / ETFs) and CoinGecko (crypto) — both key-free; Playwright as a fallback |
| Base currency | USD |
| Cost basis | FIFO |
| Benchmark | SPY |
| Runtime | Local — via `aislang chat`, compute as local Python skills |
| Cadence | On-demand runs |

Options and derivatives, a deployable compute layer, and scheduled runs are
planned for a later version.

## How it will work

Once the agents land, using the app follows the standard aiSlang loop against
the orchestrator project:

```sh
aislang validate .        # parse + type-check the project (no network, no writes)
aislang plan     .        # resolve models, emit the local stack + .env.example
aislang chat     orchestrator .   # run the pipeline and get a recommendation
```

A run walks the pipeline **Fetcher → Analytic → Strategic → Visualizer**, then
the orchestrator synthesizes the analytic and strategic findings — grounded by
the knowledge corpus — into a **keep** or **rebalance** verdict, and the
Visualizer produces a `dashboard.html` you can open locally.

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

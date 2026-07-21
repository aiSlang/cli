-- portfolio-pnl — SQLite schema.
--
-- Structured, queryable store for the numeric side of the portfolio:
-- instruments, the price time-series, and every computed metric
-- snapshot.  (Unstructured strategy knowledge lives separately in the
-- vector store under knowledge/ — see DESIGN.md.)
--
-- All timestamps are ISO-8601 UTC strings (YYYY-MM-DD for daily bars,
-- full datetime for intraday/run stamps) so they sort lexically.

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- Instruments held or referenced (incl. the benchmark).
CREATE TABLE IF NOT EXISTS instruments (
    symbol      TEXT PRIMARY KEY,
    name        TEXT,
    asset_class TEXT NOT NULL,        -- cash|tbill|bond_short|bond_long|equity|etf|reit|commodity|crypto|option
    currency    TEXT NOT NULL DEFAULT 'USD',
    source      TEXT,                 -- price-source adapter id (stooq|coingecko|manual|…)
    meta_json   TEXT
);

-- Cost-basis lots (from holdings.json) — one row per lot for FIFO.
CREATE TABLE IF NOT EXISTS lots (
    id         INTEGER PRIMARY KEY,
    symbol     TEXT NOT NULL REFERENCES instruments(symbol),
    quantity   REAL NOT NULL,
    cost_basis REAL NOT NULL,         -- per-unit acquisition price
    lot_date   TEXT
);

-- Price time-series (the reason we use SQL, not a vector store).
CREATE TABLE IF NOT EXISTS prices (
    symbol TEXT NOT NULL REFERENCES instruments(symbol),
    ts     TEXT NOT NULL,             -- ISO date/datetime
    price  REAL NOT NULL,
    source TEXT,
    PRIMARY KEY (symbol, ts)
);

-- Portfolio-level valuation snapshots over time.
CREATE TABLE IF NOT EXISTS snapshots (
    ts             TEXT PRIMARY KEY,
    total_value    REAL NOT NULL,
    total_cost     REAL NOT NULL,
    cash           REAL NOT NULL DEFAULT 0,
    unrealized_pnl REAL NOT NULL,
    realized_pnl   REAL NOT NULL DEFAULT 0,
    pnl_pct        REAL
);

-- Per-position metrics at a snapshot.
CREATE TABLE IF NOT EXISTS position_metrics (
    ts              TEXT NOT NULL,
    symbol          TEXT NOT NULL REFERENCES instruments(symbol),
    quantity        REAL NOT NULL,
    price           REAL NOT NULL,
    value           REAL NOT NULL,
    cost            REAL NOT NULL,
    weight          REAL NOT NULL,
    unrealized_pnl  REAL NOT NULL,
    unrealized_pct  REAL,
    contrib_to_risk REAL,
    PRIMARY KEY (ts, symbol)
);

-- Portfolio risk metrics at a snapshot.
CREATE TABLE IF NOT EXISTS risk_metrics (
    ts           TEXT PRIMARY KEY,
    vol_ann      REAL,   -- annualized volatility of portfolio returns
    sharpe       REAL,
    sortino      REAL,
    max_drawdown REAL,   -- most negative peak-to-trough (fraction, e.g. -0.23)
    var95        REAL,   -- historical 1-day VaR at 95% (fraction, negative)
    cvar95       REAL,   -- expected shortfall beyond var95 (fraction, negative)
    hhi          REAL,   -- Herfindahl concentration of weights (0..1)
    eff_n        REAL,   -- effective number of positions (1/HHI)
    beta         REAL    -- vs benchmark
);

-- Anti-fragility metrics at a snapshot (Taleb).
CREATE TABLE IF NOT EXISTS fragility_metrics (
    ts               TEXT PRIMARY KEY,
    convexity        REAL,   -- payoff convexity to a symmetric price shock (~0 for linear books)
    stress_asymmetry REAL,   -- (loss under -shock) + (gain under +shock); <0 => concave/fragile
    fragility_score  REAL,   -- composite 0..100 (higher = more fragile)
    classification   TEXT,   -- fragile | robust | antifragile
    barbell_safe     REAL,   -- weight fraction in the safe sleeve
    barbell_mid      REAL,   -- weight fraction in the fragile middle
    barbell_aggr     REAL,   -- weight fraction in the aggressive/convex sleeve
    skew             REAL,
    excess_kurtosis  REAL,
    tail_ratio       REAL,   -- |p95 return| / |p05 return|
    es95             REAL,   -- expected shortfall (== cvar95, kept for the fragility view)
    details_json     TEXT    -- convexity curve, per-shock stress, etc.
);

-- Orchestrator recommendations.
CREATE TABLE IF NOT EXISTS recommendations (
    ts            TEXT PRIMARY KEY,
    verdict       TEXT,      -- keep | rebalance
    summary       TEXT,
    actions_json  TEXT,      -- [{symbol, action, size, rationale}]
    grounded_refs TEXT
);

CREATE INDEX IF NOT EXISTS idx_prices_symbol_ts ON prices(symbol, ts);
CREATE INDEX IF NOT EXISTS idx_posmetrics_ts    ON position_metrics(ts);

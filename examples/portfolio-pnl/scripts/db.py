"""portfolio-pnl — SQLite persistence layer.

Thin, dependency-free helpers over the schema in db/schema.sql.  The
DB is the shared source of truth: the Fetcher writes prices, the
Analytic/Strategic agents write metric snapshots, and the Visualizer
reads everything back.
"""
from __future__ import annotations

import json
import os
import sqlite3
from typing import Iterable, Mapping

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DEFAULT_DB = os.path.join(ROOT, "portfolio.db")
SCHEMA_PATH = os.path.join(ROOT, "db", "schema.sql")


def connect(path: str = DEFAULT_DB) -> sqlite3.Connection:
    conn = sqlite3.connect(path)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys = ON")
    return conn


def init_db(conn: sqlite3.Connection, schema_path: str = SCHEMA_PATH) -> None:
    with open(schema_path, encoding="utf-8") as fh:
        conn.executescript(fh.read())
    conn.commit()


def load_holdings(path: str) -> dict:
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def sync_holdings(conn: sqlite3.Connection, holdings: Mapping) -> None:
    """Upsert instruments + replace the lots table from holdings.json."""
    conn.execute("DELETE FROM lots")
    for p in holdings.get("positions", []):
        conn.execute(
            "INSERT INTO instruments(symbol, name, asset_class, currency, source, meta_json) "
            "VALUES(?,?,?,?,?,?) "
            "ON CONFLICT(symbol) DO UPDATE SET name=excluded.name, "
            "asset_class=excluded.asset_class, currency=excluded.currency, source=excluded.source",
            (p["symbol"], p.get("name"), p["asset_class"],
             p.get("currency", "USD"), p.get("source"), None),
        )
        for lot in p["lots"]:
            conn.execute(
                "INSERT INTO lots(symbol, quantity, cost_basis, lot_date) VALUES(?,?,?,?)",
                (p["symbol"], lot["quantity"], lot["cost_basis"], lot.get("date")),
            )
    conn.commit()


# ─── prices ──────────────────────────────────────────────────────────
def upsert_price(conn, symbol: str, ts: str, price: float, source: str | None = None) -> None:
    conn.execute(
        "INSERT INTO prices(symbol, ts, price, source) VALUES(?,?,?,?) "
        "ON CONFLICT(symbol, ts) DO UPDATE SET price=excluded.price, source=excluded.source",
        (symbol, ts, price, source),
    )


def upsert_prices(conn, rows: Iterable[tuple]) -> int:
    """rows: iterable of (symbol, ts, price, source)."""
    n = 0
    for symbol, ts, price, source in rows:
        upsert_price(conn, symbol, ts, price, source)
        n += 1
    conn.commit()
    return n


def price_history(conn, symbol: str) -> list[tuple]:
    cur = conn.execute("SELECT ts, price FROM prices WHERE symbol=? ORDER BY ts", (symbol,))
    return [(r["ts"], r["price"]) for r in cur.fetchall()]


def latest_prices(conn) -> dict:
    cur = conn.execute(
        "SELECT p.symbol, p.price FROM prices p "
        "JOIN (SELECT symbol, MAX(ts) mts FROM prices GROUP BY symbol) m "
        "  ON p.symbol=m.symbol AND p.ts=m.mts"
    )
    return {r["symbol"]: r["price"] for r in cur.fetchall()}


def all_price_history(conn, symbols: Iterable[str]) -> dict:
    return {s: price_history(conn, s) for s in symbols}


# ─── metric snapshots ────────────────────────────────────────────────
def write_snapshot(conn, ts: str, totals: Mapping) -> None:
    conn.execute(
        "INSERT OR REPLACE INTO snapshots"
        "(ts, total_value, total_cost, cash, unrealized_pnl, realized_pnl, pnl_pct) "
        "VALUES(?,?,?,?,?,?,?)",
        (ts, totals["total_value"], totals["total_cost"], totals.get("cash", 0.0),
         totals["unrealized_pnl"], totals.get("realized_pnl", 0.0), totals.get("pnl_pct")),
    )
    conn.commit()


def write_position_metrics(conn, ts: str, rows: Iterable[Mapping]) -> None:
    conn.execute("DELETE FROM position_metrics WHERE ts=?", (ts,))
    for r in rows:
        conn.execute(
            "INSERT INTO position_metrics"
            "(ts, symbol, quantity, price, value, cost, weight, unrealized_pnl, "
            " unrealized_pct, contrib_to_risk) VALUES(?,?,?,?,?,?,?,?,?,?)",
            (ts, r["symbol"], r["quantity"], r["price"], r["value"], r["cost"],
             r["weight"], r["unrealized_pnl"], r.get("unrealized_pct"),
             r.get("contrib_to_risk")),
        )
    conn.commit()


def write_risk_metrics(conn, ts: str, m: Mapping) -> None:
    cols = ("vol_ann", "sharpe", "sortino", "max_drawdown", "var95",
            "cvar95", "hhi", "eff_n", "beta")
    conn.execute(
        f"INSERT OR REPLACE INTO risk_metrics(ts, {', '.join(cols)}) "
        f"VALUES(?, {', '.join('?' * len(cols))})",
        (ts, *(m.get(c) for c in cols)),
    )
    conn.commit()


def write_fragility_metrics(conn, ts: str, m: Mapping) -> None:
    cols = ("convexity", "stress_asymmetry", "fragility_score", "classification",
            "barbell_safe", "barbell_mid", "barbell_aggr", "skew",
            "excess_kurtosis", "tail_ratio", "es95")
    vals = [m.get(c) for c in cols]
    details = m.get("details")
    conn.execute(
        f"INSERT OR REPLACE INTO fragility_metrics(ts, {', '.join(cols)}, details_json) "
        f"VALUES(?, {', '.join('?' * len(cols))}, ?)",
        (ts, *vals, json.dumps(details) if details is not None else None),
    )
    conn.commit()


def write_recommendation(conn, ts: str, verdict: str, summary: str,
                         actions: list | None = None, grounded_refs: str | None = None) -> None:
    conn.execute(
        "INSERT OR REPLACE INTO recommendations(ts, verdict, summary, actions_json, grounded_refs) "
        "VALUES(?,?,?,?,?)",
        (ts, verdict, summary, json.dumps(actions or []), grounded_refs),
    )
    conn.commit()

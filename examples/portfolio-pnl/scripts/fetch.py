#!/usr/bin/env python3
"""portfolio-pnl — Fetcher: pull prices into the SQLite store.

Deterministic, standard-library-only price fetcher. Reads holdings.json,
fetches a daily price history per position from a key-free source
(dispatched by the position's `source` field), upserts it into
portfolio.db, and prints a `fetch_report` JSON matching
_schemas/fetch_report.schema.json.

Sources:
  * yahoo      — equities / ETFs (default), the key-free v8 chart API.
  * coingecko  — crypto, market_chart (no key).
  * stooq      — legacy CSV adapter; the public endpoint now sits behind
                 a JavaScript anti-bot challenge, so it's kept for
                 completeness but no longer works over plain HTTP.
  * tradingview— reported as `pending` (needs the Playwright fallback,
                 wired in a later increment), never fails the run.

    python3 scripts/fetch.py [--db portfolio.db] [--holdings holdings.json] [--days 180]

The compute layer never lives here — this only fetches + stores.
"""
from __future__ import annotations

import argparse
import csv
import datetime
import io
import json
import os
import sys
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import db as dbmod  # noqa: E402

USER_AGENT = "Mozilla/5.0 (compatible; aislang-portfolio-pnl/0.1; +https://aislang.org)"
TIMEOUT = 20

# Minimal symbol → CoinGecko id map; a position may override with `cg_id`.
COINGECKO_IDS = {
    "BTC-USD": "bitcoin", "BTC": "bitcoin",
    "ETH-USD": "ethereum", "ETH": "ethereum",
    "SOL-USD": "solana", "SOL": "solana",
}


def _get(url: str) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=TIMEOUT) as resp:
        return resp.read()


# ─── source adapters (pure parsers are unit-tested) ──────────────────
def yahoo_symbol(position: dict) -> str:
    return position.get("yahoo_symbol") or position["symbol"]


def parse_yahoo(payload: dict) -> list[tuple]:
    """Parse a Yahoo v8 chart response -> [(date, close)], one per UTC day."""
    res = payload["chart"]["result"][0]
    stamps = res.get("timestamp") or []
    closes = res["indicators"]["quote"][0].get("close") or []
    dedup: dict[str, float] = {}
    for t, c in zip(stamps, closes):
        if c is None:
            continue
        ts = datetime.datetime.fromtimestamp(
            t, tz=datetime.timezone.utc).date().isoformat()
        dedup[ts] = float(c)
    return sorted(dedup.items())


def fetch_yahoo(position: dict, days: int) -> list[tuple]:
    sym = yahoo_symbol(position)
    rng = "6mo" if days <= 126 else ("1y" if days <= 252 else "2y")
    url = (f"https://query1.finance.yahoo.com/v8/finance/chart/{sym}"
           f"?interval=1d&range={rng}")
    rows = parse_yahoo(json.loads(_get(url).decode("utf-8", "replace")))
    return rows[-days:] if days else rows


def stooq_symbol(position: dict) -> str:
    if position.get("stooq_symbol"):
        return position["stooq_symbol"]
    # US listings map to "<ticker>.us"; strip a trailing currency suffix.
    base = position["symbol"].split("-")[0].lower()
    return f"{base}.us"


def parse_stooq_csv(text: str) -> list[tuple]:
    """Parse Stooq daily CSV (Date,Open,High,Low,Close,Volume) -> [(ts, close)]."""
    out = []
    reader = csv.DictReader(io.StringIO(text))
    for row in reader:
        date = (row.get("Date") or "").strip()
        close = (row.get("Close") or "").strip()
        if not date or close in ("", "N/D"):
            continue
        try:
            out.append((date, float(close)))
        except ValueError:
            continue
    return out


def fetch_stooq(position: dict, days: int) -> list[tuple]:
    sym = stooq_symbol(position)
    url = f"https://stooq.com/q/d/l/?s={sym}&i=d"
    rows = parse_stooq_csv(_get(url).decode("utf-8", "replace"))
    return rows[-days:] if days else rows


def coingecko_id(position: dict) -> str:
    return position.get("cg_id") or COINGECKO_IDS.get(
        position["symbol"], position["symbol"].split("-")[0].lower())


def parse_coingecko(payload: dict) -> list[tuple]:
    """Parse CoinGecko market_chart {prices:[[ms, price], …]} -> [(date, price)]."""
    out = []
    for ms, price in payload.get("prices", []):
        ts = datetime.datetime.fromtimestamp(
            ms / 1000.0, tz=datetime.timezone.utc).date().isoformat()
        out.append((ts, float(price)))
    # collapse to one (last) point per day
    dedup = {}
    for ts, price in out:
        dedup[ts] = price
    return sorted(dedup.items())


def fetch_coingecko(position: dict, days: int) -> list[tuple]:
    cid = coingecko_id(position)
    url = (f"https://api.coingecko.com/api/v3/coins/{cid}/market_chart"
           f"?vs_currency=usd&days={days or 365}&interval=daily")
    return parse_coingecko(json.loads(_get(url).decode("utf-8", "replace")))


ADAPTERS = {"yahoo": fetch_yahoo, "coingecko": fetch_coingecko, "stooq": fetch_stooq}
DEFAULT_SOURCE = "yahoo"


# ─── driver ──────────────────────────────────────────────────────────
def fetch_all(conn, holdings: dict, days: int) -> dict:
    run_ts = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
    fetched, failures, pending = [], [], []

    positions = list(holdings.get("positions", []))
    bench = holdings.get("benchmark")
    if bench and not any(p["symbol"] == bench for p in positions):
        positions.append({"symbol": bench, "asset_class": "etf", "source": DEFAULT_SOURCE})

    for p in positions:
        symbol, source = p["symbol"], p.get("source", DEFAULT_SOURCE)
        if source == "tradingview":
            pending.append({"symbol": symbol, "source": source,
                            "reason": "needs the Playwright/TradingView fallback"})
            continue
        adapter = ADAPTERS.get(source)
        if adapter is None:
            failures.append({"symbol": symbol, "source": source,
                             "error": f"unknown source '{source}'"})
            continue
        try:
            rows = adapter(p, days)
        except (urllib.error.URLError, ValueError, KeyError, TimeoutError, OSError) as exc:
            failures.append({"symbol": symbol, "source": source, "error": str(exc)})
            continue
        if not rows:
            failures.append({"symbol": symbol, "source": source, "error": "no data returned"})
            continue
        # Register the instrument first (e.g. the benchmark isn't a holding),
        # so the price rows satisfy the FK to instruments(symbol).
        dbmod.ensure_instrument(conn, symbol, p.get("asset_class", "equity"),
                                source, p.get("name"), p.get("currency", "USD"))
        dbmod.upsert_prices(conn, ((symbol, ts, price, source) for ts, price in rows))
        latest_ts, latest_price = rows[-1]
        fetched.append({"symbol": symbol, "source": source, "points": len(rows),
                        "latest_ts": latest_ts, "latest_price": round(latest_price, 6)})

    return {"run_ts": run_ts, "fetched": fetched, "failures": failures,
            "pending": pending,
            "summary": {"ok": len(fetched), "failed": len(failures),
                        "pending": len(pending)}}


def main() -> int:
    ap = argparse.ArgumentParser(description="Fetch prices into portfolio.db.")
    ap.add_argument("--db", default=dbmod.DEFAULT_DB)
    ap.add_argument("--holdings", default=os.path.join(dbmod.ROOT, "holdings.json"))
    ap.add_argument("--days", type=int, default=180)
    args = ap.parse_args()

    holdings = dbmod.load_holdings(args.holdings)
    conn = dbmod.connect(args.db)
    dbmod.init_db(conn)
    dbmod.sync_holdings(conn, holdings)

    report = fetch_all(conn, holdings, args.days)
    print(json.dumps(report, indent=2))
    # A run "succeeds" (exit 0) even with per-symbol failures/pending so a
    # partial price update still lets the rest of the pipeline proceed.
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

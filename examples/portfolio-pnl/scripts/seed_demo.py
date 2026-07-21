#!/usr/bin/env python3
"""portfolio-pnl — seed synthetic price history for offline testing.

Generates deterministic (seeded) daily price walks for every held
symbol + the benchmark, so `analyze.py` runs end-to-end without the
Fetcher / network.  Replace with real Fetcher output for live use.

    python3 scripts/seed_demo.py [--db portfolio.db] [--days 180]
"""
from __future__ import annotations

import argparse
import datetime
import math
import os
import random
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import db as dbmod  # noqa: E402

# Per-asset-class (annualized drift, annualized vol) for the synthetic walk.
PROFILE = {
    "bond_short": (0.02, 0.02),
    "etf": (0.08, 0.15),
    "equity": (0.12, 0.28),
    "crypto": (0.30, 0.75),
    "cash": (0.00, 0.00),
}
DT = 1.0 / 252.0


def walk(start: float, drift: float, vol: float, days: int, rng: random.Random) -> list[float]:
    price = start
    out = [price]
    for _ in range(days - 1):
        shock = rng.gauss(0.0, 1.0)
        price *= math.exp((drift - 0.5 * vol * vol) * DT + vol * math.sqrt(DT) * shock)
        out.append(price)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=dbmod.DEFAULT_DB)
    ap.add_argument("--holdings", default=os.path.join(dbmod.ROOT, "holdings.json"))
    ap.add_argument("--days", type=int, default=180)
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()

    holdings = dbmod.load_holdings(args.holdings)
    conn = dbmod.connect(args.db)
    dbmod.init_db(conn)
    dbmod.sync_holdings(conn, holdings)

    rng = random.Random(args.seed)
    today = datetime.date.today()
    dates = [(today - datetime.timedelta(days=args.days - 1 - i)).isoformat()
             for i in range(args.days)]

    # start each walk near the earliest lot cost basis so P&L is plausible.
    seeds = {}
    for p in holdings["positions"]:
        seeds[p["symbol"]] = (p["asset_class"], p["lots"][0]["cost_basis"])
    bench = holdings.get("benchmark")
    if bench and bench not in seeds:
        seeds[bench] = ("etf", 475.0)

    rows = []
    for symbol, (asset_class, start) in seeds.items():
        drift, vol = PROFILE.get(asset_class, PROFILE["equity"])
        series = walk(start * rng.uniform(0.9, 1.1), drift, vol, args.days, rng)
        for ts, price in zip(dates, series):
            rows.append((symbol, ts, round(price, 6), "synthetic"))

    n = dbmod.upsert_prices(conn, rows)
    print(f"seeded {n} price points across {len(seeds)} symbols "
          f"({args.days} days, {dates[0]}..{dates[-1]})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

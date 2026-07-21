#!/usr/bin/env python3
"""portfolio-pnl — analyze: compute P&L, risk, and fragility from the DB.

Reads holdings.json + prices from portfolio.db, computes every metric
deterministically, optionally persists a snapshot, and prints a JSON
report. This is exactly what the Analytic + Strategic aiSlang agents
will invoke (via compute skills) — the LLM interprets the JSON, it does
not do the math.

    python3 scripts/analyze.py [--db portfolio.db] [--holdings holdings.json] [--persist]
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import db as dbmod          # noqa: E402
import portfolio as pf      # noqa: E402


def build_report(holdings: dict, latest: dict, history: dict) -> dict:
    positions = holdings["positions"]
    cash = holdings.get("cash", 0.0)
    buckets = holdings.get("barbell_buckets", {})
    benchmark = holdings.get("benchmark")

    pnl = pf.portfolio_pnl(positions, latest, cash)
    quantities = {r["symbol"]: r["quantity"] for r in pnl["positions"]}

    held_history = {s: history.get(s, []) for s in quantities}
    vseries = pf.portfolio_value_series(held_history, quantities, cash)
    values = [v for _, v in vseries]
    rets = pf.simple_returns(values)

    returns_by_symbol = {
        s: pf.simple_returns([p for _, p in history.get(s, [])]) for s in quantities
    }

    bench_rets = None
    if benchmark and history.get(benchmark):
        bench_rets = pf.simple_returns([p for _, p in history[benchmark]])

    weights = [r["weight"] for r in pnl["positions"]]
    risk = {
        "vol_ann": pf.volatility(rets),
        "sharpe": pf.sharpe(rets),
        "sortino": pf.sortino(rets),
        "max_drawdown": pf.max_drawdown(values) if values else None,
        "var95": pf.var_historical(rets),
        "cvar95": pf.cvar_historical(rets),
        "hhi": pf.hhi(weights) if weights else None,
        "eff_n": pf.effective_n(weights) if weights else None,
        "beta": pf.beta(rets, bench_rets) if bench_rets else None,
    }

    barbell = pf.barbell_allocation(
        pnl["positions"], cash, pnl["totals"]["total_value"], buckets)
    skew = pf.skewness(rets)
    stress = pf.stress_asymmetry(rets)
    frag = pf.fragility_assessment(
        hhi_val=risk["hhi"], cvar95=risk["cvar95"], stress_asym=stress,
        barbell_mid=barbell["mid"], max_dd=risk["max_drawdown"], skew=skew)
    convexity_curve = [
        {"shock": s, "convexity": pf.convexity_to_shock(positions, latest, s)}
        for s in (0.05, 0.10, 0.20, 0.30)
    ]

    strategic = {
        "fragility_score": frag["fragility_score"],
        "classification": frag["classification"],
        "convexity": convexity_curve[1]["convexity"],
        "stress_asymmetry": stress,
        "barbell_safe": barbell["safe"],
        "barbell_mid": barbell["mid"],
        "barbell_aggr": barbell["aggr"],
        "skew": skew,
        "excess_kurtosis": pf.excess_kurtosis(rets),
        "tail_ratio": pf.tail_ratio(rets),
        "es95": risk["cvar95"],
        "details": {"convexity_curve": convexity_curve, "drivers": frag["drivers"]},
    }
    analytic = {
        "totals": pnl["totals"],
        "positions": pnl["positions"],
        "risk": risk,
        "correlation": pf.correlation_matrix(returns_by_symbol),
        "missing": pnl["missing"],
        "n_price_points": len(values),
    }
    # Deterministic recommendation (rule engine — NOT the LLM). Same
    # metrics in, same recommendation out; the LLM only explains it.
    recommendation = pf.recommend(analytic, strategic, buckets)
    return {"analytic": analytic, "strategic": strategic,
            "recommendation": recommendation}


def _snapshot_ts(history: dict, quantities: dict) -> str:
    stamps = [h[-1][0] for s, h in history.items() if s in quantities and h]
    return max(stamps) if stamps else datetime.date.today().isoformat()


def main() -> int:
    ap = argparse.ArgumentParser(description="Compute portfolio P&L, risk, and fragility.")
    ap.add_argument("--db", default=dbmod.DEFAULT_DB)
    ap.add_argument("--holdings", default=os.path.join(dbmod.ROOT, "holdings.json"))
    ap.add_argument("--persist", action="store_true", help="write a metric snapshot to the DB")
    ap.add_argument("--compact", action="store_true",
                    help="emit single-line JSON (pipe straight into one agent chat turn)")
    args = ap.parse_args()

    holdings = dbmod.load_holdings(args.holdings)
    conn = dbmod.connect(args.db)
    dbmod.init_db(conn)
    dbmod.sync_holdings(conn, holdings)

    symbols = [p["symbol"] for p in holdings["positions"]]
    if holdings.get("benchmark"):
        symbols.append(holdings["benchmark"])
    latest = dbmod.latest_prices(conn)
    history = dbmod.all_price_history(conn, set(symbols))

    report = build_report(holdings, latest, history)

    if args.persist:
        quantities = {r["symbol"]: r["quantity"] for r in report["analytic"]["positions"]}
        ts = _snapshot_ts(history, quantities)
        dbmod.write_snapshot(conn, ts, report["analytic"]["totals"])
        dbmod.write_position_metrics(conn, ts, report["analytic"]["positions"])
        dbmod.write_risk_metrics(conn, ts, report["analytic"]["risk"])
        dbmod.write_fragility_metrics(conn, ts, report["strategic"])
        # Persist the whole deterministic recommendation (the dashboard reads
        # it back from the recommendations table — no LLM required).
        rec = report["recommendation"]
        conn.execute(
            "INSERT OR REPLACE INTO recommendations"
            "(ts, verdict, summary, actions_json, grounded_refs) VALUES(?,?,?,?,?)",
            (ts, rec["verdict"], rec["summary"], json.dumps(rec),
             json.dumps(rec.get("grounded_refs", []))),
        )
        conn.commit()
        report["persisted_ts"] = ts

    if args.compact:
        print(json.dumps(report, default=float))
    else:
        print(json.dumps(report, indent=2, default=float))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

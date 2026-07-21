"""Unit tests for the deterministic compute core (scripts/portfolio.py).

Run:  python3 -m pytest examples/portfolio-pnl/tests -q
"""
import math
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))
import portfolio as pf  # noqa: E402


# ── P&L ────────────────────────────────────────────────────────────
def test_position_pnl():
    lots = [{"quantity": 10, "cost_basis": 150.0}, {"quantity": 5, "cost_basis": 190.0}]
    r = pf.position_pnl(lots, 200.0)
    assert r["quantity"] == 15
    assert r["cost"] == pytest.approx(2450.0)      # 10*150 + 5*190
    assert r["value"] == pytest.approx(3000.0)     # 15*200
    assert r["unrealized_pnl"] == pytest.approx(550.0)
    assert r["unrealized_pct"] == pytest.approx(550.0 / 2450.0)


def test_realized_pnl_fifo():
    lots = [{"quantity": 10, "cost_basis": 150.0}, {"quantity": 5, "cost_basis": 190.0}]
    # sell 12 @ 200: 10 from lot1 (+50 each) + 2 from lot2 (+10 each) = 500 + 20
    assert pf.realized_pnl(lots, 12, 200.0) == pytest.approx(520.0)


def test_portfolio_pnl_totals_and_weights():
    positions = [
        {"symbol": "A", "asset_class": "equity", "lots": [{"quantity": 10, "cost_basis": 100.0}]},
        {"symbol": "B", "asset_class": "etf", "lots": [{"quantity": 5, "cost_basis": 200.0}]},
    ]
    prices = {"A": 110.0, "B": 200.0}   # A value 1100 (cost 1000), B value 1000 (cost 1000)
    out = pf.portfolio_pnl(positions, prices, cash=900.0)
    t = out["totals"]
    assert t["invested_value"] == pytest.approx(2100.0)
    assert t["total_value"] == pytest.approx(3000.0)
    assert t["unrealized_pnl"] == pytest.approx(100.0)
    # weights (incl. cash) sum to 1
    assert sum(r["weight"] for r in out["positions"]) + t["cash_weight"] == pytest.approx(1.0)


def test_portfolio_pnl_missing_price():
    positions = [{"symbol": "A", "asset_class": "equity", "lots": [{"quantity": 1, "cost_basis": 10.0}]}]
    out = pf.portfolio_pnl(positions, {}, cash=0.0)
    assert out["missing"] == ["A"]
    assert out["positions"] == []


# ── returns / risk ─────────────────────────────────────────────────
def test_simple_returns():
    assert pf.simple_returns([100, 110, 99]) == pytest.approx([0.1, -0.1])


def test_max_drawdown():
    assert pf.max_drawdown([100, 120, 90, 130]) == pytest.approx(-0.25)   # 120 -> 90
    assert pf.max_drawdown([100, 101, 102]) == pytest.approx(0.0)


def test_percentile():
    xs = [1, 2, 3, 4]
    assert pf.percentile(xs, 0.5) == pytest.approx(2.5)
    assert pf.percentile(xs, 0.0) == pytest.approx(1.0)
    assert pf.percentile(xs, 1.0) == pytest.approx(4.0)


def test_var_cvar():
    rets = [-0.10, -0.05, -0.02, 0.0, 0.01, 0.02, 0.03, 0.04, 0.05, 0.06]
    var = pf.var_historical(rets, 0.95)          # 5th percentile
    assert var == pytest.approx(-0.0775)
    assert pf.cvar_historical(rets, 0.95) == pytest.approx(-0.10)   # only -0.10 <= var


def test_hhi_and_effective_n():
    assert pf.hhi([0.5, 0.5]) == pytest.approx(0.5)
    assert pf.effective_n([0.5, 0.5]) == pytest.approx(2.0)
    assert pf.hhi([1.0]) == pytest.approx(1.0)
    assert pf.effective_n([1.0]) == pytest.approx(1.0)


def test_volatility_and_sharpe_edge():
    assert pf.volatility([0.01]) is None                # too few points
    assert pf.volatility([0.01, 0.01, 0.01]) == pytest.approx(0.0)
    assert pf.sharpe([0.01, 0.01, 0.01]) is None        # zero vol -> undefined


def test_beta_and_correlation():
    bench = [0.01, -0.02, 0.03, 0.00, 0.015]
    asset = [2 * x for x in bench]                      # perfectly linear, slope 2
    assert pf.beta(asset, bench) == pytest.approx(2.0)
    assert pf.correlation(asset, bench) == pytest.approx(1.0)


def test_portfolio_value_series_forward_fill():
    hist = {"A": [("d1", 10.0), ("d3", 12.0)], "B": [("d2", 5.0), ("d3", 6.0)]}
    series = pf.portfolio_value_series(hist, {"A": 1, "B": 1}, cash=0.0)
    # d1: B unpriced -> skipped; d2: A=10 (ffill), B=5 -> 15; d3: A=12, B=6 -> 18
    assert series == [("d2", 15.0), ("d3", 18.0)]


# ── anti-fragility ─────────────────────────────────────────────────
def test_skewness_symmetric_is_zero():
    assert pf.skewness([-2, -1, 0, 1, 2]) == pytest.approx(0.0, abs=1e-9)


def test_tail_ratio():
    rets = [-0.05, -0.02, 0.0, 0.02, 0.10]
    assert pf.tail_ratio(rets) == pytest.approx(abs(pf.percentile(rets, 0.95)) /
                                                abs(pf.percentile(rets, 0.05)))


def test_convexity_zero_for_linear_book():
    positions = [{"symbol": "A", "lots": [{"quantity": 3, "cost_basis": 100.0}]}]
    prices = {"A": 120.0}
    for s in (0.05, 0.1, 0.3):
        assert pf.convexity_to_shock(positions, prices, s) == pytest.approx(0.0, abs=1e-9)


def test_barbell_allocation():
    rows = [
        {"asset_class": "bond_short", "value": 100.0},
        {"asset_class": "equity", "value": 300.0},
        {"asset_class": "crypto", "value": 100.0},
    ]
    buckets = {"safe": ["cash", "tbill", "bond_short"], "aggressive": ["crypto", "option"]}
    b = pf.barbell_allocation(rows, cash=0.0, total_value=500.0, buckets=buckets)
    assert b["safe"] == pytest.approx(0.2)
    assert b["mid"] == pytest.approx(0.6)
    assert b["aggr"] == pytest.approx(0.2)
    # cash counts as safe
    b2 = pf.barbell_allocation(rows, cash=500.0, total_value=1000.0, buckets=buckets)
    assert b2["safe"] == pytest.approx(0.6)


def test_fragility_monotonic_and_classification():
    fragile = pf.fragility_assessment(
        hhi_val=0.8, cvar95=-0.08, stress_asym=-0.03, barbell_mid=0.9,
        max_dd=-0.4, skew=-1.0)
    robust = pf.fragility_assessment(
        hhi_val=0.2, cvar95=-0.02, stress_asym=0.01, barbell_mid=0.2,
        max_dd=-0.1, skew=0.5)
    assert fragile["fragility_score"] > robust["fragility_score"]
    assert fragile["classification"] == "fragile"
    assert robust["classification"] in ("robust", "antifragile")
    assert 0.0 <= robust["fragility_score"] <= 100.0


# ── recommendation (deterministic rule engine) ─────────────────────
_BUCKETS = {"safe": ["cash", "tbill", "bond_short"], "aggressive": ["crypto", "option"]}


def _fragile_case():
    analytic = {"totals": {"total_value": 34500.0},
                "positions": [
                    {"symbol": "SPY", "asset_class": "etf", "weight": 0.26},
                    {"symbol": "AAPL", "asset_class": "equity", "weight": 0.14},
                    {"symbol": "MSFT", "asset_class": "equity", "weight": 0.07},
                    {"symbol": "SHY", "asset_class": "bond_short", "weight": 0.24},
                    {"symbol": "BTC-USD", "asset_class": "crypto", "weight": 0.29}]}
    strategic = {"barbell_safe": 0.24, "barbell_mid": 0.47, "barbell_aggr": 0.29,
                 "fragility_score": 35.1, "classification": "robust",
                 "stress_asymmetry": -0.0015, "skew": -0.145}
    return analytic, strategic


def test_recommend_is_deterministic():
    a, s = _fragile_case()
    r1 = pf.recommend(a, s, _BUCKETS)
    r2 = pf.recommend(a, s, _BUCKETS)
    assert r1 == r2                       # pure function — no sampling, no mutation


def test_recommend_fragile_rebalances_with_trim_and_cash():
    a, s = _fragile_case()
    r = pf.recommend(a, s, _BUCKETS)
    assert r["verdict"] == "rebalance"
    acts = {x["symbol"]: x for x in r["actions"]}
    assert acts["SPY"]["action"] == "trim"       # > 25% cap AND fragile middle
    assert acts["CASH"]["action"] == "add"       # freed weight rotates to safe
    assert acts["BTC-USD"]["action"] == "hold"   # convex sleeve held (29% < 35% ceiling)


def test_recommend_healthy_keeps_with_no_actions():
    a = {"totals": {"total_value": 10000.0},
         "positions": [
             {"symbol": "SHY", "asset_class": "bond_short", "weight": 0.55},
             {"symbol": "SPY", "asset_class": "etf", "weight": 0.20},
             {"symbol": "BTC-USD", "asset_class": "crypto", "weight": 0.20}]}
    s = {"barbell_safe": 0.60, "barbell_mid": 0.20, "barbell_aggr": 0.20,
         "fragility_score": 22.0, "classification": "robust",
         "stress_asymmetry": 0.01, "skew": 0.2}
    r = pf.recommend(a, s, _BUCKETS)
    assert r["verdict"] == "keep"
    assert r["actions"] == []

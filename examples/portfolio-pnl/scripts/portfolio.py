"""portfolio-pnl — deterministic numeric core.

Pure functions, standard library only (no numpy/pandas) so the example
runs anywhere `python3` exists.  The agents NEVER do this arithmetic —
they call these functions (via compute skills) and interpret the
results.

Conventions:
  * returns are simple period returns (p[i]/p[i-1] - 1), as fractions.
  * a price_history is a list of (ts, price) tuples sorted ascending.
  * money amounts are in the portfolio base currency.
"""
from __future__ import annotations

import math
import statistics
from typing import Iterable, Mapping, Sequence

TRADING_DAYS = 252


# ─────────────────────────────────────────────────────────────────────
# small numeric helpers
# ─────────────────────────────────────────────────────────────────────
def percentile(xs: Sequence[float], q: float):
    """Linear-interpolated percentile. q in [0, 1]."""
    ys = sorted(x for x in xs if x is not None)
    if not ys:
        return None
    if len(ys) == 1:
        return ys[0]
    idx = q * (len(ys) - 1)
    lo = math.floor(idx)
    hi = math.ceil(idx)
    if lo == hi:
        return ys[int(idx)]
    return ys[lo] + (ys[hi] - ys[lo]) * (idx - lo)


def _mean(xs):
    xs = list(xs)
    return statistics.fmean(xs) if xs else None


def simple_returns(prices: Sequence[float]) -> list[float]:
    out = []
    for i in range(1, len(prices)):
        prev = prices[i - 1]
        if prev:
            out.append(prices[i] / prev - 1.0)
    return out


# ─────────────────────────────────────────────────────────────────────
# P&L
# ─────────────────────────────────────────────────────────────────────
def position_cost_and_qty(lots: Iterable[Mapping]) -> tuple[float, float]:
    """Total held quantity and total cost basis across FIFO lots.

    (We hold every open lot, so the current cost basis is simply the
    sum over lots; FIFO only matters once a SALE realizes P&L, which is
    handled by `realized_pnl`.)
    """
    qty = sum(l["quantity"] for l in lots)
    cost = sum(l["quantity"] * l["cost_basis"] for l in lots)
    return qty, cost


def position_pnl(lots: Iterable[Mapping], price: float) -> dict:
    qty, cost = position_cost_and_qty(list(lots))
    value = qty * price
    upnl = value - cost
    return {
        "quantity": qty,
        "price": price,
        "value": value,
        "cost": cost,
        "unrealized_pnl": upnl,
        "unrealized_pct": (upnl / cost) if cost else None,
    }


def realized_pnl(lots: Sequence[Mapping], sell_qty: float, sell_price: float) -> float:
    """FIFO realized P&L for selling `sell_qty` units at `sell_price`.

    Lots must be ordered oldest-first.  Does not mutate `lots`.
    """
    remaining = sell_qty
    realized = 0.0
    for lot in lots:
        if remaining <= 0:
            break
        take = min(remaining, lot["quantity"])
        realized += take * (sell_price - lot["cost_basis"])
        remaining -= take
    return realized


def portfolio_pnl(positions: Sequence[Mapping], prices: Mapping[str, float],
                  cash: float = 0.0) -> dict:
    """Compute per-position and portfolio-level P&L.

    `positions` is the holdings.json positions list; `prices` maps
    symbol -> latest price.  Positions with no price are skipped and
    reported under `missing`.
    """
    rows, missing = [], []
    invested_value = 0.0
    total_cost = 0.0
    for p in positions:
        price = prices.get(p["symbol"])
        if price is None:
            missing.append(p["symbol"])
            continue
        pp = position_pnl(p["lots"], price)
        pp["symbol"] = p["symbol"]
        pp["asset_class"] = p.get("asset_class")
        rows.append(pp)
        invested_value += pp["value"]
        total_cost += pp["cost"]

    total_value = invested_value + cash
    for r in rows:
        r["weight"] = (r["value"] / total_value) if total_value else 0.0

    unrealized = invested_value - total_cost
    totals = {
        "total_value": total_value,
        "invested_value": invested_value,
        "cash": cash,
        "cash_weight": (cash / total_value) if total_value else 0.0,
        "total_cost": total_cost,
        "unrealized_pnl": unrealized,
        "pnl_pct": (unrealized / total_cost) if total_cost else None,
    }
    return {"totals": totals, "positions": rows, "missing": missing}


# ─────────────────────────────────────────────────────────────────────
# portfolio value series (for returns, drawdown, risk)
# ─────────────────────────────────────────────────────────────────────
def portfolio_value_series(price_history: Mapping[str, Sequence[tuple]],
                           quantities: Mapping[str, float],
                           cash: float = 0.0) -> list[tuple]:
    """Build a (ts, value) series from per-symbol price histories.

    Uses a forward-filled union of timestamps so gaps in one series
    don't drop the whole bar; a bar is only emitted once every held
    symbol has at least one known price.
    """
    per_symbol = {s: dict(h) for s, h in price_history.items() if s in quantities}
    if not per_symbol:
        return []
    all_ts = sorted({ts for h in per_symbol.values() for ts in h})
    last = {s: None for s in per_symbol}
    series = []
    for ts in all_ts:
        for s, h in per_symbol.items():
            if ts in h:
                last[s] = h[ts]
        if any(last[s] is None for s in per_symbol):
            continue  # not every symbol has priced yet
        value = cash + sum(quantities[s] * last[s] for s in per_symbol)
        series.append((ts, value))
    return series


# ─────────────────────────────────────────────────────────────────────
# risk
# ─────────────────────────────────────────────────────────────────────
def volatility(returns: Sequence[float], ppy: int = TRADING_DAYS):
    if len(returns) < 2:
        return None
    return statistics.stdev(returns) * math.sqrt(ppy)


def sharpe(returns: Sequence[float], rf_annual: float = 0.0, ppy: int = TRADING_DAYS):
    if len(returns) < 2:
        return None
    sd = statistics.stdev(returns)
    if sd == 0:
        return None
    mu = statistics.fmean(returns) * ppy
    return (mu - rf_annual) / (sd * math.sqrt(ppy))


def sortino(returns: Sequence[float], rf_annual: float = 0.0, ppy: int = TRADING_DAYS):
    if len(returns) < 2:
        return None
    downs = [min(0.0, r) for r in returns]
    dd = math.sqrt(sum(d * d for d in downs) / len(downs)) * math.sqrt(ppy)
    if dd == 0:
        return None
    mu = statistics.fmean(returns) * ppy
    return (mu - rf_annual) / dd


def max_drawdown(values: Sequence[float]):
    """Most negative peak-to-trough return over the value series (<= 0)."""
    peak = None
    mdd = 0.0
    for v in values:
        if peak is None or v > peak:
            peak = v
        if peak:
            mdd = min(mdd, (v - peak) / peak)
    return mdd


def var_historical(returns: Sequence[float], level: float = 0.95):
    """Historical VaR — the (1-level) percentile return (negative)."""
    if not returns:
        return None
    return percentile(returns, 1 - level)


def cvar_historical(returns: Sequence[float], level: float = 0.95):
    """Expected shortfall — mean of returns at/below the VaR cutoff."""
    v = var_historical(returns, level)
    if v is None:
        return None
    tail = [r for r in returns if r <= v]
    return statistics.fmean(tail) if tail else v


def hhi(weights: Sequence[float]) -> float:
    return sum(w * w for w in weights)


def effective_n(weights: Sequence[float]):
    h = hhi(weights)
    return (1.0 / h) if h else None


def _cov(a: Sequence[float], b: Sequence[float]):
    n = min(len(a), len(b))
    if n < 2:
        return None
    a, b = a[-n:], b[-n:]
    ma, mb = statistics.fmean(a), statistics.fmean(b)
    return sum((a[i] - ma) * (b[i] - mb) for i in range(n)) / (n - 1)


def beta(asset_returns: Sequence[float], bench_returns: Sequence[float]):
    cov = _cov(asset_returns, bench_returns)
    var = _cov(bench_returns, bench_returns)
    if cov is None or not var:
        return None
    return cov / var


def correlation(a: Sequence[float], b: Sequence[float]):
    cov = _cov(a, b)
    sa, sb = _cov(a, a), _cov(b, b)
    if cov is None or not sa or not sb:
        return None
    return cov / math.sqrt(sa * sb)


def correlation_matrix(returns_by_symbol: Mapping[str, Sequence[float]]) -> dict:
    syms = list(returns_by_symbol)
    out = {}
    for s in syms:
        out[s] = {}
        for t in syms:
            out[s][t] = 1.0 if s == t else correlation(
                returns_by_symbol[s], returns_by_symbol[t])
    return out


# ─────────────────────────────────────────────────────────────────────
# anti-fragility (Taleb)
# ─────────────────────────────────────────────────────────────────────
def _std_moment(returns: Sequence[float], k: int):
    n = len(returns)
    if n < 2:
        return None
    m = statistics.fmean(returns)
    sd = statistics.pstdev(returns)
    if sd == 0:
        return None
    return sum(((r - m) / sd) ** k for r in returns) / n


def skewness(returns: Sequence[float]):
    return _std_moment(returns, 3)


def excess_kurtosis(returns: Sequence[float]):
    k = _std_moment(returns, 4)
    return None if k is None else k - 3.0


def tail_ratio(returns: Sequence[float]):
    """|right tail| / |left tail| — >1 means the up-tail dominates (good)."""
    hi = percentile(returns, 0.95)
    lo = percentile(returns, 0.05)
    if lo is None or hi is None or lo == 0:
        return None
    return abs(hi) / abs(lo)


def stress_asymmetry(returns: Sequence[float], level: float = 0.95):
    """Data-driven convexity read: mean best-tail + mean worst-tail move.

    Uses the historical portfolio return distribution instead of a
    (trivially symmetric) uniform price shock.  < 0 means the left tail
    is deeper than the right — a concave / fragile payoff; > 0 means an
    anti-fragile, positively-asymmetric one.
    """
    if len(returns) < 4:
        return None
    lo = percentile(returns, 1 - level)
    hi = percentile(returns, level)
    left = [r for r in returns if r <= lo]
    right = [r for r in returns if r >= hi]
    if not left or not right:
        return None
    return statistics.fmean(right) + statistics.fmean(left)


def convexity_to_shock(positions: Sequence[Mapping], prices: Mapping[str, float],
                       shock: float) -> float:
    """Payoff convexity to a symmetric ±shock on all prices:
    ½·(V(+s)+V(−s)) − V(0).

    For a purely linear long book this is ~0 by construction — genuine
    convexity comes from optionality.  It's reported so option overlays
    (v2) show up, and to make the linearity explicit.
    """
    def value(mult):
        v = 0.0
        for p in positions:
            price = prices.get(p["symbol"])
            if price is None:
                continue
            qty, _ = position_cost_and_qty(p["lots"])
            v += qty * price * mult
        return v
    v0 = value(1.0)
    return 0.5 * (value(1.0 + shock) + value(1.0 - shock)) - v0


def barbell_allocation(pnl_rows: Sequence[Mapping], cash: float,
                       total_value: float, buckets: Mapping[str, Sequence[str]]) -> dict:
    """Split portfolio weight into safe / fragile-middle / aggressive-convex.

    Cash counts as safe.  asset_classes not listed in `safe`/`aggressive`
    fall in the fragile middle.
    """
    safe = set(buckets.get("safe", []))
    aggr = set(buckets.get("aggressive", []))
    s, m, a = cash, 0.0, 0.0
    for r in pnl_rows:
        ac = r.get("asset_class")
        v = r["value"]
        if ac in safe:
            s += v
        elif ac in aggr:
            a += v
        else:
            m += v
    tot = s + m + a if (s + m + a) else (total_value or 1.0)
    return {"safe": s / tot, "mid": m / tot, "aggr": a / tot}


def _clamp01(x: float) -> float:
    return max(0.0, min(1.0, x))


def fragility_assessment(*, hhi_val, cvar95, stress_asym, barbell_mid,
                         max_dd, skew) -> dict:
    """Transparent composite fragility score (0..100, higher = more
    fragile) + a fragile/robust/antifragile classification.

    Each driver is mapped to a 0..1 fragility contribution on an
    explicit scale, then weighted.  Missing inputs are treated as
    neutral (0.5) so the score degrades gracefully with short history.
    """
    def contrib(x):
        return 0.5 if x is None else _clamp01(x)

    concentration_f = contrib(hhi_val)                       # HHI is already 0..1
    tail_f = contrib(None if cvar95 is None else -cvar95 / 0.10)   # -10% daily ES -> 1
    asym_f = contrib(None if stress_asym is None else 0.5 - stress_asym / 0.10)
    barbell_f = contrib(barbell_mid)                          # weight stuck in the middle
    drawdown_f = contrib(None if max_dd is None else -max_dd / 0.50)  # -50% DD -> 1
    skew_f = contrib(None if skew is None else 0.5 - skew / 2.0)      # negative skew -> fragile

    weights = {
        "concentration": 0.20, "tail": 0.20, "asymmetry": 0.15,
        "barbell": 0.20, "drawdown": 0.15, "skew": 0.10,
    }
    parts = {
        "concentration": concentration_f, "tail": tail_f, "asymmetry": asym_f,
        "barbell": barbell_f, "drawdown": drawdown_f, "skew": skew_f,
    }
    score = 100.0 * sum(weights[k] * parts[k] for k in weights)

    positive_convex = (stress_asym is not None and stress_asym > 0) and \
                      (skew is not None and skew > 0)
    if score < 35 and positive_convex:
        classification = "antifragile"
    elif score < 60:
        classification = "robust"
    else:
        classification = "fragile"
    return {"fragility_score": round(score, 1),
            "classification": classification,
            "drivers": {k: round(v, 3) for k, v in parts.items()}}


# ─────────────────────────────────────────────────────────────────────
# recommendation — DETERMINISTIC rule engine (not the LLM)
# ─────────────────────────────────────────────────────────────────────
# The keep/rebalance decision and the sizing are mechanical given the
# metrics, so they live in code — same metrics in, same recommendation
# out, every run.  The LLM only *explains* this; it never decides it.
# Thresholds are explicit and tunable.
REC_RULES = {
    "concentration_cap": 0.25,   # no single position above this weight
    "target_mid_max":    0.40,   # the fragile middle should sit at/below this
    "min_safe":          0.25,   # safe sleeve floor (informational flag)
    "max_aggr":          0.35,   # convex/aggressive sleeve ceiling
    "fragility_rebalance": 55.0,  # score at/above this always rebalances
}


def _bucket_of(asset_class, buckets) -> str:
    if asset_class in set(buckets.get("safe", [])):
        return "safe"
    if asset_class in set(buckets.get("aggressive", [])):
        return "aggr"
    return "mid"


def recommend(analytic: Mapping, strategic: Mapping, buckets: Mapping,
              rules: Mapping = REC_RULES) -> dict:
    """Deterministic keep/rebalance recommendation from the metrics.

    Returns a dict matching _schemas/recommendation.schema.json, with
    verdict + actions (weight-based sizes) + reason strings decided by
    rule.  Reproducible: identical inputs -> identical output.
    """
    positions = analytic.get("positions", [])
    rows = [{"symbol": p["symbol"], "weight": p.get("weight", 0.0),
             "bucket": _bucket_of(p.get("asset_class"), buckets)} for p in positions]
    by_sym = {r["symbol"]: r for r in rows}

    safe = strategic.get("barbell_safe", 0.0) or 0.0
    mid = strategic.get("barbell_mid", 0.0) or 0.0
    aggr = strategic.get("barbell_aggr", 0.0) or 0.0
    frag = strategic.get("fragility_score", 0.0) or 0.0
    classification = strategic.get("classification", "robust")

    trim: dict[str, float] = {}     # symbol -> weight points to trim
    flags: list[str] = []

    def add_trim(sym, pts):
        trim[sym] = trim.get(sym, 0.0) + max(0.0, pts)

    # 1. single-name concentration — only in the fragile MIDDLE. A large
    # safe sleeve (cash / short bonds) is what a barbell wants, and the
    # aggressive sleeve is governed by its own ceiling below.
    for r in rows:
        if r["bucket"] != "mid":
            continue
        over = r["weight"] - rules["concentration_cap"]
        if over > 0.005:
            add_trim(r["symbol"], over)
            flags.append(f"{r['symbol']} at {r['weight']*100:.0f}% exceeds the "
                         f"{rules['concentration_cap']*100:.0f}% single-name cap")

    # 2. fragile middle too heavy -> proportional trim of middle names to target
    excess_mid = mid - rules["target_mid_max"]
    if excess_mid > 0.005 and mid > 0:
        for r in rows:
            if r["bucket"] == "mid":
                add_trim(r["symbol"], excess_mid * (r["weight"] / mid))
        flags.append(f"fragile middle at {mid*100:.0f}% exceeds the "
                     f"{rules['target_mid_max']*100:.0f}% target")

    # 3. aggressive/convex sleeve ceiling
    aggr_rows = [r for r in rows if r["bucket"] == "aggr"]
    over_aggr = aggr - rules["max_aggr"]
    if over_aggr > 0.005 and aggr_rows:
        big = max(aggr_rows, key=lambda r: r["weight"])
        add_trim(big["symbol"], over_aggr)
        flags.append(f"aggressive sleeve at {aggr*100:.0f}% exceeds the "
                     f"{rules['max_aggr']*100:.0f}% ceiling")

    # realize trims into actions (bounded to 90% of the position)
    actions = []
    for r in sorted(rows, key=lambda r: -trim.get(r["symbol"], 0.0)):
        t = min(trim.get(r["symbol"], 0.0), r["weight"] * 0.9)
        if t > 0.005:
            new_w = r["weight"] - t
            reason = ("single-name concentration" if r["weight"] > rules["concentration_cap"]
                      else "reduce the fragile middle" if r["bucket"] == "mid"
                      else "cap the convex sleeve")
            actions.append({
                "symbol": r["symbol"], "action": "trim",
                "size": f"-{t*100:.1f} pts ({r['weight']*100:.1f}% → {new_w*100:.1f}%)",
                "rationale": f"Trim {r['symbol']} — {reason}; rotate the weight to the safe sleeve.",
            })

    freed = sum(min(trim.get(r["symbol"], 0.0), r["weight"] * 0.9) for r in rows)
    if freed > 0.005:
        # Freed weight rotates into CASH, so size the move against CASH's OWN
        # weight (what the allocation donut shows) — not the whole safe sleeve.
        cash_w = (analytic.get("totals", {}).get("cash_weight") or 0.0)
        new_cash = cash_w + freed
        new_safe = safe + freed
        actions.append({
            "symbol": "CASH", "action": "add",
            "size": f"+{freed*100:.1f} pts ({cash_w*100:.1f}% → {new_cash*100:.1f}% cash)",
            "rationale": "Move the trimmed weight into cash / short bonds to thicken the "
                         f"safe sleeve (now ~{new_safe*100:.0f}%) and restore the barbell "
                         "(heavy safe, thin middle).",
        })
        if new_safe < rules["min_safe"]:
            flags.append(f"safe sleeve still below the {rules['min_safe']*100:.0f}% floor "
                         f"after trimming")

    # keep the convex sleeve when it isn't the thing being trimmed
    if aggr_rows and over_aggr <= 0.005:
        big = max(aggr_rows, key=lambda r: r["weight"])
        actions.append({
            "symbol": big["symbol"], "action": "hold", "size": None,
            "rationale": "The convex / aggressive sleeve is the antifragile engine "
                         "(capped weight, asymmetric upside) — hold it; do not trim.",
        })

    material = any(a["action"] in ("trim", "add", "exit", "hedge") for a in actions)
    verdict = "rebalance" if (material or frag >= rules["fragility_rebalance"]) else "keep"
    if verdict == "keep":
        actions = []   # schema: no actions on a keep

    if verdict == "keep":
        summary = (f"Keep. The book is {classification} (fragility {frag:.0f}/100) and sits "
                   f"within the barbell and concentration guardrails — safe {safe*100:.0f}%, "
                   f"middle {mid*100:.0f}%, aggressive {aggr*100:.0f}%. No action needed.")
    else:
        why = "; ".join(flags[:3]) if flags else "the barbell is drifting from target"
        summary = (f"Rebalance. {classification.capitalize()} overall (fragility {frag:.0f}/100), "
                   f"but {why}. Trim the flagged names and rotate the weight into the safe "
                   f"sleeve to restore the barbell.")

    sa = strategic.get("stress_asymmetry")
    skew = strategic.get("skew")
    frag_read = (f"{classification.capitalize()} (fragility {frag:.0f}/100). Barbell "
                 f"{safe*100:.0f}% safe / {mid*100:.0f}% middle / {aggr*100:.0f}% aggressive"
                 + (f"; stress asymmetry {sa:+.4f}" if sa is not None else "")
                 + (f", skew {skew:+.2f}" if skew is not None else "")
                 + (". Concave/left-tail-heavy shape." if (sa is not None and sa < 0)
                    else "."))

    return {
        "verdict": verdict, "summary": summary, "fragility_read": frag_read,
        "actions": actions, "confidence": "high",
        "grounded_refs": flags or ["within barbell / concentration / fragility guardrails"],
    }

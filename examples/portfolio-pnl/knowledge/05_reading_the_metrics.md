# Reading the metrics this app computes

This is the bridge from the app's exact numbers to the concepts in the other
knowledge files. Every value below is computed by the deterministic Python core
(`scripts/portfolio.py`) and handed to the agents in the `strategic_output` /
`analytic_output` contracts. The agents **interpret** these numbers; they never
recompute them. All returns are simple *daily* returns (fractions); money is in
the base currency (USD).

A general caveat: risk and fragility metrics need history. With few price bars
they are `null` or noisy, and the composite score treats missing inputs as
neutral. Weight your read accordingly when a portfolio is young.

## The two headline anti-fragility numbers

### `fragility_score` (0..100, higher = more fragile) and `classification`

A transparent composite. Six drivers are each mapped to a 0..1 fragility
contribution and weighted:

| Driver | Weight | Fragile (→1) when |
|---|---|---|
| concentration (HHI) | 0.20 | HHI near 1 (concentrated) |
| tail (expected shortfall) | 0.20 | ES95 near −10%/day or worse |
| asymmetry (stress_asymmetry) | 0.15 | stress_asymmetry strongly negative |
| barbell (middle weight) | 0.20 | large `barbell_mid` |
| drawdown | 0.15 | max drawdown near −50% or worse |
| skew | 0.10 | strongly negative skew |

`classification` is then:
- **antifragile** — `fragility_score < 35` **and** both `stress_asymmetry > 0`
  and `skew > 0` (genuinely positive convexity, not just a low score).
- **robust** — `fragility_score < 60` (and not antifragile).
- **fragile** — `fragility_score ≥ 60`.

Rough bands for the score: **< 35** is strong (and, if convex, antifragile);
**35–60** is robust/acceptable; **≥ 60** is fragile and warrants action. Always
read the underlying drivers, not just the headline — two portfolios can reach
55 for very different reasons.

## The anti-fragility detail metrics

### `convexity`
Payoff curvature to a symmetric ±price shock: `½·(V(+s)+V(−s)) − V(0)`. For a
**purely linear, long-only book this is ~0 by construction** — it only becomes
informative once options / non-linear positions exist. So on a plain
stock/ETF portfolio, do **not** read convexity as "no antifragility"; it is
simply uninformative, and the anti-fragility read leans on the items below.
A materially non-zero convexity (positive = convex/good, negative =
concave/bad) is meaningful *only* when the book actually holds non-linear
positions.

### `stress_asymmetry`
The data-driven convexity read: `mean(best-tail returns) + mean(worst-tail
returns)`, using the historical distribution rather than a trivially symmetric
shock.
- **< 0** — the left tail is deeper than the right is high → concave / **fragile**.
- **> 0** — positively asymmetric → more **antifragile**.
- **≈ 0** — roughly symmetric.
For a linear book this is one of the *primary* antifragility signals (since
`convexity` is ~0). More negative = worse.

### `barbell_safe` / `barbell_mid` / `barbell_aggr`
Weight fractions in the three sleeves (they sum to ~1; cash counts as safe).
- **`barbell_safe`** — cash + short government bonds. Higher caps total loss.
- **`barbell_mid`** — the **fragile middle** (ordinary long equities/ETFs with
  tail exposure but no convex upside). A **high `barbell_mid` is a fragility
  flag** — it drives 20% of the composite score directly.
- **`barbell_aggr`** — the small, capped-downside/large-upside convex sleeve.
A **healthy** barbell is heavy `safe` + a small `aggr`, with `mid` kept thin. A
**concerning** one is middle-dominated. On a 100%-equity book, expect
`barbell_mid ≈ 1` — the main lever is to move weight into `safe`.

## Tail-shape metrics (read the return distribution)

- **`skew`** — want **≥ 0**. Negative skew (occasional large losses vs. steady
  small gains) is the fragile shape; positive skew is antifragile.
- **`excess_kurtosis`** — **> 0 means fat tails** (extreme moves more likely
  than normal). Higher = more tail risk on both sides; watch it together with
  skew (fat + negative-skew is the dangerous combination).
- **`tail_ratio`** — `|p95 return| / |p05 return|`. **> 1 favours the upside**
  (right tail dominates, leans antifragile); **< 1** means the down-tail is
  bigger (fragile).
- **`es95` / `cvar95`** — expected shortfall at 95%: the **mean return in the
  worst 5% of days** (negative). These are the *same value*. **Larger magnitude
  (more negative) = deeper left tail = more fragile.** This is the most direct
  tail-severity number; in the composite, roughly −10%/day maps to maximum
  fragility on this driver.

## Risk basics (`analytic_output` / `risk_metrics`)

- **`sharpe`** — annualized excess return per unit of total volatility. Higher
  is better; roughly, < 0 poor, ~0.5–1 okay, > 1 good. A risk-adjusted return
  gauge, *not* a fragility measure on its own.
- **`sortino`** — like Sharpe but penalizes only *downside* deviation, so it
  rewards positive asymmetry. Sortino comfortably above Sharpe hints at a
  favourable return shape.
- **`max_drawdown`** — worst peak-to-trough decline, as a **fraction ≤ 0**
  (e.g. `−0.23` = −23%). Larger magnitude = deeper historical pain; near −50%
  maxes out its fragility contribution.
- **`var95`** — historical 1-day VaR: the 5th-percentile daily return
  (negative). The threshold; `es95`/`cvar95` describe what lies *beyond* it and
  matter more for fragility.
- **`hhi`** + **`eff_n`** — concentration. `hhi = Σ(weightᵢ²)` in **0..1**;
  `eff_n = 1/hhi` is the effective number of independent-weight positions.
  **High HHI / low effective-N = concentrated = fragile** (drives 20% of the
  composite). A book of 12 names with `eff_n ≈ 3` is really a 3-bet portfolio.
- **`beta`** — sensitivity to the benchmark (SPY): `cov(port,bench)/var(bench)`.
  ~1 moves with the market, > 1 amplifies it, < 1 dampens it, < 0 hedges. High
  beta means market shocks pass through with leverage.

## Combining them into an overall health read

Read the numbers *together*; no single metric decides it. The two archetypes:

- **Robust / antifragile** — low `fragility_score` (< 35), `stress_asymmetry ≥ 0`,
  `skew ≥ 0`, `tail_ratio > 1`, a **healthy barbell** (solid `barbell_safe`,
  thin `barbell_mid`), **diversified** (low HHI / high `eff_n`), and a modest
  `max_drawdown`. Small, contained left tail; the shape works *for* you.
- **Fragile** — high `fragility_score` (≥ 60), **negative** `skew` and
  `stress_asymmetry`, `tail_ratio < 1`, a **middle-heavy barbell** (large
  `barbell_mid`, little `safe`), **concentrated** (high HHI / low `eff_n`),
  large-magnitude `es95` and deep `max_drawdown`. Accelerating losses in the
  tail; the shape works *against* you.

Practical reading order: start with `fragility_score` + `classification` for the
headline, then confirm *why* via the drivers — check `barbell_mid` and HHI/
`eff_n` for structural fragility, `es95` + `skew` + `stress_asymmetry` for tail
fragility, and `max_drawdown` for historical severity. On a linear long-only
book, ignore `convexity` (~0) and lean on `stress_asymmetry`, `skew`, and the
barbell/concentration structure instead. A single red driver against an
otherwise healthy book is a targeted re-balance (e.g. trim the concentrated
winner, raise the safe sleeve); several red drivers together mean the whole
structure is fragile and needs de-risking.

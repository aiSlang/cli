# Detecting fragility

Taleb and Raphael Douady's **fragility heuristic** gives a model-free way to
detect fragility without predicting the shock: probe how the thing responds to
an *increasing* stressor and watch the *curvature* of the response.

## Fragility = accelerating losses (a concave response)

The signature of fragility is that **losses accelerate as stress increases**. If
doubling the size of a shock more than doubles the loss, the response is
**concave** — the position is fragile. Robust things lose roughly in proportion
to the shock; antifragile things eventually *gain* as the shock grows (a convex
response). So you do not need to know *whether* a shock will come — you only need
to measure the *shape* of the response to shocks of growing size. A fragile
position is one where the harm is disproportionately large in the tail.

Concretely, you probe with a stressor of increasing magnitude and look at the
second-order behaviour (the acceleration), not just the first-order slope. A
straight-line (linear) response is neither fragile nor antifragile in this
sense; a downward-bending curve is fragile.

## Statistical signatures of fragility in the return distribution

Because a portfolio's fragility is ultimately about **left-tail severity**, the
historical return distribution carries most of the evidence. Look for:

- **Fat tails (high kurtosis).** Extreme moves are far more frequent than a
  normal distribution implies. Excess kurtosis > 0 means the tails are fatter
  than normal — big surprises, in both directions, are more likely than a
  Gaussian model would suggest.
- **Negative skew.** A left-skewed distribution has occasional large losses
  against a background of small, steady gains — the classic fragile ("picking
  up pennies in front of a steamroller") shape. Negative skew is a fragility
  signature; positive skew (small steady costs, occasional large gains) is the
  antifragile shape.
- **Deep left tail / expected shortfall.** The severity of the worst outcomes —
  measured as **expected shortfall** (the mean loss *beyond* the VaR cutoff,
  a.k.a. CVaR) — is the most direct fragility number. A large-magnitude negative
  expected shortfall means that when things go wrong, they go *very* wrong.
- **Tail asymmetry.** Compare the worst tail to the best tail. If the left tail
  is deeper than the right tail is high, the payoff is concave / fragile; if the
  right dominates, it leans antifragile.

## Why a symmetric price shock alone under-detects fragility

A natural first probe is a **symmetric ±x% shock** applied to all prices, then
measure the payoff curvature: `½·(V(+x) + V(−x)) − V(0)`. This works beautifully
when non-linear positions (options, structured payoffs) are present, because
their curvature shows up immediately.

But for a **purely linear, long-only book** (ordinary stocks and ETFs, no
options) this measure is **~0 by construction** — value scales linearly with
price, so `V(+x)` and `V(−x)` average back to `V(0)`. A symmetric price shock
sees *no* convexity or concavity in a linear book, even when the book is in fact
fragile in the real world (fat tails, crash correlation, concentration).

The fix is to **read the actual historical return distribution** rather than
relying on a synthetic symmetric shock. Real markets deliver *asymmetric*,
fat-tailed moves — crashes are faster and deeper than rallies — and that
asymmetry lives in the data. So for linear books, fragility detection leans on
the distributional signatures above (skew, kurtosis, expected shortfall, and a
data-driven tail asymmetry that averages the worst-tail and best-tail moves)
rather than on the symmetric-shock convexity number. Keep the convexity probe
for the day option overlays are added; use the distribution for everything else.

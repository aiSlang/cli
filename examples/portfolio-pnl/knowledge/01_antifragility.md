# Anti-fragility: the fragile / robust / antifragile triad

Nassim Taleb's core idea (from *Antifragile* and *The Black Swan*) is that
things fall into three categories by how they respond to disorder — volatility,
stress, randomness, shocks, and the passage of time:

- **Fragile** — *harmed* by disorder. Gains are small and bounded; losses are
  large and unbounded. A fragile thing wants calm and predictability, and it
  breaks under a shock it did not see coming.
- **Robust / resilient** — *unaffected* by disorder within a range. It absorbs
  shocks without breaking, but it does not benefit from them either.
- **Antifragile** — *helped* by disorder. Within limits, volatility, errors,
  and stressors make it stronger or richer. Its upside from a shock exceeds its
  downside.

The triad is about the **response function**, not the average outcome. Two
positions with the same expected return can be worlds apart: one bleeds
catastrophically in a crash, the other profits from the same chaos.

## Convexity: the mathematical signature

Fragility and antifragility are, precisely, statements about the **curvature**
(the convexity or concavity) of a payoff as a function of a stressor.

- A **concave** response bends downward: as the stressor grows, you lose at an
  *accelerating* rate. Harm from a big shock is more than proportional to harm
  from a small one. **Concavity = fragility.**
- A **convex** response bends upward: as the stressor grows, you gain at an
  *accelerating* rate, while your loss on the downside is capped or small.
  **Convexity = antifragility.**

**Jensen's inequality** gives the intuition. For a convex payoff *f*, the
*average of the outcomes* under a spread of shocks exceeds the *outcome at the
average* shock: `E[f(x)] > f(E[x])`. So a convex position is *paid* by
volatility itself — more dispersion in *x* raises the expected payoff even if
the average *x* does not move. For a concave (fragile) payoff the inequality
flips: volatility *costs* you. This is why "more uncertainty" is good news for
the antifragile and bad news for the fragile.

## Asymmetry of payoffs

Antifragility is fundamentally about **asymmetry**: you want the payoff where
the upside is much larger than the downside (a "convex" or "long-optionality"
bet — limited, known loss; large, open-ended gain). Fragility is the mirror
image: limited upside, open-ended loss (like selling insurance or picking up
pennies in front of a steamroller). Look at the *shape* of the tails, not the
central tendency.

## Why prediction is unreliable — structure beats forecasting

Rare, high-impact events ("Black Swans") dominate outcomes in fat-tailed
domains like markets, yet they are exactly the events that models and forecasts
miss. Because you cannot reliably predict *when* or *how big* the next shock is,
Taleb's prescription is to stop trying to forecast the shock and instead **fix
your exposure to it**: build a structure that is convex (or at least robust) to
disorder, so that being wrong is cheap and being right is large. You do not need
to know the future if your downside is bounded and your upside is open. In this
app, that principle drives everything: we measure the *shape of exposure* — tail
severity, asymmetry, convexity, concentration — rather than trying to predict
returns.

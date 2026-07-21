# Deliberate re-balancing heuristics

Re-balancing here is **risk management, not market timing**. The goal is not to
predict which way the market goes next; it is to keep the portfolio's *shape* —
its concentration, its tail exposure, its barbell structure — inside sane
bounds, so that no single event can do outsized damage. Every rule below is
about controlling exposure, not chasing return.

## Concentration limits and true diversification

Cap how much any single position — and any single *cluster* of correlated
positions — can represent. Diversification is **not** about the *count* of
holdings; ten names that all move together in a crash are effectively one bet.
What matters is **correlation clustering**: read the correlation matrix and the
effective-number-of-positions, and treat highly correlated holdings as a single
concentrated exposure. A book that looks diversified by count but collapses to a
few independent risk factors is fragile.

## Trim winners that have grown into oversized weights

A position that appreciates strongly will quietly grow into an oversized share
of the portfolio — and with it, an oversized share of the *risk*. Trimming
winners back toward target weight is the core mechanical discipline: it locks in
some gains, restores diversification, and prevents a single name's reversal from
dominating the drawdown. This is uncomfortable precisely because it means
selling what has been working — which is exactly why it must be a rule, not a
mood.

## Keep a cash / safe buffer

Maintain a deliberate allocation to the safe sleeve (cash, short government
bonds). The buffer does three things: it caps the whole portfolio's maximum
loss, it provides dry powder to deploy *into* dislocations (buying convexity
when it is cheap), and it is the primary lever for de-fragilizing a linear book.
When in doubt, raising the safe weight is the most reliable way to reduce
fragility.

## Preserve optionality

Prefer changes that keep future choices open and keep the downside capped.
Avoid moves that lock you into a single scenario or that create open-ended
losses. A small, capped-downside convex sleeve is worth keeping even when it is
quiet — it is the part of the portfolio that pays off in the surprises you
cannot predict.

## Drawdown discipline

Set drawdown limits in advance and act on them mechanically. Pre-committed
rules beat in-the-moment judgement, which is exactly when fear and hope corrupt
decisions. Discipline about *when* to de-risk is worth more than any forecast of
*whether* to.

## Avoid performance-chasing

Do not add to a position simply because it has recently outperformed, and do not
dump a safe holding because it has been "boring." Performance-chasing
systematically buys high, concentrates into whatever just ran, and quietly
rebuilds fragility. Re-balancing should push *against* recent performance
(trim what ran, top up what lagged toward target), not with it.

## The bottom line

Treat re-balancing as maintenance of a resilient structure. Ask "does this
change reduce concentration, cap the left tail, preserve optionality, and keep
the barbell healthy?" — not "will this make more money next quarter?" If a
proposed action is really a market-timing bet in disguise, it does not belong in
a re-balance.

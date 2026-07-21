#!/usr/bin/env bash
# portfolio-pnl — run the full advisory pipeline end to end:
#
#   fetch prices -> compute metrics -> render dashboard -> ask the agents
#   for a keep / rebalance recommendation.
#
# The deterministic stages (fetch, analyze, render) are plain Python and
# need nothing but python3.  The advisor stage runs the aiSlang `advise`
# pipeline (Strategic -> Orchestrator), which routes model calls through a
# local LiteLLM proxy — so it needs the model stack up.
#
# Prerequisites for the ADVISOR stage:
#   * the `aislang` binary on PATH (override with AISLANG=/path/to/aislang)
#   * Docker running (LiteLLM comes up as a Compose service via `apply`)
#   * a chat model: ANTHROPIC_API_KEY in ./.env  (or run Ollama with qwen3_8b)
#   * OPTIONAL: Ollama serving bge-m3 (`ollama pull bge-m3`) so the RAG index
#     builds — without it the agents still run, just UNGROUNDED.
#
# This script runs `aislang apply` to start LiteLLM AND build the RAG index
# before chatting.  Set SKIP_APPLY=1 if the stack is already up.
#
# Usage:
#   ./run.sh                 # live prices (Yahoo / CoinGecko)
#   ./run.sh --offline       # seed synthetic prices instead
#   SKIP_APPLY=1 ./run.sh    # skip `apply` (LiteLLM already running)
set -euo pipefail
cd "$(dirname "$0")"

AISLANG="${AISLANG:-aislang}"

if [[ "${1:-}" == "--offline" ]]; then
  echo "==> seeding synthetic prices (offline)…"
  python3 scripts/seed_demo.py >/dev/null
else
  echo "==> fetching prices (Yahoo / CoinGecko)…"
  python3 scripts/fetch.py >/dev/null
fi

echo "==> computing P&L / risk / anti-fragility metrics…"
# compact single-line JSON so it lands as a single chat turn.
REPORT="$(python3 scripts/analyze.py --persist --compact)"

echo "==> rendering dashboard.html…"
python3 scripts/render_dashboard.py
echo "    $(printf '%s' "$REPORT" | python3 -c 'import json,sys; r=json.load(sys.stdin); t=r["analytic"]["totals"]; s=r["strategic"]; tv=t["total_value"]; pnl=t["unrealized_pnl"]; fs=s["fragility_score"]; cl=s["classification"]; print(f"value {tv:.0f}  P&L {pnl:+.0f}  fragility {fs} ({cl})")')"

if [[ "${SKIP_APPLY:-}" != "1" ]]; then
  echo "==> starting the model router (LiteLLM) + building the RAG index (aislang apply)…"
  echo "    needs Docker + ANTHROPIC_API_KEY in ./.env  (Ollama bge-m3 optional, for RAG grounding)"
  "$AISLANG" apply portfolio.ais
fi

# Give LiteLLM a moment to accept connections on :4000 before we chat.
echo "==> waiting for LiteLLM on 127.0.0.1:4000…"
for _ in $(seq 1 30); do
  if curl -sf -o /dev/null "http://127.0.0.1:4000/health/liveness" 2>/dev/null; then
    echo "    LiteLLM is up."
    break
  fi
  sleep 2
done

# The keep/rebalance decision is already made — deterministically, by the rule
# engine in analyze.py — and was persisted to the DB and rendered on top of the
# dashboard above.  The advisor stage only *explains* that fixed recommendation
# in plain prose (Strategic brief -> Orchestrator explanation); it does not
# decide, and there is nothing further to persist or re-render.
echo "==> asking the advisor to explain the recommendation (Strategic -> Orchestrator)…"
echo
printf '%s\n' "$REPORT" | "$AISLANG" chat advise portfolio.ais || true

echo
echo "Open dashboard.html in a browser for the full view.  Tear the stack down"
echo "when you're done:  aislang destroy portfolio.ais"

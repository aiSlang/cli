#!/usr/bin/env bash
# portfolio-pnl — run the full advisory pipeline end to end:
#
#   fetch fresh prices  ->  compute metrics  ->  ask the agents for a
#   keep / rebalance recommendation.
#
# The deterministic stages (fetch, analyze) are plain Python; the final
# step pipes the metrics JSON into the aiSlang `advise` pipeline
# (Strategic -> Orchestrator).
#
# Prerequisites for the agent step:
#   * the `aislang` binary on PATH (override with AISLANG=/path/to/aislang)
#   * a chat model: set ANTHROPIC_API_KEY, or run Ollama with qwen3_8b
#   * Ollama serving bge-m3 for the RAG index (ollama pull bge-m3)
#
# Usage:
#   ./run.sh            # live prices (Yahoo / CoinGecko)
#   ./run.sh --offline  # seed synthetic prices instead (no network / no keys for fetch)
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
echo "    $(printf '%s' "$REPORT" | python3 -c 'import json,sys; r=json.load(sys.stdin); t=r["analytic"]["totals"]; s=r["strategic"]; print(f"value {t[\"total_value\"]:.0f}  P&L {t[\"unrealized_pnl\"]:+.0f}  fragility {s[\"fragility_score\"]} ({s[\"classification\"]})")')"

echo "==> asking the advisor (Strategic -> Orchestrator)…"
echo
printf '%s\n' "$REPORT" | "$AISLANG" chat advise portfolio.ais

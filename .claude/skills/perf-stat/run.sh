#!/usr/bin/env bash
# perf-stat — thin dispatcher; analysis lives in analyze.py (stdlib python).
# See .claude/skills/perf-stat/SKILL.md for the full design.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v python3 >/dev/null 2>&1; then
  echo "perf-stat: python3 not on PATH — install via apt/brew/pacman." >&2
  exit 1
fi

# All arguments are forwarded to analyze.py.  Default invocation
# (`run.sh` with no args) runs retrofit + report.
exec python3 "${script_dir}/analyze.py" "$@"

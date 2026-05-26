#!/usr/bin/env bash
# code-stat — emit Rust LoC delta against the last cached value.
#
# Usage:   .claude/skills/code-stat/run.sh
# Stdout:  one line, format `LoC (Rust): <prev> -> <curr> (+<delta>)`
# Stderr:  diagnostics on failure
# Side:    writes `<curr>` to .claude/skills/code-stat/cache.txt
#
# Mirrors the user's reference invocation:
#   cloc --fullpath --not-match-d=target .
# …and extracts the "code" column from the Rust row.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cache_file="${script_dir}/cache.txt"

if ! command -v cloc >/dev/null 2>&1; then
  echo "code-stat: cloc not on PATH — install with: apt install cloc / brew install cloc / pacman -S cloc" >&2
  exit 1
fi

# Run cloc from the project root. The script lives at
# .claude/skills/code-stat/, so the workspace root is three levels up.
workspace="$(cd "${script_dir}/../../.." && pwd)"

# Extract the "code" column from the `Rust` row. cloc's output table has
# six columns (Language, files, blank, comment, code) plus the SUM row.
# `awk '$1 == "Rust" { print $NF }'` picks the rightmost numeric column
# of the Rust row only — same number as the user's reference output.
curr=$(
  cd "$workspace" && \
  cloc --fullpath --not-match-d=target . 2>/dev/null \
    | awk '$1 == "Rust" { print $NF }'
)

if [[ -z "${curr}" ]]; then
  echo "code-stat: cloc produced no Rust row — is there any Rust code in $workspace ?" >&2
  exit 1
fi

# Strip thousands-separator commas if cloc ever emits them. Pin numeric.
curr="${curr//,/}"

# Read prior value, or seed with current for the first-ever run.
if [[ -f "${cache_file}" ]]; then
  prev=$(<"${cache_file}")
  prev="${prev//[^0-9]/}"   # defensive: strip non-digits
  [[ -z "${prev}" ]] && prev="${curr}"
else
  prev="${curr}"
fi

delta=$((curr - prev))
if [[ ${delta} -ge 0 ]]; then
  sign="+"
else
  sign=""    # negative numbers already carry their `-`
fi

echo "LoC (Rust): ${prev} -> ${curr} (${sign}${delta})"

# Atomic write: tempfile + rename so a Ctrl-C mid-write can't leave the
# cache in a half-written state. Same pattern as aislang's spend.json
# (Thursday's work).
tmp="${cache_file}.tmp.$$"
printf '%s\n' "${curr}" > "${tmp}"
mv -f "${tmp}" "${cache_file}"

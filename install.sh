#!/bin/sh
# aiSlang installer — PLAN-90-days.md §Week 11 Wed, Decision 37.
#
# Detects host OS+arch via `uname`, fetches the matching tarball from
# GitHub Releases, verifies its sha256 against `checksums.txt`, and
# installs the `aislang` binary into ${PREFIX:-$HOME/.local/bin}.
#
# Supported targets (Decision 36):
#   - Linux  x86_64 / aarch64
#   - macOS  x86_64 / arm64 (Apple Silicon)
#
# Dependencies: POSIX `sh`, `curl`, `tar`, `mktemp`, and either
# `sha256sum` (Linux) or `shasum` (macOS).  No bash, no jq.
#
# Environment-variable overrides:
#   REPO      — github "<owner>/<repo>"  (default: aislang/aislang)
#   VERSION   — tag to install           (default: latest)
#   PREFIX    — install dir              (default: $HOME/.local/bin)
#   BASE_URL  — release-asset base URL   (default: https://github.com/$REPO/releases/download)
#               Useful for rehearsing the installer against a local
#               HTTP mirror before tagging — the cold-VM rehearsal
#               (PLAN §Week 11 Thu) uses this. When BASE_URL is set,
#               VERSION must be a concrete tag (no "latest" resolve).
#
# Usage:
#   curl -sSfL https://github.com/aislang/aislang/releases/latest/download/install.sh | sh
#   PREFIX=/usr/local/bin sudo sh install.sh
#   VERSION=v0.1.0 sh install.sh
#
# Strategic note (Decision 34): through v0.1 and v0.2 this script
# is intentionally distributed via authenticated `gh release
# download` rather than a public curl one-liner — the script
# exists and the workflow attaches it to releases, but the
# public install URL only goes live with v0.3.

set -eu

# ── Defaults / env overrides ────────────────────────────────────
REPO="${REPO:-aislang/aislang}"
VERSION="${VERSION:-latest}"
PREFIX="${PREFIX:-$HOME/.local/bin}"
BASE_URL="${BASE_URL:-}"   # empty → derived from REPO in fetch_assets
BIN="aislang"

# ── tiny logging helpers ────────────────────────────────────────
info() { printf '\033[1;34m==>\033[0m %s\n' "$*" >&2; }
warn() { printf '\033[1;33m==>\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m==>\033[0m %s\n' "$*" >&2; exit 1; }

# ── ensure required tools are on PATH ───────────────────────────
ensure_deps() {
  for cmd in curl tar mktemp uname; do
    command -v "$cmd" >/dev/null 2>&1 \
      || die "required command not found: $cmd"
  done

  # sha256sum (linux coreutils) OR shasum (macOS / older bsd).
  if command -v sha256sum >/dev/null 2>&1; then
    SHA256_CMD='sha256sum'
  elif command -v shasum >/dev/null 2>&1; then
    SHA256_CMD='shasum -a 256'
  else
    die "neither sha256sum nor shasum found — cannot verify checksum"
  fi
}

# ── detect host triple ──────────────────────────────────────────
# Sets the global $TARGET to one of the four published targets.
detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux)
      case "$arch" in
        x86_64|amd64)         TARGET='x86_64-unknown-linux-gnu' ;;
        aarch64|arm64)        TARGET='aarch64-unknown-linux-gnu' ;;
        *) die "unsupported Linux arch: $arch (supported: x86_64, aarch64)" ;;
      esac ;;
    Darwin)
      case "$arch" in
        x86_64)               TARGET='x86_64-apple-darwin' ;;
        arm64|aarch64)        TARGET='aarch64-apple-darwin' ;;
        *) die "unsupported macOS arch: $arch (supported: x86_64, arm64)" ;;
      esac ;;
    MINGW*|MSYS*|CYGWIN*|Windows*)
      die "Windows is not supported in v0.1 — use WSL2 or wait for v0.2+." ;;
    *)
      die "unsupported OS: $os (supported: Linux, macOS)" ;;
  esac

  info "detected target: $TARGET"
}

# ── resolve VERSION (when "latest", query the redirect URL) ─────
# `releases/latest` returns a 302 to `releases/tag/<vX.Y.Z>`; we
# don't actually need to parse it because GitHub also serves the
# tarball under `releases/latest/download/<asset>`. But we DO want
# the resolved tag in the success message + the checksums URL.
resolve_version() {
  if [ "$VERSION" = "latest" ]; then
    if [ -n "$BASE_URL" ]; then
      die "VERSION=latest is not supported with BASE_URL override; pass a concrete tag."
    fi
    info "resolving latest release tag from github.com/$REPO …"
    # Follow redirects (-L), print final URL (-w), no body (-o /dev/null).
    final="$(curl -sSL -o /dev/null -w '%{url_effective}' \
      "https://github.com/$REPO/releases/latest")" \
      || die "failed to resolve latest release URL"
    # Final URL shape: .../releases/tag/v0.1.0
    VERSION="${final##*/}"
    if [ -z "$VERSION" ] || [ "$VERSION" = "latest" ]; then
      die "could not parse latest tag from: $final"
    fi
  fi
  info "installing version: $VERSION"
}

# ── fetch tarball + checksums into a tmpdir ─────────────────────
fetch_assets() {
  TMPDIR="$(mktemp -d -t aislang-install-XXXXXX)"
  trap 'rm -rf "$TMPDIR"' EXIT INT TERM

  # Strip leading 'v' for the filename (workflow produces
  # `aislang-0.1.0-<target>.tar.gz`, not `aislang-v0.1.0-…`).
  v_strip="${VERSION#v}"
  TARBALL="aislang-${v_strip}-${TARGET}.tar.gz"
  base="${BASE_URL:-https://github.com/$REPO/releases/download}/${VERSION}"

  info "downloading ${base}/${TARBALL}"
  curl -sSfL -o "$TMPDIR/$TARBALL" "$base/$TARBALL" \
    || die "failed to download tarball: $base/$TARBALL"

  info "downloading ${base}/checksums.txt"
  curl -sSfL -o "$TMPDIR/checksums.txt" "$base/checksums.txt" \
    || die "failed to download checksums.txt"
}

# ── verify sha256 against the published checksums file ──────────
verify_checksum() {
  cd "$TMPDIR"

  # Expected line: "<hash>  aislang-<ver>-<target>.tar.gz"
  expected="$(grep -F "  $TARBALL" checksums.txt | awk '{print $1}')"
  [ -n "$expected" ] || die "checksums.txt has no entry for $TARBALL"

  actual="$($SHA256_CMD "$TARBALL" | awk '{print $1}')"

  if [ "$expected" != "$actual" ]; then
    die "checksum mismatch for $TARBALL
       expected: $expected
       actual:   $actual"
  fi
  info "checksum verified: $actual"
  cd - >/dev/null
}

# ── extract + install ───────────────────────────────────────────
install_binary() {
  cd "$TMPDIR"
  tar -xzf "$TARBALL" \
    || die "failed to extract $TARBALL"

  # Tarball expands to `aislang-<version>-<target>/aislang`.
  v_strip="${VERSION#v}"
  src="$TMPDIR/aislang-${v_strip}-${TARGET}/${BIN}"
  [ -x "$src" ] || die "binary not found in tarball at expected path: $src"

  mkdir -p "$PREFIX" \
    || die "cannot create install dir: $PREFIX  (try PREFIX=/usr/local/bin sudo sh install.sh)"

  dest="$PREFIX/$BIN"
  info "installing → $dest"
  cp "$src" "$dest" \
    || die "cannot write to $dest  (try a different PREFIX or use sudo)"
  chmod +x "$dest"
  cd - >/dev/null
}

# ── success banner ──────────────────────────────────────────────
print_success() {
  installed_version="$("$PREFIX/$BIN" --version 2>/dev/null || echo '(could not run)')"
  printf '\n'
  info "installed: $installed_version  →  $PREFIX/$BIN"

  case ":$PATH:" in
    *":$PREFIX:"*)
      info "ready to use:  aislang --help" ;;
    *)
      warn "$PREFIX is not on your PATH."
      warn "add it with one of:"
      warn "  echo 'export PATH=\"$PREFIX:\$PATH\"' >> ~/.bashrc   # bash"
      warn "  echo 'export PATH=\"$PREFIX:\$PATH\"' >> ~/.zshrc    # zsh"
      warn "  echo 'fish_add_path $PREFIX'         >> ~/.config/fish/config.fish   # fish"
      ;;
  esac

  printf '\n'
  info "next steps:  https://github.com/$REPO#quick-taste"
  info "prereq:      Docker is required for \`aislang apply\` / \`destroy\`."
}

# ── main ────────────────────────────────────────────────────────
main() {
  info "aiSlang installer"
  ensure_deps
  detect_target
  resolve_version
  fetch_assets
  verify_checksum
  install_binary
  print_success
}

main "$@"

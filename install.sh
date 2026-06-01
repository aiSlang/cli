#!/bin/sh
# aiSlang installer — PLAN-90-days-v0_1.md §Week 11 Wed, Decision 37.
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
#   REPO         — github "<owner>/<repo>"  (default: aislang/aislang)
#   VERSION      — tag to install           (default: latest)
#   PREFIX       — install dir              (default: $HOME/.local/bin)
#   BASE_URL     — release-asset base URL   (default: https://github.com/$REPO/releases/download)
#                  Useful for rehearsing the installer against a local
#                  HTTP mirror before tagging — the cold-VM rehearsal
#                  (PLAN §Week 11 Thu) uses this. When BASE_URL is set,
#                  VERSION must be a concrete tag (no "latest" resolve).
#   GITHUB_TOKEN — Personal Access Token (Classic with `repo` scope,
#     or GH_TOKEN    or fine-grained with read-access to the repo).
#                  REQUIRED for private repos; ignored for public ones.
#                  Phase 2 Week 15 Fri (post-Phase-2-close) addition —
#                  unblocks cold-VM rehearsal against private forks
#                  before the v0.4 public-launch flip.  When set, the
#                  installer passes `Authorization: Bearer $TOKEN` on
#                  every curl call (latest-tag resolve + tarball
#                  fetch + checksums fetch).
#
# Flags:
#   --check-deps   — skip install, just probe runtime prerequisites
#                    (docker / docker compose / helm / kubectl / kind)
#                    and print a tailored install-hints block.
#                    Same probe runs at the end of a normal install.
#
# Usage:
#   # public repo (no token):
#   curl -sSfL https://github.com/aislang/aislang/releases/latest/download/install.sh | sh
#   PREFIX=/usr/local/bin sudo sh install.sh
#   VERSION=v0.1.0 sh install.sh
#   sh install.sh --check-deps        # probe-only, no install
#
#   # private repo / fork (token required for assets, NOT for the
#   # script itself if you've already cloned it — the bootstrap
#   # one-liner below uses the token for both):
#   GITHUB_TOKEN=ghp_XXXX REPO=aiSlang/cli \
#     curl -sSfL -H "Authorization: Bearer $GITHUB_TOKEN" \
#       "https://raw.githubusercontent.com/${REPO}/main/install.sh" \
#     | GITHUB_TOKEN=$GITHUB_TOKEN REPO=$REPO sh
#
# Strategic note (Decision 34): through v0.1 and v0.2 this script
# is intentionally distributed via authenticated `gh release
# download` rather than a public curl one-liner — the script
# exists and the workflow attaches it to releases, but the
# public install URL only goes live with v0.3.  v0.2 Phase 2
# close adds GITHUB_TOKEN support so authenticated cold-VM
# rehearsal works against private forks in the meantime
# (PLAN §"Three Phase-2-Week-14 lessons" #3 — procedure-only
# deliverables become live-execution-ready once cred-bearing
# users actually test).

set -eu

# ── Defaults / env overrides ────────────────────────────────────
REPO="${REPO:-aislang/aislang}"
VERSION="${VERSION:-latest}"
PREFIX="${PREFIX:-$HOME/.local/bin}"
BASE_URL="${BASE_URL:-}"   # empty → derived from REPO in fetch_assets
BIN="aislang"

# Either `GH_TOKEN` or `GITHUB_TOKEN` (former wins when both set,
# matching the `gh` CLI convention).  Empty when neither is set —
# triggers the public-repo curl path with no Authorization header.
TOKEN="${GH_TOKEN:-${GITHUB_TOKEN:-}}"

# ── tiny logging helpers ────────────────────────────────────────
info() { printf '\033[1;34m==>\033[0m %s\n' "$*" >&2; }
warn() { printf '\033[1;33m==>\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m==>\033[0m %s\n' "$*" >&2; exit 1; }

# ── authenticated curl wrapper ──────────────────────────────────
#
# Drop-in replacement for `curl …` that:
#   1. Adds `Authorization: Bearer $TOKEN` when TOKEN is non-empty.
#   2. Uses plain `-L` (NOT `--location-trusted`) so the
#      Authorization header is STRIPPED on the cross-origin redirect
#      from `github.com/…/releases/download/…` to the temporary
#      signed URL on `objects.githubusercontent.com`.
#
# Why this matters (corrected v0.3-prep — earlier v0.2 commit got it
# backwards): GitHub's private-repo release-asset flow uses TWO
# different auth schemes on the two hops:
#
#   hop 1: `github.com/.../releases/download/<tag>/<file>`
#          → expects `Authorization: Bearer <PAT>` to verify
#            the caller has repo-read access; responds 302 with
#            a Location header pointing at the signed CDN URL.
#
#   hop 2: `objects.githubusercontent.com/...?token=<signed>`
#          → uses QUERY-STRING signing (the `?token=` param +
#            related ones).  The CDN REJECTS an extra
#            Authorization header — observed 404 in the wild
#            on private repos when `--location-trusted` keeps
#            the Bearer through to this hop.
#
# Plain `-L` since curl 7.86 (Oct 2022, ubiquitous now) strips
# `Authorization` on cross-origin redirects by default.  Result:
# hop-1 gets Bearer auth (PAT verifies), hop-2 gets just the
# signed URL (no spurious Bearer that the CDN would reject).
auth_curl() {
  if [ -n "$TOKEN" ]; then
    curl -L \
         -H "Authorization: Bearer $TOKEN" \
         "$@"
  else
    curl -L "$@"
  fi
}

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
      # Soft-bail with explicit WSL2 guidance — running aislang
      # natively on Windows would require porting Docker shellouts,
      # MCP stdio framing, and ~5-15 tests. Since Docker Desktop on
      # Windows runs through WSL2 anyway, the Linux build of aislang
      # is what every Windows user actually needs — installed inside
      # WSL where ELF binaries are first-class.
      #
      # As of v0.2 Phase 1 Week 13 Mon (b), `install.ps1` is the
      # supported one-liner for Windows users; this branch fires
      # only if somebody piped install.sh into Git-Bash / MSYS /
      # Cygwin directly (bypassing the PowerShell wrapper).
      cat >&2 <<EOF

==> Windows is not natively supported.

    aiSlang depends on Docker, which on Windows runs through WSL2
    anyway — so the Linux build of aislang is what you want,
    installed inside WSL.  Use the PowerShell installer instead:

        irm https://github.com/${REPO}/releases/latest/download/install.ps1 | iex

    It checks for WSL2, installs aislang inside your default WSL
    distro, and prints the same success banner you'd see on Linux.

    One-time WSL2 setup (only if you haven't installed it yet),
    from an *elevated* PowerShell prompt (Run as Administrator):

        wsl --install

    Then reboot and re-run the install.ps1 one-liner above.

    If you'd rather skip the PowerShell wrapper and stay in a
    Linux shell, open WSL and re-run THIS script:

        wsl
        curl -sSfL https://github.com/${REPO}/releases/latest/download/install.sh | sh

EOF
      exit 1
      ;;
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
    # Follow redirects (auth_curl handles `-L` + Bearer-strip-on-CDN), print
    # final URL (-w), no body (-o /dev/null).  Token-aware so private
    # repos can resolve the latest tag via the same redirect chain.
    final="$(auth_curl -sS -o /dev/null -w '%{url_effective}' \
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
  auth_curl -sSf -o "$TMPDIR/$TARBALL" "$base/$TARBALL" \
    || die "failed to download tarball: $base/$TARBALL
       (private repo? set GITHUB_TOKEN / GH_TOKEN — see header comment)"

  info "downloading ${base}/checksums.txt"
  auth_curl -sSf -o "$TMPDIR/checksums.txt" "$base/checksums.txt" \
    || die "failed to download checksums.txt
       (private repo? set GITHUB_TOKEN / GH_TOKEN — see header comment)"
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
}

# ── Runtime-dependency probe (Phase 2 Week 14 Thu) ──────────────
#
# aiSlang itself is one static binary, but `aislang apply`/`destroy`
# shell out to docker (target = compose) or helm + kubectl (target =
# kubernetes).  Auto-installing these would mean choosing a distro-
# specific package manager and asking for sudo — too invasive for a
# `curl | sh` flow.  We DETECT instead: print what's present + what's
# missing + per-distro install hints, then exit 0.  Users who want to
# proceed without the missing tool can; users who don't can run the
# printed commands.
#
# Exits 0 always — never blocks the install on missing runtime deps.
check_runtime_deps() {
  printf '\n'
  info "runtime prerequisites probe (informational — never blocks install)"
  printf '\n'

  # ── docker (target = compose; also required by kind) ───────────
  if command -v docker >/dev/null 2>&1; then
    docker_ver="$(docker --version 2>/dev/null | head -n1)"
    printf '  \033[1;32m✓\033[0m docker         %s\n' "$docker_ver"
  else
    printf '  \033[1;31m✗\033[0m docker         (not found — required for target = compose AND kind)\n'
    DEP_MISSING_DOCKER=1
  fi

  # ── docker compose plugin (Compose v2 is a `docker compose` subcommand) ──
  if docker compose version >/dev/null 2>&1; then
    compose_ver="$(docker compose version --short 2>/dev/null || echo '?')"
    printf '  \033[1;32m✓\033[0m docker compose v%s\n' "$compose_ver"
  elif command -v docker-compose >/dev/null 2>&1; then
    printf '  \033[1;33m~\033[0m docker-compose (legacy v1 — works but consider upgrading to Docker Compose v2)\n'
  elif [ -z "${DEP_MISSING_DOCKER:-}" ]; then
    # docker present but compose subcommand missing — usually the
    # case on stripped Docker installs (no compose plugin).
    printf '  \033[1;31m✗\033[0m docker compose (plugin missing — required for target = compose)\n'
    DEP_MISSING_COMPOSE=1
  fi

  # ── helm (target = kubernetes) ─────────────────────────────────
  if command -v helm >/dev/null 2>&1; then
    helm_ver="$(helm version --short 2>/dev/null || echo '?')"
    printf '  \033[1;32m✓\033[0m helm           %s\n' "$helm_ver"
  else
    printf '  \033[1;31m✗\033[0m helm           (not found — required for target = kubernetes)\n'
    DEP_MISSING_HELM=1
  fi

  # ── kubectl (target = kubernetes) ──────────────────────────────
  if command -v kubectl >/dev/null 2>&1; then
    kubectl_ver="$(kubectl version --client -o json 2>/dev/null \
      | awk -F'"' '/gitVersion/ {print $4; exit}' || echo '?')"
    printf '  \033[1;32m✓\033[0m kubectl        %s\n' "${kubectl_ver:-?}"
  else
    printf '  \033[1;31m✗\033[0m kubectl        (not found — required for target = kubernetes)\n'
    DEP_MISSING_KUBECTL=1
  fi

  # ── kind (recommended local K8s for testing) ───────────────────
  if command -v kind >/dev/null 2>&1; then
    kind_ver="$(kind --version 2>/dev/null | head -n1)"
    printf '  \033[1;32m✓\033[0m kind           %s\n' "$kind_ver"
  else
    printf '  \033[1;33m~\033[0m kind           (not found — optional; recommended for local K8s testing)\n'
    DEP_MISSING_KIND=1
  fi

  # ── Optional but commonly needed ───────────────────────────────
  if command -v ollama >/dev/null 2>&1; then
    printf '  \033[1;32m✓\033[0m ollama         (local-model option for $0 dev runs)\n'
  fi

  # If anything's missing, print install hints scoped to the
  # detected OS.  Each block is a small recipe the user can copy-
  # paste; auto-running them would require sudo + assumption of
  # the package manager, both of which we deliberately avoid.
  if [ -n "${DEP_MISSING_DOCKER:-}${DEP_MISSING_COMPOSE:-}${DEP_MISSING_HELM:-}${DEP_MISSING_KUBECTL:-}${DEP_MISSING_KIND:-}" ]; then
    print_dep_install_hints
  else
    printf '\n'
    info "all runtime dependencies present — ready for both target = compose AND target = kubernetes."
  fi
}

# Print install hints for missing runtime deps, scoped to host OS.
# Called only when at least one DEP_MISSING_* flag is set.
print_dep_install_hints() {
  os="$(uname -s)"
  printf '\n'
  info "install hints for missing prerequisites:"
  printf '\n'

  case "$os" in
    Linux)
      # Detect distro family for better hints.  /etc/os-release is
      # POSIX-standard on Debian/Ubuntu/RHEL/Arch/etc.
      distro_id=''
      if [ -r /etc/os-release ]; then
        # shellcheck disable=SC1091
        distro_id="$(. /etc/os-release; echo "${ID_LIKE:-$ID}" | awk '{print $1}')"
      fi
      case "$distro_id" in
        debian|ubuntu)
          [ -n "${DEP_MISSING_DOCKER:-}" ] && cat >&2 <<'EOF'
  ── docker (Ubuntu/Debian) ────────────────────────────────────
    sudo apt update && sudo apt install -y docker.io docker-compose-v2
    sudo usermod -aG docker $USER && newgrp docker

EOF
          [ -n "${DEP_MISSING_COMPOSE:-}" ] && [ -z "${DEP_MISSING_DOCKER:-}" ] && cat >&2 <<'EOF'
  ── docker compose plugin (Ubuntu/Debian) ─────────────────────
    sudo apt install -y docker-compose-v2

EOF
          [ -n "${DEP_MISSING_KUBECTL:-}" ] && cat >&2 <<'EOF'
  ── kubectl ───────────────────────────────────────────────────
    curl -sSL -o /tmp/kubectl "https://dl.k8s.io/release/$(curl -sSL https://dl.k8s.io/release/stable.txt)/bin/linux/amd64/kubectl"
    sudo install -m 0755 /tmp/kubectl /usr/local/bin/kubectl

EOF
          [ -n "${DEP_MISSING_HELM:-}" ] && cat >&2 <<'EOF'
  ── helm 3 ────────────────────────────────────────────────────
    curl -sSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash

EOF
          [ -n "${DEP_MISSING_KIND:-}" ] && cat >&2 <<'EOF'
  ── kind (local K8s in Docker — for testing target = kubernetes) ─
    curl -sSL -o /tmp/kind https://kind.sigs.k8s.io/dl/v0.24.0/kind-linux-amd64
    sudo install -m 0755 /tmp/kind /usr/local/bin/kind
    kind create cluster        # spins up a working cluster in ~30s

EOF
          ;;
        rhel|fedora|centos|rocky|almalinux)
          [ -n "${DEP_MISSING_DOCKER:-}" ] && cat >&2 <<'EOF'
  ── docker (RHEL/Fedora) ──────────────────────────────────────
    sudo dnf install -y docker docker-compose-plugin
    sudo systemctl enable --now docker
    sudo usermod -aG docker $USER && newgrp docker

EOF
          [ -n "${DEP_MISSING_KUBECTL:-}" ] && cat >&2 <<'EOF'
  ── kubectl (RHEL/Fedora) ─────────────────────────────────────
    sudo dnf install -y kubectl

EOF
          [ -n "${DEP_MISSING_HELM:-}" ] && cat >&2 <<'EOF'
  ── helm 3 ────────────────────────────────────────────────────
    curl -sSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash

EOF
          [ -n "${DEP_MISSING_KIND:-}" ] && cat >&2 <<'EOF'
  ── kind ──────────────────────────────────────────────────────
    curl -sSL -o /tmp/kind https://kind.sigs.k8s.io/dl/v0.24.0/kind-linux-amd64
    sudo install -m 0755 /tmp/kind /usr/local/bin/kind

EOF
          ;;
        arch|manjaro)
          [ -n "${DEP_MISSING_DOCKER:-}" ] && cat >&2 <<'EOF'
  ── docker (Arch) ─────────────────────────────────────────────
    sudo pacman -S --needed docker docker-compose
    sudo systemctl enable --now docker
    sudo usermod -aG docker $USER && newgrp docker

EOF
          [ -n "${DEP_MISSING_KUBECTL:-}" ] && cat >&2 <<'EOF'
  ── kubectl + helm + kind (Arch) ──────────────────────────────
    sudo pacman -S --needed kubectl helm kind

EOF
          ;;
        *)
          warn "(unknown Linux distro: '$distro_id' — falling back to generic hints)"
          [ -n "${DEP_MISSING_DOCKER:-}" ] && warn "install docker:  https://docs.docker.com/engine/install/"
          [ -n "${DEP_MISSING_KUBECTL:-}" ] && warn "install kubectl: https://kubernetes.io/docs/tasks/tools/#kubectl"
          [ -n "${DEP_MISSING_HELM:-}" ] && warn "install helm:    https://helm.sh/docs/intro/install/"
          [ -n "${DEP_MISSING_KIND:-}" ] && warn "install kind:    https://kind.sigs.k8s.io/docs/user/quick-start/#installation"
          ;;
      esac
      ;;
    Darwin)
      # macOS via Homebrew — single-line install for all four
      # missing tools.  Docker Desktop is the easy GUI route; the
      # CLI-only `docker` formula works too if the user prefers no
      # GUI overhead.
      cat >&2 <<'EOF'
  ── macOS (Homebrew) ──────────────────────────────────────────
    brew install docker docker-compose helm kubectl kind
    # Or Docker Desktop (GUI): https://docs.docker.com/desktop/install/mac-install/
    open -a Docker         # if Docker Desktop installed

EOF
      ;;
    *)
      warn "(unknown OS: '$os' — see vendor pages for install instructions)"
      ;;
  esac

  printf '\n'
  info "after installing, re-run with --check-deps to verify:  sh install.sh --check-deps"
}

# ── main ────────────────────────────────────────────────────────
main() {
  # --check-deps flag short-circuits to the probe + install hints
  # without doing the binary install.  Useful for the cold-start
  # workflow: "I already have aislang; what runtime deps do I need
  # for `target = kubernetes`?"
  if [ "${1:-}" = "--check-deps" ]; then
    info "aiSlang runtime-dependency probe (--check-deps)"
    check_runtime_deps
    exit 0
  fi

  info "aiSlang installer"
  if [ -n "$TOKEN" ]; then
    # Print just the first 8 chars so a screen-share / log paste
    # doesn't leak the full token.  `cut` is POSIX, available
    # everywhere `sh` runs.
    token_prefix="$(printf '%s' "$TOKEN" | cut -c1-8)"
    info "GITHUB_TOKEN / GH_TOKEN detected (${token_prefix}…) — using authenticated download"
  fi
  ensure_deps
  detect_target
  resolve_version
  fetch_assets
  verify_checksum
  install_binary
  print_success
  check_runtime_deps
}

main "$@"

# Install

## One-liner

The installer detects your OS and architecture, fetches the matching tarball from GitHub Releases, verifies its sha256 checksum, and installs the `aislang` binary into `$HOME/.local/bin`:

```sh
curl -sSfL https://github.com/aiSlang/cli/releases/latest/download/install.sh | sh
```

## Private repo / fork

If you're installing from a private repository or fork, create a GitHub PAT — a Classic PAT with `repo` scope, or a fine-grained PAT with read access to the single repo — at <https://github.com/settings/tokens>, then:

```sh
GITHUB_TOKEN=ghp_XXXX REPO=aiSlang/cli \
  curl -sSfL -H "Authorization: Bearer $GITHUB_TOKEN" \
    "https://raw.githubusercontent.com/${REPO}/main/install.sh" \
  | GITHUB_TOKEN=$GITHUB_TOKEN REPO=$REPO sh
```

`GITHUB_TOKEN` (or `GH_TOKEN`) is honored for all release-asset downloads.

## Where it installs

The binary lands in `$HOME/.local/bin`. Make sure that directory is on your `PATH`. Note that GUI tools (like Claude Desktop) don't inherit your shell `PATH`, so when you point one of them at `aislang` you must use an absolute path — find it with `command -v aislang`.

## Check prerequisites

Probe for runtime prerequisites without installing anything. This reports on `docker`, `docker compose`, `helm`, `kubectl`, and `kind`, and prints per-distro install hints:

```sh
sh install.sh --check-deps
```

Prerequisites:

- **Docker** is required — the Compose deploy target and local chat both use it.
- **helm**, **kubectl**, and **kind** are only needed for the Kubernetes deploy target.

## Build from source

```sh
cargo build --release
./target/release/aislang --help
```

# CLI reference

The `aislang` binary is the single entry point for authoring, validating,
deploying, and operating aiSlang projects. Every command that takes a positional
`<file>` accepts either a single `.ais` file or a project directory containing
an `aislang.toml` (see [multi-module projects](./language.md#multi-module-projects)),
except where noted.

Run `aislang <command> --help` for the exhaustive flag list. Verbosity flags
(`-v`, `-vv`, `-vvv`) accumulate and print progressively more detail to stderr.

## `validate`

Parse, type-check, and resolve references in a project without touching the
network or writing any files.

**Flags**

- `-v` / `-vv` / `-vvv` — one-line resource counts / pretty-printed AST /
  full AST as JSON IR.

**Exit codes** — `0` when the source parses and type-checks; `1` on any parse,
type, or reference error (rendered with a source-aware snippet and caret).

```sh
aislang validate examples/support-agent.ais
# OK: examples/support-agent.ais parsed and type-checked

aislang validate examples/multi-module-demo/   # a project directory
```

## `plan`

Resolve models, reconcile the lockfile, and emit deployment artifacts. The
Compose YAML (or Kubernetes manifests) prints to stdout; supporting files
(`litellm_config.yaml`, `.env.example`, `aislang.lock`) are written to disk and
their paths reported on stderr. No stack is brought up.

**Flags**

- `-o, --output-dir <DIR>` — where to write generated artifacts. Defaults to
  the source file's directory.
- `--no-lock` — skip reading/writing `aislang.lock` and always re-resolve.
  Useful for a one-off plan that shouldn't touch the lockfile.
- `--smoke-test` — pipe the emitted YAML through `docker compose -f - config -q`
  to confirm Docker accepts the structure. Skips (exit 0) if Docker isn't on
  PATH.
- `--smoke-test-live` — bring the stack up with `docker compose up -d`, poll
  LiteLLM on port 4000, fire one real `/v1/chat/completions` against the first
  resolved model, then always `down -v` (even on failure). Implies
  `--smoke-test`. Requires Docker and a populated `.env` in the output
  directory.

```sh
aislang plan examples/support-agent.ais -o ./out
aislang plan examples/support-agent.ais --smoke-test
```

## `apply`

Emit the stack and deploy it. Under `target = compose` it writes `stack.yml`
and runs `docker compose up -d`, then records `.aislang/state.json`. Under
`target = kubernetes` it writes `manifests/` and `chart/`, then runs
`helm upgrade --install <deploy-label> chart/`. Both paths degrade gracefully
when their backend CLI is missing — the artifacts are written and the manual
command is printed.

```sh
aislang apply examples/support-agent.ais
# stack is up — next: aislang chat support examples/support-agent.ais
```

## `destroy`

Tear down the deployed stack and free its resources. Under Compose it runs
`docker compose down -v`; under Kubernetes, `helm uninstall <deploy-label>`.
The operation is idempotent — re-running against an already-torn-down stack
exits 0 with a "nothing to destroy" message. The `applied_at` field in
`.aislang/state.json` is the source of truth.

**Flags**

- `--force` — run the teardown even when the state file is absent or already
  marked torn down. Useful after manual edits to `.aislang/`.

```sh
aislang destroy examples/support-agent.ais
aislang destroy --force examples/support-agent.ais
```

## `eval`

Run the project's declared `eval` blocks against the deployed agent and print a
JSON `EvalReport` to stdout. Each case carries a `trace_url` that deep-links
into Jaeger.

**Flags**

- `-e, --eval <LABEL>` — run only the named eval. When omitted, every `eval`
  block runs.

**Exit codes** — `0` only when every eval meets its threshold; `1` otherwise.

```sh
aislang eval examples/support-agent.ais
aislang eval -e factual_accuracy examples/support-agent.ais
aislang eval examples/support-agent.ais | jq -r '.trace_url, .cases[].trace_url'
```

## `cost`

Print today's UTC spend — total plus per-model breakdown — read from
`~/.aislang/spend.json`, compared against the `budget` caps declared in the
source. Read-only, no network, no Docker.

```sh
aislang cost examples/support-agent.ais
```

## `status`

Render stack health from `.aislang/state.json` and `docker compose ps`,
optionally with spend sections appended.

**Flags**

- `--spend` — append today's total spend and the day-cap utilization.
- `--model-spend[=MODEL]` — append the by-model spend breakdown; pass
  `--model-spend=<id>` (note the `=`) to filter to one model, or the bare flag
  for all.
- `--main-spend` — append the by-`main`-block breakdown (the principal-LLM side
  of the cost model). Entries whose model uses `source = subscription` render
  as `subscription (no API charge)`.

**Exit codes**

- `0` — stack matches state (applied and healthy).
- `1` — drift: state missing, `applied_at` is null, or `compose ps` disagrees.
- `2` — Docker unavailable when a stack check was attempted.

```sh
aislang status examples/support-agent.ais
aislang status --spend --model-spend examples/support-agent.ais
aislang status --model-spend=anthropic.claude_haiku_4_5 examples/support-agent.ais
```

## `chat`

Open an interactive chat session against a deployed agent. The command
validates the agent exists, resolves its model and system prompt, spawns the
declared MCP servers as stdio children for the session, and connects to the
deployed LiteLLM. The tool-use loop is hard-capped at 8 iterations per turn.

**Arguments** — `<agent>` label followed by `<file>`.

**Flags**

- `--interactive` — for pipeline agents, pause between steps so you can
  hand-edit the previous step's persisted output before the next step reads it.
  Every step's reply is persisted under
  `<project>/.aislang/pipeline/<label>/` regardless. No-op for single-agent and
  router paths.
- `--keep-history` — keep the previous session's pipeline persistence
  directory instead of wiping it at session start.

```sh
aislang chat support examples/support-agent.ais
# type messages; /exit or Ctrl-D to leave
```

## `mcp-serve`

Start the aiSlang MCP server, exposing the CLI surface (`lang_spec`,
`examples`, `describe`, `validate`, `plan`, `apply`, `destroy`, `status`,
`chat_turn`, `eval`) as MCP tools so MCP-aware hosts can drive aiSlang without
using the terminal. See [MCP integration](./mcp.md).

**Flags**

- `--stdio` — use the stdio transport (default). This is what desktop hosts
  spawn as a child process. Mutually exclusive with `--http`.
- `--http <ADDR>` — serve the same tools over Streamable-HTTP at
  `http://<ADDR>/mcp`. Accepts any `host:port` (`0.0.0.0:8080`,
  `127.0.0.1:8080`, `[::1]:8080`). Suitable for cluster/network deployments
  with multiple clients.
- `--rag-build-dir <DIR>` — build the project's RAG indexes before serving
  (equivalent to `rag build-all`, tolerant of per-store failures). Used by the
  in-cluster agent runtime at startup.
- `--litellm-url <URL>` — the LiteLLM base URL the pre-serve RAG build embeds
  against.
- `--wait-litellm-secs <SECS>` — seconds to wait for LiteLLM to accept
  connections before the pre-serve build (`0` = don't wait). Only used with
  `--rag-build-dir`.

```sh
aislang mcp-serve --stdio
aislang mcp-serve --http 0.0.0.0:8080
```

## `skill run`

Run one declared `skill` block end-to-end, giving `.ais` authors a way to test
skills without writing Rust glue. Shell-exec skills run through the per-skill
sandbox (scrubbed PATH, command denylist, per-skill working directory).
MCP-delegate skills spawn the referenced `mcp_server` container via
`docker run -i`, route the call, then shut the child down. The typed skill
output is printed to stdout.

**Arguments** — `<skill-label>` and `<file>` (single-file or project dir).

**Flags**

- `--args <JSON>` — JSON arguments for MCP-delegate skills (e.g.
  `{"path":"/data/hello.txt"}`). Ignored for shell-exec skills. Defaults to
  `{}`.

**Exit codes** — `0` on success; `1` with the rendered error on any skill
failure (denylist match, non-zero exit, MCP failure).

```sh
aislang skill run list_files examples/support-agent.ais
aislang skill run read_doc examples/support-agent.ais --args '{"path":"/data/hello.txt"}'
```

## `rag`

RAG runtime operations against declared `vector_store` blocks. All subcommands
take a `<file>` or project directory and default the LiteLLM proxy URL to
`http://localhost:4000`.

**Subcommands**

- `rag build <label> <file>` — build one vector store's index from its `source`
  URI, embed every chunk via the referenced `embedding` block, and persist to
  `<project>/.aislang/vector/<label>.bin`.
- `rag query <label> <file> <query>` — embed the query and return the top-K
  matching chunks (default 3, override with `--top-k <N>`) by cosine
  similarity. Builds the index on first call if it doesn't exist yet.
- `rag verify <file>` — scan every declared vector store and report chunk
  counts per persisted index. Exits non-zero if any index is empty or missing.
- `rag build-all <file>` — build every declared vector store in one go,
  tolerating per-store failures (logged; exits 0 so a stack can still come up).
  Indexes land in `AISLANG_VECTOR_DIR` if set, else the project's `.aislang/`.

**Flags** — `--litellm-url <URL>` (all subcommands), `--top-k <N>` (`query`).

```sh
aislang rag build handbook examples/support-agent.ais
aislang rag query handbook examples/support-agent.ais "what is the return policy?"
aislang rag verify examples/support-agent.ais
aislang rag build-all examples/support-agent.ais
```

## `remote`

Delegate a CLI verb to a managed control plane instead of running it locally.
It is the same `aislang` binary self-hosted users run — only `--target`
differs. The control plane itself lives in a separate repository, `aiSlang/cloud`,
and is out of scope for these local, self-hosted docs; this command is simply
the client that talks to it. Verbs operate on a project by its server-stamped
id.

**Flags**

- `--target <URL>` — control-plane base URL (e.g. `https://api.example.com`).
- `--tenant <SLUG>` — tenant slug; defaults to `AISLANG_TENANT`, else `default`.
- `--token <TOKEN>` — bearer token; defaults to `AISLANG_CLOUD_TOKEN`.

**Subcommands** — `validate`, `plan`, `apply`, `destroy`, `status`, each taking
a project id and delegating the same-named operation to the control plane.

```sh
export AISLANG_CLOUD_TOKEN=…
aislang remote --target=https://api.example.com --tenant=acme validate p-abc123
aislang remote --target=https://api.example.com --tenant=acme apply    p-abc123
```

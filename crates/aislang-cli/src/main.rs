//! `aislang` — declarative AI infrastructure CLI.
//!
//! v0.1 subcommand surface (most are stubs until their scheduled week).
//! See PLAN-90-days-v0_1.md for the implementation schedule.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

// Week-12 follow-up: full version string is "<cargo-pkg-version>
// (<git-short-hash>[-dirty])" — built from CARGO_PKG_VERSION + the
// two AISLANG_GIT_* env vars injected by `build.rs`.  Splicing at
// compile time keeps the runtime --version reply zero-cost while
// making "exactly which code is running?" unambiguous during the
// v0.2 daily-build cadence.
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (", env!("AISLANG_GIT_HASH"), env!("AISLANG_GIT_DIRTY"), ")",
);

#[derive(Parser, Debug)]
#[command(
    name = "aislang",
    version = VERSION,
    about = "Declarative infrastructure for AI projects",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Parse + type-check + resolve references in a .ais file
    Validate {
        /// Path to the .ais project file
        file: PathBuf,
        /// Increase output verbosity. Accumulates (-v, -vv, -vvv):
        ///   -v   one-line summary of resource counts
        ///   -vv  pretty-printed AST (Rust Debug form)
        ///   -vvv full AST serialized as JSON — the IR consumed by
        ///        downstream stages (planner, resolver, emitter)
        #[arg(short, long, action = clap::ArgAction::Count)]
        verbose: u8,
    },
    /// Resolve models + reconcile lockfile + emit Compose YAML to stdout
    Plan {
        /// Path to the .ais project file
        file: PathBuf,
        /// Increase output verbosity (-v stage timings; -vv ResolvedProject
        /// Debug; -vvv combined JSON IR with ast+graph+resolution)
        #[arg(short, long, action = clap::ArgAction::Count)]
        verbose: u8,
        /// Skip lockfile read/write — always re-resolve. Useful when
        /// debugging or producing a one-off plan without touching
        /// `aislang.lock`.
        #[arg(long)]
        no_lock: bool,
        /// Directory to write generated artifacts (litellm_config.yaml,
        /// aislang.lock, .env.example). Defaults to the source file's
        /// directory. The emitted Compose YAML embeds absolute paths
        /// so the user can `docker compose -f - up` from anywhere.
        #[arg(short = 'o', long)]
        output_dir: Option<PathBuf>,
        /// After emitting, pipe the generated Compose YAML through
        /// `docker compose -f - config -q` to confirm Docker accepts
        /// the structure. Gracefully degrades when the docker CLI
        /// isn't on PATH (prints "skipped" — exit 0).
        #[arg(long)]
        smoke_test: bool,
        /// Live smoke test: actually brings the stack up with
        /// `docker compose up -d`, polls LiteLLM's readiness on
        /// port 4000, runs one real `/v1/chat/completions` against
        /// the first resolved model alias, then `down -v` (always,
        /// even on failure). Catches boot-time bugs that
        /// `--smoke-test` (config-only) misses — see PLAN week 6
        /// retrospective. Requires Docker on PATH + a populated
        /// `.env` in the output dir (for provider creds the chat
        /// call will use). Implies `--smoke-test`.
        #[arg(long)]
        smoke_test_live: bool,
    },
    /// Apply the plan — deploy the stack via Docker Compose
    Apply {
        file: PathBuf,
    },
    /// Tear down the deployed stack and free resources
    ///
    /// Idempotent: re-running destroy on an already-torn-down stack
    /// exits 0 with a "nothing to destroy" message (week 10 Wed,
    /// Decision 27). The state file at `.aislang/state.json` is the
    /// source of truth — its `applied_at` field flips to null after
    /// a successful tear-down. Re-applying recreates state.json with
    /// a fresh timestamp.
    Destroy {
        file: PathBuf,
        /// Run `docker compose down -v` against the stack.yml even when
        /// the state file is absent or marked as torn down. Useful for
        /// users who manually edited `.aislang/` away or who want to
        /// re-run cleanup as a belt-and-suspenders measure on a
        /// torn-down stack (Decision 29).
        #[arg(long)]
        force: bool,
    },
    /// Run declared evals against the deployed agent(s)
    ///
    /// `aislang eval [<eval-label>] <file>`. Label optional: without
    /// it, every `eval` block in the project runs. With it, only the
    /// named one. Overall exit code = 0 only when every eval passes
    /// its threshold (per Decision 16).
    Eval {
        /// Optional eval label to run. When omitted, all evals run.
        #[arg(long = "eval", short = 'e')]
        eval_label: Option<String>,
        /// Path to the .ais project file.
        file: PathBuf,
    },
    /// Show today's spend (per-model + total) and the budget caps for
    /// the declared agent(s)
    ///
    /// Reads `~/.aislang/spend.json` (the persistent per-day spend
    /// tracker written during `aislang chat` / `aislang eval` runs)
    /// and renders today's UTC total alongside the per-model breakdown
    /// (week 10 Tue's `by_model` attribution). Compares against the
    /// `budget { hard_cap_per_run, hard_cap_per_day }` from the
    /// project's .ais source. No network calls — read-only.
    Cost {
        file: PathBuf,
    },
    /// Show stack status, spend, and/or per-model breakdown for the
    /// deployed agent(s)
    ///
    /// Reads `.aislang/state.json` (project-local, written by apply)
    /// and `~/.aislang/spend.json` (global, written during chat/eval).
    /// With no flags: renders stack status (state file + docker compose
    /// ps). With `--spend`: appends today's UTC total. With
    /// `--model-spend [MODEL]`: appends today's by-model breakdown,
    /// optionally filtered to one model id.  With `--main-spend`
    /// (week 13 Thu): appends today's by-`main`-block breakdown
    /// (currently empty pending v0.3's OpenAI-compat adapter that
    /// would auto-record principal-LLM tokens).
    ///
    /// Exit codes (Decision 28):
    ///   0  stack matches state (or no flags + state present + applied_at Some)
    ///   1  drift — state missing, applied_at is None, or compose ps disagrees
    ///   2  docker unavailable when a stack check was attempted
    Status {
        file: PathBuf,
        /// Append today's total spend (read from `~/.aislang/spend.json`).
        #[arg(long)]
        spend: bool,
        /// Append today's by-model spend breakdown. Filter to one
        /// model id via `--model-spend=anthropic.claude_haiku_4_5`
        /// (note the `=` — required to disambiguate from the
        /// positional FILE argument), or pass bare `--model-spend`
        /// to show all models recorded today.
        #[arg(long = "model-spend", value_name = "MODEL",
              num_args = 0..=1, default_missing_value = "",
              require_equals = true)]
        model_spend: Option<String>,
        /// Append today's by-`main`-block spend breakdown — the
        /// principal-LLM side of the Track-A2 cost model.  Week 13
        /// Thu adds the schema + read path; auto-recording requires
        /// v0.3's OpenAI-compat `mcp-serve` adapter (until then the
        /// section reports an empty state + the v0.3 carry-over
        /// pointer, same shape `--model-spend` uses for Open-WebUI
        /// users whose chats bypass the SpendTracker).
        ///
        /// Source-conditional output: `main` entries whose declared
        /// `model.source = subscription` render `subscription
        /// (no API charge)` instead of a dollar amount; `source = api`
        /// entries render the same way as `--model-spend`.
        #[arg(long = "main-spend")]
        main_spend: bool,
    },
    /// Open a CLI chat session against a deployed agent
    ///
    /// v0.1 (week 6 Mon): validates the agent exists in the source
    /// project and prints a placeholder line. Chat loop (LiteLLM +
    /// MCP) lands Wednesday; tool-use loop lands Thursday.
    Chat {
        /// Agent label (e.g. `support`)
        agent: String,
        /// Path to the .ais project file. v0.2 may infer this from
        /// `aislang.lock` in the current directory.
        file:  PathBuf,
    },
    /// Run the aiSlang MCP server (v0.2 Track A — week 12 Mon scaffold)
    ///
    /// Starts an MCP server that exposes the aislang CLI surface as
    /// MCP tools, so MCP-aware LLM clients (Claude Desktop, Cursor,
    /// ChatGPT-with-MCP, custom rmcp clients) can author and operate
    /// aiSlang stacks without learning the DSL.
    ///
    /// **Week 12 Mon (this commit):** scaffolding only — registers a
    /// single `aislang_ping` health-check tool that returns "pong".
    /// The substantive tool surface (lang_spec / examples / describe /
    /// validate / plan / apply / destroy / status / chat_turn / eval)
    /// lands Tue–Thu per PLAN-90-days-v0_2.md Phase 1 Week 12.
    ///
    /// **Transports:** `--stdio` (default — for parent-spawned use by
    /// Claude Desktop, Cursor, etc.) is wired week 12 Mon.  `--http
    /// <ADDR>` (week 13 Mon) hosts the same tool surface over rmcp's
    /// Streamable-HTTP transport at `http://ADDR/mcp`; suitable for
    /// k8s/cloud deployments where the server is reached by N
    /// independent MCP clients over the network.  The Session pool
    /// (chat_turn / eval) is shared across HTTP clients — two
    /// clients hitting the same `(source_dir, agent_label)` pair
    /// reuse the same in-process conversational Session, same as
    /// stdio (extended from N=1 to N).
    ///
    /// Typical local invocation comes from the client's MCP config,
    /// not from the user shell directly — but `aislang mcp-serve` IS
    /// runnable from a TTY for testing (it'll block on stdin reads
    /// or wait for HTTP connections, which is the point).
    #[command(name = "mcp-serve")]
    McpServe {
        /// Use the stdio transport (default). Mutually exclusive
        /// with `--http`. When neither flag is provided, stdio
        /// wins; when both are provided, clap rejects the call.
        #[arg(long, conflicts_with = "http", default_value_t = true)]
        stdio: bool,
        /// Use the Streamable-HTTP transport at the given bind
        /// address. Accepts any `host:port` form `SocketAddr`
        /// parses — `127.0.0.1:8080`, `0.0.0.0:8080`, `[::1]:8080`,
        /// etc. The MCP endpoint is mounted at `/mcp` (the rmcp
        /// convention); a client URL therefore looks like
        /// `http://<host>:<port>/mcp`.
        #[arg(long, value_name = "ADDR", conflicts_with = "stdio")]
        http: Option<SocketAddr>,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            let msg = format!("{e:#}");
            // The sentinel "(see error above)" signals: subcommand
            // already rendered its own diagnostic (e.g. validate's
            // source-aware snippet output); skip the redundant
            // "aislang: …" prefix.
            if msg != "(see error above)" {
                eprintln!("aislang: {msg}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    // Most subcommands return `Result<()>` and exit 0 on success;
    // `status` returns `Result<ExitCode>` so it can surface
    // Decision 28's 0/1/2 spectrum. `.map(|_| ExitCode::SUCCESS)`
    // unifies the two shapes for the dispatch.
    let code = match cli.command {
        Command::Validate { file, verbose }            =>
            run_validate(file, verbose).map(|_| ExitCode::SUCCESS)?,
        Command::Plan { file, verbose, no_lock, output_dir, smoke_test, smoke_test_live } =>
            run_plan(file, verbose, no_lock, output_dir, smoke_test, smoke_test_live)
                .map(|_| ExitCode::SUCCESS)?,
        Command::Apply { file }      => run_apply(file).map(|_| ExitCode::SUCCESS)?,
        Command::Destroy { file, force } => run_destroy(file, force).map(|_| ExitCode::SUCCESS)?,
        Command::Eval { eval_label, file } => run_eval(eval_label, file).map(|_| ExitCode::SUCCESS)?,
        Command::Cost { file }       => run_cost(file).map(|_| ExitCode::SUCCESS)?,
        Command::Status { file, spend, model_spend, main_spend } =>
            run_status(file, spend, model_spend, main_spend)?,
        Command::Chat { agent, file } => run_chat(agent, file).map(|_| ExitCode::SUCCESS)?,
        Command::McpServe { stdio, http } =>
            run_mcp_serve(stdio, http).map(|_| ExitCode::SUCCESS)?,
    };
    Ok(code)
}

// ============================================================================
// `aislang mcp-serve` — week 12 Mon scaffolding, week 13 Mon HTTP
// ============================================================================
//
// Week 12 Mon: registers a single `aislang_ping` tool over stdio.
// Validates the rmcp-server pipeline on a trivial case before
// scaling to 9 tool families Tue-Thu.  The actual server
// implementation lives in `aislang-runtime::mcp_server`; this
// function is the async-runtime bring-up + transport dispatch.
//
// Week 13 Mon (Decision 39 follow-through): HTTP/SSE transport
// alongside stdio.  Both transports drive the SAME `AislangServer`
// struct — only the wire integration differs.  `--http <ADDR>`
// brings up axum-hosted rmcp `StreamableHttpService` at
// `http://ADDR/mcp`.

/// Bring up the aiSlang MCP server.  Blocks until the connected
/// client disconnects (stdio EOF) or Ctrl-C signals shutdown (HTTP).
///
/// Runtime selection:
///   - **stdio** is inherently single-client.  A small
///     `current_thread` runtime is sufficient and saves a worker
///     thread.
///   - **http** serves N concurrent MCP clients.  We need
///     `multi_thread` so axum's connection accept loop, per-session
///     workers, and the !Send `Session::turn` `spawn_blocking`
///     workers can all overlap.
fn run_mcp_serve(stdio: bool, http: Option<SocketAddr>) -> Result<()> {
    if let Some(addr) = http {
        return run_mcp_serve_http(addr);
    }

    // Stdio is the default (clap's `default_value_t = true` on
    // `--stdio`).  If somebody forces `--stdio=false` without
    // `--http`, clap's `conflicts_with` won't catch it (--stdio is
    // just `false`); guard explicitly.
    if !stdio {
        return Err(anyhow::anyhow!(
            "no transport selected; pass either --stdio or --http <ADDR>"));
    }

    // Log the start to stderr — stdout belongs to the MCP wire
    // bytes and must not be polluted (a single rogue println! on
    // stdout corrupts the JSON-RPC framing).
    eprintln!("aislang mcp-serve: starting stdio MCP server …");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;

    rt.block_on(async {
        aislang_runtime::mcp_serve_stdio().await
            .map_err(|e| anyhow::anyhow!("mcp-serve: {e}"))
    })?;

    eprintln!("aislang mcp-serve: client disconnected; shutting down.");
    Ok(())
}

/// HTTP variant of [`run_mcp_serve`].
///
/// Binds a TCP listener at the user-supplied `addr`, hands it to
/// the runtime's `serve_http`, and serves until Ctrl-C.  Ctrl-C
/// fires a `CancellationToken` which axum uses for graceful
/// shutdown (drain active SSE streams, refuse new connections,
/// propagate cancellation into per-session rmcp workers).
///
/// The MCP endpoint is at `http://<addr>/mcp`.  We print the full
/// URL on startup so users can paste it into Claude Desktop's MCP
/// server config / Cursor / a raw `curl` directly.
fn run_mcp_serve_http(addr: SocketAddr) -> Result<()> {
    eprintln!("aislang mcp-serve: starting HTTP MCP server on http://{addr}/mcp …");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;

    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await
            .map_err(|e| anyhow::anyhow!("bind {addr}: {e}"))?;
        // Log the actual port (resolves the ephemeral case where
        // the user passed :0 — useful for ad-hoc local testing).
        let bound = listener.local_addr()
            .map_err(|e| anyhow::anyhow!("query bound addr: {e}"))?;
        eprintln!("aislang mcp-serve: listening on http://{bound}/mcp \
                   (Ctrl-C to shut down).");

        let ct = tokio_util::sync::CancellationToken::new();
        let ct_signal = ct.clone();
        tokio::spawn(async move {
            // Best-effort Ctrl-C handling.  If the OS doesn't
            // give us SIGINT, we fall back to running until the
            // listener errors out for some other reason.
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("aislang mcp-serve: Ctrl-C received; draining …");
                ct_signal.cancel();
            }
        });

        aislang_runtime::mcp_serve_http(listener, ct).await
            .map_err(|e| anyhow::anyhow!("mcp-serve: {e}"))
    })?;

    eprintln!("aislang mcp-serve: HTTP server shut down cleanly.");
    Ok(())
}

fn run_validate(file: PathBuf, verbose: u8) -> Result<()> {
    // v0.3 Week 16 Thu: dir-vs-file dispatch.  Multi-module inputs
    // go through the load_project_input pipeline (which runs parser
    // + check_multi + catalog-check internally), so we just summarize
    // the result here.
    if file.is_dir() {
        return run_validate_dir(file, verbose);
    }
    let source = std::fs::read_to_string(&file)?;

    // ── Stage 1: parse + typecheck (in aislang-core::validate) ──
    let t_start = std::time::Instant::now();
    let validate_result = aislang_core::validate(&source);
    let t_validate = t_start.elapsed();

    match validate_result {
        Ok((project, mut warnings)) => {
            // ── Stage 2: catalog ID validation (in aislang-resolver,
            // see PLAN §Week-1 dep-graph note for why it lives there). ──
            let t_cat_start = std::time::Instant::now();
            let cat_result  = aislang_resolver::check_against_bundled_catalog(&project);
            let t_catalog   = t_cat_start.elapsed();
            match cat_result {
                Ok(cat_warnings) => warnings.extend(cat_warnings),
                Err(e) => return Err(anyhow::anyhow!("catalog error: {e}")),
            }
            let t_total = t_start.elapsed();

            // Success line always goes to stdout (machine-readable surface).
            println!("OK: {} parsed and type-checked", file.display());

            // Warnings (stderr; exit code stays 0).
            if !warnings.is_empty() {
                eprintln!();
                eprintln!("warnings:");
                for w in &warnings {
                    eprintln!("  - {}", w.message);
                }
            }

            // Verbose output goes to stderr so stdout stays clean for
            // callers that pipe `aislang validate` into other tools.
            if verbose >= 1 {
                print_summary(&project);
                eprintln!();
                eprintln!("checks: ✓ parse  ✓ typecheck  ✓ catalog");
                eprintln!("elapsed: {:.2?}  (validate {:.2?} + catalog {:.2?})",
                    t_total, t_validate, t_catalog);
            }
            if verbose >= 2 {
                eprintln!();
                eprintln!("AST (Debug):");
                eprintln!("{project:#?}");

                eprintln!();
                eprintln!("Resource Graph (Debug):");
                match aislang_core::checker::graph_summary(&project) {
                    Ok(g)  => eprintln!("{g:#?}"),
                    Err(e) => eprintln!("(graph build failed: {e})"),
                }
            }
            if verbose >= 3 {
                eprintln!();
                eprintln!("AST + Resource Graph (JSON IR):");
                let graph = aislang_core::checker::graph_summary(&project).ok();
                let combined = serde_json::json!({
                    "ast":   &project,
                    "graph": graph,
                });
                match serde_json::to_string_pretty(&combined) {
                    Ok(json) => eprintln!("{json}"),
                    Err(e)   => eprintln!("(JSON serialization failed: {e})"),
                }
            }
            Ok(())
        }
        Err(validate_err) => {
            render_validate_error(&source, &validate_err);

            // Failure-path verbose diagnostic from week-2 Wed —
            // partial RawBlock tree when grammar succeeded but
            // conversion failed. Independent of source rendering.
            if verbose >= 1 {
                emit_failure_diagnostic(&source, verbose);
            }
            // We've already rendered the error message above; bubble a
            // sentinel up so main() exits non-zero without re-printing.
            Err(anyhow::anyhow!("(see error above)"))
        }
    }
}

/// Render a `ValidateError` using the source-aware diagnostic pipeline.
/// Shared by `run_validate` and `run_plan` so error output is identical
/// regardless of which command the user invoked.
///
/// v0.3 Week 16 Thu — multi-module validate.  Runs the same
/// load_project_input pipeline as `aislang plan` (parser +
/// check_multi + catalog-check per module) but doesn't emit
/// anything; success is the parsed + checked merged project.
fn run_validate_dir(dir: PathBuf, verbose: u8) -> Result<()> {
    let t_start = std::time::Instant::now();
    let loaded = load_project_input(&dir)?;
    let t_total = t_start.elapsed();

    println!("OK: {} parsed and type-checked ({} modules)",
        dir.display(), loaded.resolved.module_source_hashes.len());

    if !loaded.warnings.is_empty() {
        eprintln!();
        eprintln!("warnings:");
        for w in &loaded.warnings {
            eprintln!("  - {}", w.message);
        }
    }

    if verbose >= 1 {
        print_summary(&loaded.resolved.project);
        eprintln!();
        eprintln!("checks: ✓ parse  ✓ typecheck  ✓ catalog  ✓ cross-module");
        eprintln!("elapsed: {t_total:.2?}");
        eprintln!("modules:");
        for (m, h) in &loaded.resolved.module_source_hashes {
            eprintln!("  - {m} ({})", &h[..12]);
        }
    }
    if verbose >= 2 {
        eprintln!();
        eprintln!("Merged AST (Debug):");
        eprintln!("{:#?}", loaded.resolved.project);
    }
    Ok(())
}

/// - `Parse(errors)` with `Some(span)` → `diagnostic::render_with_source`
///   (line:col + 2-3-line snippet + caret).
/// - `Parse(errors)` with `None` (converter errors) → plain message.
/// - `Type` / `UnresolvedRef` → plain message (no AST spans yet).
fn render_validate_error(source: &str, err: &aislang_core::ValidateError) {
    match err {
        aislang_core::ValidateError::Parse(errors) => {
            for e in errors {
                match &e.span {
                    Some(span) => eprint!("{}", aislang_core::diagnostic::render_with_source(
                        source,
                        span.clone(),
                        aislang_core::diagnostic::Severity::Error,
                        &e.message,
                    )),
                    None => eprintln!("aislang: parse error: {}", e.message),
                }
            }
        }
        other => eprintln!("aislang: {other}"),
    }
}

// ============================================================================
// v0.3 Week 16 Thu — dir-vs-file dispatch for the project input
// ============================================================================

/// Output of [`load_project_input`].  Encapsulates everything the
/// downstream subcommands (plan / validate / apply) need to operate
/// on a project, regardless of whether the input was a single `.ais`
/// file or a directory holding an `aislang.toml` manifest.
struct LoadedProject {
    /// Resolved project (catalog bindings + prompt hashes).  For
    /// multi-module inputs this carries module-qualified keys per
    /// the Wed resolver contract.
    resolved: aislang_resolver::ResolvedProject,
    /// In-core validate warnings — accumulated across each module
    /// for multi-module inputs.
    warnings: Vec<aislang_core::Warning>,
    /// True when the input is a directory holding `aislang.toml`.
    /// Drives the K8s emitter's CM layout + the pod command line.
    is_multi_module: bool,
    /// Bag of ready-to-mount sources for the K8s emitter.  For
    /// single-file: `ais_source` populated.  For multi-module:
    /// `modules` + `manifest` populated.
    sources: aislang_k8s::ProjectSources,
    /// Root directory for lockfile + emitted-artifact placement.
    /// For single-file: the parent of the .ais file.  For multi-
    /// module: the manifest's containing dir.
    project_root: PathBuf,
    /// Per-input source bytes used by the lockfile reconciler.
    /// Single-file: the `.ais` content.  Multi-module: empty (the
    /// per-module hashes in `resolved.module_source_hashes` drive
    /// reconciliation via `reconcile_multi`).
    single_file_source: String,
    /// Project name used by state.json + display banners.  Single-
    /// file: the `project "<name>" { … }` block's label.  Multi-
    /// module: `aislang.toml`'s `[project] name` field.
    project_name: String,
}

impl LoadedProject {
    /// Deterministic content hash used by state.json + diff messages.
    /// Single-file: SHA-256 of the `.ais` source bytes (matches v0.2
    /// `Lockfile::source_hash`).  Multi-module: SHA-256 of a stable
    /// concatenation of every module's name + body bytes (matches
    /// the K8s pod-template `aislang.project-hash` annotation shape
    /// from `services::sha256_hex_of_multi_module`, minus the
    /// manifest/lockfile parts since state.json wants a hash of
    /// the SOURCE INPUTS only — manifest changes implicitly through
    /// module list changes, lockfile is derived).
    fn effective_source_hash(&self) -> String {
        if self.is_multi_module {
            // Concatenate every module's name + body bytes with the
            // same `module:` / `||` separator pattern the K8s emitter
            // uses for the pod-template `aislang.project-hash`
            // annotation (see `services::sha256_hex_of_multi_module`)
            // — minus manifest + lockfile which are derived inputs.
            let mut combined = Vec::with_capacity(2048);
            for (name, body) in &self.sources.modules {
                combined.extend_from_slice(b"module:");
                combined.extend_from_slice(name.as_bytes());
                combined.extend_from_slice(b"||");
                combined.extend_from_slice(body.as_bytes());
                combined.extend_from_slice(b"||");
            }
            aislang_runtime::hex_sha256(&combined)
        } else {
            aislang_runtime::hex_sha256(self.single_file_source.as_bytes())
        }
    }
}

/// Load a project from a CLI positional path argument.  If the path
/// is a file, the v0.2 single-file pipeline runs (read → validate →
/// catalog-check → resolve).  If the path is a directory, the v0.3
/// multi-module pipeline runs (load `aislang.toml` → read every
/// declared module → check_multi → resolve_multi).
///
/// Errors carry the input path so the user sees what went wrong
/// without having to guess.  Stage-by-stage logging mirrors the
/// single-file pipeline so verbose output looks the same regardless
/// of input shape.
fn load_project_input(input: &Path) -> Result<LoadedProject> {
    if input.is_dir() {
        load_multi_module_project(input)
    } else {
        load_single_file_project(input)
    }
}

fn load_single_file_project(file: &Path) -> Result<LoadedProject> {
    let source = std::fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    // `Path::parent` returns `Some("")` (NOT None) when the input is
    // a single-component relative path like `support-agent.ais` —
    // catch that case too so `canonicalize` doesn't choke on `""`.
    let source_dir = match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => std::path::Path::new("."),
    };

    let (project, mut warnings) = aislang_core::validate(&source)
        .map_err(|err| {
            render_validate_error(&source, &err);
            anyhow::anyhow!("(see error above)")
        })?;

    let cat_warnings = aislang_resolver::check_against_bundled_catalog(&project)
        .map_err(|e| anyhow::anyhow!("catalog error: {e}"))?;
    warnings.extend(cat_warnings);

    let resolved = aislang_resolver::resolve_against_bundled_catalog(
        &project, source_dir)
        .map_err(|e| anyhow::anyhow!("resolve error: {e}"))?;

    // Prompt bodies for the K8s emitter — same loader logic as the
    // legacy K8s apply path, hoisted up here so dir+file inputs
    // share one code path.
    let mut prompt_bodies = std::collections::BTreeMap::new();
    for prompt in &project.prompts {
        let abs = if prompt.file.is_absolute() {
            prompt.file.clone()
        } else {
            source_dir.join(&prompt.file)
        };
        let body = std::fs::read_to_string(&abs)
            .map_err(|e| anyhow::anyhow!(
                "read prompt `{}` from {}: {e}", prompt.label, abs.display()))?;
        prompt_bodies.insert(prompt.label.clone(), body);
    }

    let sources = aislang_k8s::ProjectSources {
        ais_source: source.clone(),
        prompt_bodies,
        ..Default::default()
    };

    let project_name = resolved.project.name.clone();
    Ok(LoadedProject {
        resolved,
        warnings,
        is_multi_module: false,
        sources,
        project_root: source_dir.to_path_buf(),
        single_file_source: source,
        project_name,
    })
}

fn load_multi_module_project(dir: &Path) -> Result<LoadedProject> {
    // ── Stage 0: load + validate the manifest.
    let manifest = aislang_core::manifest::load_manifest(dir)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .ok_or_else(|| anyhow::anyhow!(
            "no `aislang.toml` in directory `{}`; either point at a `.ais` \
             file directly or add a manifest declaring `[modules]`",
            dir.display()))?;
    let manifest_body = std::fs::read_to_string(
        dir.join(aislang_core::manifest::MANIFEST_FILE_NAME))
        .map_err(|e| anyhow::anyhow!("read aislang.toml: {e}"))?;

    // ── Stage 1: read + parse every module the manifest declares.
    let mut module_inputs: std::collections::BTreeMap<String,
        aislang_resolver::ModuleInput> = std::collections::BTreeMap::new();
    let mut module_projects: std::collections::BTreeMap<String,
        aislang_core::ast::Project> = std::collections::BTreeMap::new();
    let mut module_sources: std::collections::BTreeMap<String, String>
        = std::collections::BTreeMap::new();
    let mut module_prompt_bodies: std::collections::BTreeMap<String, String>
        = std::collections::BTreeMap::new();
    let mut warnings = Vec::new();

    for (name, rel_path) in &manifest.modules {
        let abs = dir.join(rel_path);
        let source = std::fs::read_to_string(&abs)
            .map_err(|e| anyhow::anyhow!(
                "module `{name}`: read {}: {e}", abs.display()))?;
        // v0.3 Week 16 Thu: parse-only per module — the v0.2-style
        // single-file `validate()` runs the checker which errors on
        // any cross-module `use <module>.<label>` ref (correctly so
        // for single-file inputs).  Multi-module checking happens
        // below via `check_multi(modules)` which has the cross-module
        // symbol table.
        let project = aislang_core::parser::parse(&source)
            .map_err(|errs| {
                eprintln!("module `{name}`:");
                let err = aislang_core::ValidateError::Parse(errs);
                render_validate_error(&source, &err);
                anyhow::anyhow!("(see error above)")
            })?;

        // Per-module prompts: read body, key under `<module>.<label>`
        // matching the Wed resolver's prompt-hash key convention.
        let module_dir = abs.parent().unwrap_or(dir).to_path_buf();
        for prompt in &project.prompts {
            let prompt_abs = if prompt.file.is_absolute() {
                prompt.file.clone()
            } else {
                module_dir.join(&prompt.file)
            };
            let body = std::fs::read_to_string(&prompt_abs)
                .map_err(|e| anyhow::anyhow!(
                    "module `{name}`: read prompt `{}` from {}: {e}",
                    prompt.label, prompt_abs.display()))?;
            module_prompt_bodies.insert(
                format!("{name}.{}", prompt.label), body);
        }

        module_projects.insert(name.clone(), project.clone());
        module_sources.insert(name.clone(), source.clone());
        module_inputs.insert(name.clone(), aislang_resolver::ModuleInput {
            project,
            source,
            source_dir: module_dir,
        });
    }

    // ── Stage 2: cross-module checker.
    let check_warnings = aislang_core::checker::check_multi(&module_projects)
        .map_err(|e| anyhow::anyhow!("validate error: {e}"))?;
    warnings.extend(check_warnings);

    // ── Stage 3: catalog-id check (per module — picks up resources
    // the bundled catalog doesn't carry).  Each module's
    // catalog-warning gets prefixed for clarity.
    for (name, project) in &module_projects {
        let cat = aislang_resolver::check_against_bundled_catalog(project)
            .map_err(|e| anyhow::anyhow!("module `{name}` catalog error: {e}"))?;
        warnings.extend(cat.into_iter().map(|w| aislang_core::Warning {
            message: format!("module `{name}`: {}", w.message),
        }));
    }

    // ── Stage 4: multi-module resolve.
    let catalog = aislang_catalog::bundled()
        .map_err(|e| anyhow::anyhow!("bundled catalog load: {e}"))?;
    let resolved = aislang_resolver::resolve_multi(&module_inputs, &catalog)
        .map_err(|e| anyhow::anyhow!("resolve error: {e}"))?;

    let sources = aislang_k8s::ProjectSources {
        ais_source:    String::new(),
        prompt_bodies: module_prompt_bodies,
        modules:       module_sources,
        manifest:      Some(manifest_body),
        ..Default::default()
    };

    Ok(LoadedProject {
        resolved,
        warnings,
        is_multi_module: true,
        sources,
        project_root: dir.to_path_buf(),
        single_file_source: String::new(),
        project_name: manifest.project.name.clone(),
    })
}

// ============================================================================
// `aislang plan` — Friday of week 4
// ============================================================================

fn run_plan(
    file:            PathBuf,
    verbose:         u8,
    no_lock:         bool,
    output_dir:      Option<PathBuf>,
    smoke_test:      bool,
    smoke_test_live: bool,
) -> Result<()> {
    let t_total_start = std::time::Instant::now();

    // v0.3 Week 16 Thu: dir-vs-file dispatch via load_project_input.
    // Single-file inputs follow the v0.2 pipeline byte-stably; dir
    // inputs walk every module declared in `aislang.toml` and merge
    // the resolved state.
    let t = std::time::Instant::now();
    let loaded = load_project_input(&file)?;
    let t_validate = t.elapsed();

    // Output dir defaults to the project root (file's parent OR
    // dir itself).  Canonicalised so the emitted Compose YAML
    // embeds an unambiguous host-side mount path.
    let output_dir: PathBuf = match output_dir {
        Some(p) => p,
        None    => loaded.project_root.clone(),
    };
    let output_dir = std::fs::canonicalize(&output_dir).map_err(|e| {
        anyhow::anyhow!("output dir `{}` doesn't exist or unreadable: {e}",
            output_dir.display())
    })?;

    let warnings  = loaded.warnings.clone();
    let resolved      = &loaded.resolved;
    let source        = loaded.single_file_source.as_str();
    let t_catalog     = std::time::Duration::ZERO;   // catalog folded
                                                     // into load_project_input
    let t_resolve     = std::time::Duration::ZERO;   // same

    // ── Stage 4: lockfile reconcile (unless --no-lock) ──
    // Lockfile lives under `output_dir`.  Multi-module projects use
    // `reconcile_multi` against per-module source hashes; single-file
    // projects use the v0.2 `reconcile()` path unchanged.
    let t = std::time::Instant::now();
    let lock_path = output_dir.join("aislang.lock");
    let lockfile_status: &str = if no_lock {
        "skipped (--no-lock)"
    } else {
        let existing = aislang_resolver::lockfile::read(&lock_path)
            .map_err(|e| anyhow::anyhow!("lockfile read error: {e}"))?;
        let diff = if loaded.is_multi_module {
            aislang_resolver::lockfile::reconcile_multi(
                existing.as_ref(),
                &resolved.catalog_version,
                &resolved.module_source_hashes,
                &resolved.prompt_hashes,
            )
        } else {
            aislang_resolver::lockfile::reconcile(
                source,
                existing.as_ref(),
                &resolved.catalog_version,
                &resolved.prompt_hashes,
            )
        };
        match diff {
            aislang_resolver::LockfileDiff::Match => {
                "match — reused cached bindings"
            }
            aislang_resolver::LockfileDiff::Fresh => {
                let new_lock = if loaded.is_multi_module {
                    aislang_resolver::Lockfile::build_multi(resolved)
                } else {
                    aislang_resolver::Lockfile::build(source, resolved)
                };
                aislang_resolver::lockfile::write(&lock_path, &new_lock)
                    .map_err(|e| anyhow::anyhow!("lockfile write error: {e}"))?;
                "fresh — wrote new lockfile"
            }
            aislang_resolver::LockfileDiff::Stale { reasons } => {
                if verbose >= 1 {
                    eprintln!();
                    eprintln!("lockfile stale — re-resolving. reasons:");
                    for r in &reasons {
                        eprintln!("  - {r}");
                    }
                }
                let new_lock = if loaded.is_multi_module {
                    aislang_resolver::Lockfile::build_multi(resolved)
                } else {
                    aislang_resolver::Lockfile::build(source, resolved)
                };
                aislang_resolver::lockfile::write(&lock_path, &new_lock)
                    .map_err(|e| anyhow::anyhow!("lockfile write error: {e}"))?;
                "stale — rewrote lockfile"
            }
        }
    };
    let t_lock = t.elapsed();

    // ── Stage 5: emit artifacts (Compose OR K8s, decided by deploy target) ──
    let t = std::time::Instant::now();
    let catalog = aislang_catalog::bundled()
        .map_err(|e| anyhow::anyhow!("catalog load error: {e}"))?;

    // Decision 43.1 (locked Phase 2 Sunday): one chart per deploy
    // block; the FIRST `deploy { target = kubernetes }` block wins.
    // Mixed-target projects (one compose + one k8s) emit BOTH artifact
    // sets — useful when transitioning a project from Compose to k8s.
    let has_k8s_target = resolved.project.deploys.iter()
        .any(|d| d.target == aislang_core::types::DeployTarget::Kubernetes);
    let has_compose_target = resolved.project.deploys.iter()
        .any(|d| d.target == aislang_core::types::DeployTarget::Compose);
    // Default (no deploy block at all) = Compose, to preserve the v0.1
    // behaviour where bare projects emit a `stack.yml` for the demo path.
    let emit_compose = has_compose_target || resolved.project.deploys.is_empty();
    let emit_k8s     = has_k8s_target;

    // Always-on: Compose path (when selected) writes
    // litellm_config.yaml + .env.example.
    let (compose_artifacts, litellm_config_path, env_example_path) = if emit_compose {
        let artifacts = aislang_compose::emit(&resolved, &catalog, &output_dir)
            .map_err(|e| anyhow::anyhow!("compose emit error: {e}"))?;
        let litellm_config_path = output_dir.join(aislang_compose::LITELLM_CONFIG_FILENAME);
        std::fs::write(&litellm_config_path, &artifacts.litellm_config)
            .map_err(|e| anyhow::anyhow!("write {}: {e}",
                litellm_config_path.display()))?;
        // .env.example: regenerate freely (it's a template). Distinct from
        // `.env` (user's secrets) — aislang plan must NEVER touch the
        // latter; that file is governed by docker compose at runtime.
        let env_example_path = output_dir.join(aislang_compose::ENV_EXAMPLE_FILENAME);
        std::fs::write(&env_example_path, &artifacts.env_example)
            .map_err(|e| anyhow::anyhow!("write {}: {e}",
                env_example_path.display()))?;
        (Some(artifacts), Some(litellm_config_path), Some(env_example_path))
    } else {
        (None, None, None)
    };

    // K8s emit path (Phase 2 Week 14 Tue+Wed, Decisions 43.2/43.3/43.4):
    // write <output_dir>/manifests/*.yaml — five files: litellm.yaml,
    // jaeger.yaml, budget-counter.yaml, agent-runtime.yaml,
    // aislang-project-config.yaml.  Thu adds openwebui.yaml (when
    // `expose.web_ui = true`) and the Helm chart wrapper at
    // <output_dir>/chart/.
    let mut k8s_manifest_paths: Vec<PathBuf> = Vec::new();
    let mut k8s_artifacts_for_stdout: Option<aislang_k8s::K8sArtifacts> = None;
    if emit_k8s {
        // v0.3 Week 16 Thu: ProjectSources was assembled in
        // load_project_input (single-file OR multi-module).  The
        // lockfile body comes from the file we just (re)wrote above.
        let lockfile_body = std::fs::read_to_string(&lock_path).ok();
        let mut sources = loaded.sources.clone();
        sources.lockfile = lockfile_body;

        let k8s_artifacts = aislang_k8s::emit(resolved, &catalog, &sources)
            .map_err(|e| anyhow::anyhow!("k8s emit error: {e}"))?;
        let manifests_dir = output_dir.join(aislang_k8s::MANIFESTS_DIRNAME);
        std::fs::create_dir_all(&manifests_dir)
            .map_err(|e| anyhow::anyhow!("create {}: {e}",
                manifests_dir.display()))?;
        for (name, body) in &k8s_artifacts.manifests {
            let path = manifests_dir.join(name);
            std::fs::write(&path, body)
                .map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
            k8s_manifest_paths.push(path);
        }
        // Thu (Decision 43.7): also write the Helm chart tree so
        // `aislang plan` materializes EVERY artifact `aislang apply`
        // would install.  Recreate chart/ from scratch on every
        // plan run so stale templates from a previous emit don't
        // leak through.
        let chart_dir = output_dir.join(aislang_k8s::CHART_DIRNAME);
        let _ = std::fs::remove_dir_all(&chart_dir);
        for (rel_path, body) in &k8s_artifacts.chart_files {
            let abs = chart_dir.join(rel_path);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).map_err(|e|
                    anyhow::anyhow!("create {}: {e}", parent.display()))?;
            }
            std::fs::write(&abs, body).map_err(|e|
                anyhow::anyhow!("write {}: {e}", abs.display()))?;
        }
        // Hand the artifacts off to the stdout-streaming branch
        // below so we don't pay for a second `emit()` call.
        k8s_artifacts_for_stdout = Some(k8s_artifacts);
    }

    let t_emit = t.elapsed();

    let t_total = t_total_start.elapsed();

    // ── Output: stdout content depends on emit path(s) ──
    //
    // Compose-only:  Compose YAML on stdout (pipeable into
    //                `docker compose -f - up`) — v0.1 contract.
    // K8s-only:      K8s manifests on stdout, `---`-separated
    //                (pipeable into `kubectl apply -f -`).
    // Both targets:  Compose YAML on stdout (v0.1 contract wins);
    //                k8s manifests on disk only.  Allows mixed-
    //                target projects without breaking the Compose
    //                stdout pipeline.
    if emit_compose {
        if let Some(artifacts) = &compose_artifacts {
            print!("{}", artifacts.docker_compose);
        }
    } else if emit_k8s {
        // K8s-only path: stream the same artifacts we just wrote to
        // disk as a concatenated YAML stream so `aislang plan |
        // kubectl apply -f -` works analogously to the Compose
        // pipeline.  Order matches the BTreeMap (alphabetical
        // filenames).
        if let Some(k8s_artifacts) = &k8s_artifacts_for_stdout {
            println!("# Generated by `aislang plan` — target = kubernetes");
            println!("# Apply: kubectl apply -f - (or use `aislang apply`, Thu)");
            for body in k8s_artifacts.manifests.values() {
                print!("{body}");
            }
        }
    }

    // ── Generated-file paths on stderr (so stdout stays pipe-clean) ──
    eprintln!();
    eprintln!("generated:");
    if let Some(p) = &litellm_config_path {
        eprintln!("  litellm config:  {}", p.display());
    }
    if let Some(p) = &env_example_path {
        eprintln!("  env example:     {}", p.display());
    }
    for p in &k8s_manifest_paths {
        eprintln!("  k8s manifest:    {}", p.display());
    }
    if emit_k8s {
        eprintln!("  helm chart:      {}",
            output_dir.join(aislang_k8s::CHART_DIRNAME).display());
    }
    if !no_lock {
        eprintln!("  lockfile:        {}", lock_path.display());
    }

    // ── Warnings on stderr after the YAML (so stdout stays clean) ──
    if !warnings.is_empty() {
        eprintln!();
        eprintln!("warnings:");
        for w in &warnings {
            eprintln!("  - {}", w.message);
        }
    }

    // ── Verbose output on stderr ──
    if verbose >= 1 {
        eprintln!();
        eprintln!("plan stages:");
        eprintln!("  validate   {:>10.2?}", t_validate);
        eprintln!("  catalog    {:>10.2?}", t_catalog);
        eprintln!("  resolve    {:>10.2?}", t_resolve);
        eprintln!("  lockfile   {:>10.2?}   {}", t_lock, lockfile_status);
        eprintln!("  emit       {:>10.2?}", t_emit);
        eprintln!("  ───────────────────");
        eprintln!("  total      {:>10.2?}", t_total);
    }
    if verbose >= 2 {
        eprintln!();
        eprintln!("ResolvedProject (Debug):");
        eprintln!("{resolved:#?}");
    }
    if verbose >= 3 {
        eprintln!();
        eprintln!("Full IR (JSON — ast + graph + resolution):");
        // v0.3 Thu: the AST is `resolved.project` (multi-module merge
        // for dir inputs; same Project shape for single-file).  The
        // graph_summary builds against that merged AST.
        let graph = aislang_core::checker::graph_summary(&resolved.project).ok();
        let combined = serde_json::json!({
            "ast":        &resolved.project,
            "graph":      graph,
            "resolution": &resolved,
        });
        match serde_json::to_string_pretty(&combined) {
            Ok(json) => eprintln!("{json}"),
            Err(e)   => eprintln!("(JSON serialization failed: {e})"),
        }
    }

    if smoke_test || smoke_test_live {
        eprintln!();
        eprintln!("smoke test{}:",
            if smoke_test_live { " (live tier)" } else { "" });
        // Smoke-test path is currently Compose-only (docker compose
        // config -q + live up/down).  When `target = kubernetes` and
        // no Compose target alongside it, skip with a notice — kind/
        // helm-lint based smoke lands Fri (Week 14 close).
        let Some(artifacts) = &compose_artifacts else {
            eprintln!("  smoke test for `target = kubernetes` lands Fri \
                       (kind + helm-lint axis).");
            eprintln!("  for now, eyeball manifests/ + run `kubectl --dry-run=client \
                       apply -f manifests/`.");
            return Ok(());
        };
        run_smoke_test(
            &artifacts.docker_compose,
            &output_dir,
            &resolved,
            smoke_test_live,
        )?;
    }

    Ok(())
}

/// Tiered smoke test. Tiers 1–2 run for `--smoke-test`; tiers 3–5
/// additionally run for `--smoke-test-live`.
///
///   1. `docker` not on PATH         → skip everything, exit 0.
///   2. `docker compose config -q`   → reject bad YAML; clean error
///                                     on non-zero exit.
///   3. `docker compose up -d`       → bring the stack up (live tier).
///   4. Readiness + chat completion  → poll TCP :4000, then `curl`
///                                     a real `/v1/chat/completions`
///                                     against the first resolved
///                                     model alias.
///   5. `docker compose down -v`     → ALWAYS, even on failure. The
///                                     cleanup wrapper makes sure
///                                     the host is left clean.
///
/// Tiers 3–5 catch boot-time bugs the config-only check misses —
/// week 6 alone exposed three (router_settings.fallbacks shape,
/// env-var forwarding into the litellm container, fake demo image).
/// PLAN week-6 retrospective elaborates.
fn run_smoke_test(
    compose_yaml: &str,
    output_dir:   &Path,
    resolved:     &aislang_resolver::ResolvedProject,
    live:         bool,
) -> Result<()> {
    // --- Tier 1: docker CLI present? ---
    let docker_present = std::process::Command::new("docker")
        .arg("--version").output();
    match docker_present {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout);
            eprintln!("  docker present:    {}", ver.trim());
        }
        _ => {
            eprintln!("  docker not found on PATH — skipping all checks.");
            eprintln!("  (install Docker Desktop or docker.io to enable smoke tests.)");
            return Ok(());
        }
    }

    // --- Tier 2: docker compose config -q over the emitted YAML ---
    smoke_test_tier2_config(compose_yaml)?;
    if !live {
        eprintln!("  up/down + LLM call: skipped (pass --smoke-test-live to run).");
        return Ok(());
    }

    // --- Tiers 3–5: live up/probe/chat/down ---
    smoke_test_live_tier(compose_yaml, output_dir, resolved)
}

/// Tier 2 — pipe `compose_yaml` to `docker compose -f - config -q`.
/// Errors out loudly with stderr captured if Docker rejects the YAML
/// (catches Compose-schema bugs but NOT LiteLLM-semantic ones).
fn smoke_test_tier2_config(compose_yaml: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("docker")
        .args(["compose", "-f", "-", "config", "-q"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn docker compose: {e}"))?;
    child.stdin.as_mut()
        .ok_or_else(|| anyhow::anyhow!("docker compose stdin not available"))?
        .write_all(compose_yaml.as_bytes())
        .map_err(|e| anyhow::anyhow!("write to docker compose stdin: {e}"))?;
    let output = child.wait_with_output()
        .map_err(|e| anyhow::anyhow!("wait for docker compose: {e}"))?;
    if output.status.success() {
        eprintln!("  compose validate:  ✓ Docker accepts the YAML");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("  compose validate:  ✗ Docker rejected the YAML:");
        for line in stderr.lines() {
            eprintln!("      {line}");
        }
        Err(anyhow::anyhow!(
            "docker compose config -q rejected the generated YAML \
             (exit {:?}); see lines above",
            output.status.code()))
    }
}

/// Project name used for the smoke-test stack. Distinct from the
/// user's typical `aislang-demo` so a parallel-running user stack
/// doesn't clash with the smoke test's containers (ports still
/// will — see the docs string on `smoke_test_live_tier`).
const SMOKE_TEST_PROJECT: &str = "aislang-smoketest";

/// Tiers 3–5: write the YAML to a temp file, `up -d`, poll for
/// readiness, fire one real chat completion against the first
/// resolved model alias, then `down -v` (always, regardless of
/// whether the inner phases succeeded).
///
/// Port clash caveat: if the user already has the demo stack
/// running on ports 4000/16686/4317/4318, `up -d` will fail with
/// "port already allocated" — surfaced as a normal smoke-test
/// error. Take down the existing stack (`docker compose down`)
/// before running `--smoke-test-live`. v0.2 may add ephemeral
/// port mappings for the smoke-test stack to avoid this.
fn smoke_test_live_tier(
    compose_yaml: &str,
    output_dir:   &Path,
    resolved:     &aislang_resolver::ResolvedProject,
) -> Result<()> {
    use std::process::Command;

    // Need the YAML on disk — `compose up -d` doesn't accept stdin
    // like `config -q` does. Place it under output_dir so any
    // referenced paths in the YAML (volume mounts, env_file) still
    // resolve.
    let stack_path = output_dir.join(".stack-smoketest.yml");
    std::fs::write(&stack_path, compose_yaml)
        .map_err(|e| anyhow::anyhow!(
            "write {}: {e}", stack_path.display()))?;

    // Look for an .env next to the stack for compose's variable
    // interpolation. Without it, OLLAMA_API_BASE etc. expand to
    // empty strings (compose warns, doesn't fail).
    let env_path  = output_dir.join(".env");
    let has_env   = env_path.exists();
    let env_arg: Option<String> = has_env
        .then(|| env_path.to_string_lossy().into_owned());

    // Cleanup wrapper — `down -v` MUST run even if a tier 4 phase
    // panics. We split into an inner async-style function and call
    // it, then unconditionally run cleanup at the end.
    let inner = || -> Result<()> {
        // Tier 3: up -d
        let mut up = Command::new("docker");
        up.args(["compose", "--project-name", SMOKE_TEST_PROJECT,
                 "-f", stack_path.to_string_lossy().as_ref()]);
        if let Some(ef) = env_arg.as_deref() {
            up.args(["--env-file", ef]);
        }
        up.args(["up", "-d"]);
        let out = up.output()
            .map_err(|e| anyhow::anyhow!("spawn `docker compose up`: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            eprintln!("  up:                ✗ docker compose up failed:");
            for line in stderr.lines() {
                eprintln!("      {line}");
            }
            return Err(anyhow::anyhow!(
                "docker compose up exited {:?}",
                out.status.code()));
        }
        eprintln!("  up:                ✓ stack started");

        // Tier 4a: TCP bind check — cheap "did the container start?"
        // probe. Catches container exits at boot.
        wait_for_port("127.0.0.1", 4000, std::time::Duration::from_secs(30))
            .map_err(|e| anyhow::anyhow!(
                "litellm did not bind :4000 within 30s: {e}\n\
                 (run `docker compose --project-name {} -f {} logs litellm` \
                 for boot diagnostics)",
                SMOKE_TEST_PROJECT, stack_path.display()))?;
        eprintln!("  litellm port:      ✓ tcp 127.0.0.1:4000 reachable");

        // Tier 4b: HTTP readiness check.
        //
        // The TCP bind happens FAST — FastAPI binds the listener in
        // ~1s. But LiteLLM's nested startup events (router init,
        // model_list parsing, fallback validation) take several more
        // seconds. During that window, TCP connections succeed but
        // any actual HTTP request gets `Connection reset by peer`
        // (server kills the conn before the route handlers exist).
        //
        // `/health/liveness` returns 200 ONLY after all startup
        // events complete — i.e. after the proxy is ready to route
        // chat completions. Poll until it's 200, then proceed.
        wait_for_litellm_ready(
            "http://127.0.0.1:4000",
            std::time::Duration::from_secs(30),
        ).map_err(|e| anyhow::anyhow!(
            "litellm /health/liveness did not return 200 within 30s: {e}\n\
             (run `docker compose --project-name {} -f {} logs litellm` \
             for boot diagnostics)",
            SMOKE_TEST_PROJECT, stack_path.display()))?;
        eprintln!("  litellm ready:     ✓ /health/liveness returned 200");

        // Tier 4c: fire one real chat completion.
        let first_model = resolved.model_bindings.keys().next()
            .ok_or_else(|| anyhow::anyhow!(
                "no resolved models — nothing to call /v1/chat/completions against"))?;
        try_chat_completion(first_model)
            .map_err(|e| anyhow::anyhow!(
                "chat completion against `{first_model}` failed: {e}\n\
                 (run `docker compose --project-name {} -f {} logs litellm` \
                 for diagnostics)",
                SMOKE_TEST_PROJECT, stack_path.display()))?;
        eprintln!("  chat completion:   ✓ /v1/chat/completions returned a reply");
        Ok(())
    };

    let result = inner();

    // Tier 5: down -v ALWAYS, regardless of inner result.
    let mut down = Command::new("docker");
    down.args(["compose", "--project-name", SMOKE_TEST_PROJECT,
               "-f", stack_path.to_string_lossy().as_ref()]);
    if let Some(ef) = env_arg.as_deref() {
        down.args(["--env-file", ef]);
    }
    down.args(["down", "-v"]);
    let down_out = down.output();
    match down_out {
        Ok(out) if out.status.success() => {
            eprintln!("  down -v:           ✓ stack torn down");
        }
        Ok(out) => {
            eprintln!("  down -v:           ⚠ docker compose down exited {:?}",
                out.status.code());
            let stderr = String::from_utf8_lossy(&out.stderr);
            for line in stderr.lines() {
                eprintln!("      {line}");
            }
        }
        Err(e) => {
            eprintln!("  down -v:           ⚠ spawn failed: {e}");
        }
    }
    let _ = std::fs::remove_file(&stack_path);

    result
}

/// Poll `127.0.0.1:port` (or whatever) until TCP connect succeeds,
/// or `timeout` elapses. Used to wait for LiteLLM's port to bind
/// during boot. Each attempt has a 1s connect-timeout so we don't
/// hang on a slow probe.
fn wait_for_port(host: &str, port: u16, timeout: std::time::Duration) -> Result<()> {
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    use std::time::Instant;
    let deadline = Instant::now() + timeout;
    let addr: SocketAddr = (host, port).to_socket_addrs()
        .map_err(|e| anyhow::anyhow!("resolve {host}:{port}: {e}"))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("no addrs for {host}:{port}"))?;
    let probe_timeout = std::time::Duration::from_secs(1);
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&addr, probe_timeout).is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(anyhow::anyhow!("timed out polling tcp {host}:{port}"))
}

/// Poll `<base_url>/health/liveness` until it returns 200, or
/// `timeout` elapses. LiteLLM's liveness endpoint returns 200 only
/// after all startup events complete — which is the actual
/// readiness signal for chat completions.
///
/// Why this exists alongside `wait_for_port`: TCP bind happens
/// fast (~1s) but startup events take several more seconds. During
/// that window TCP connects succeed but HTTP requests get
/// "Connection reset by peer" because the route handlers aren't
/// registered yet. The HTTP probe catches the actual ready state.
///
/// Uses shell `curl` (universal, same reason as `try_chat_completion`).
/// Each probe has a 2s connect-timeout so we don't hang on a single
/// slow attempt. Polls every 500ms.
fn wait_for_litellm_ready(
    base_url: &str,
    timeout:  std::time::Duration,
) -> Result<()> {
    use std::process::Command;
    use std::time::Instant;
    let url      = format!("{base_url}/health/liveness");
    let deadline = Instant::now() + timeout;
    let mut last_status = String::from("(no response yet)");
    while Instant::now() < deadline {
        let out = Command::new("curl")
            .args([
                "--silent", "--max-time", "2",
                "-o", "/dev/null",
                "-w", "%{http_code}",
                &url,
            ])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if code == "200" {
                    return Ok(());
                }
                last_status = format!("HTTP {code}");
            }
            Ok(o) => {
                let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                let exit = o.status.code();
                last_status = format!("curl exit {exit:?} (http={code})");
            }
            Err(e) => {
                last_status = format!("spawn curl: {e}");
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(anyhow::anyhow!(
        "timed out polling {url}; last attempt: {last_status}"))
}

/// Fire one minimal `/v1/chat/completions` request via `curl`. We
/// use the shell `curl` instead of dragging reqwest into the CLI
/// crate — it's installed on every dev / CI Linux/Mac/WSL machine
/// and keeps the CLI's dep graph small. Times out after 60s
/// (cold-start Ollama loading a fresh model can be slow on first
/// hit).
fn try_chat_completion(model_alias: &str) -> Result<()> {
    use std::process::Command;
    let body = serde_json::json!({
        "model": model_alias,
        "messages": [{"role": "user", "content": "say 1 in one short sentence"}],
    });
    let body_str = serde_json::to_string(&body)?;
    let out = Command::new("curl")
        .args([
            "--silent", "--show-error", "--fail-with-body",
            "--max-time", "60",
            "-H", "Content-Type: application/json",
            "-X", "POST",
            "-d", &body_str,
            "http://127.0.0.1:4000/v1/chat/completions",
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("spawn curl: {e}"))?;
    if !out.status.success() {
        let body = String::from_utf8_lossy(&out.stdout);
        let err  = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow::anyhow!(
            "curl exit {:?}; stderr: {}; body: {}",
            out.status.code(), err.trim(), body.trim()));
    }
    // Parse the body — expect choices[0].message.content non-empty.
    let body = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!("response body not JSON: {e}; got: {body}"))?;
    let content = parsed.pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!(
            "response missing choices[0].message.content; body: {body}"))?;
    if content.is_empty() {
        return Err(anyhow::anyhow!(
            "model returned empty content; body: {body}"));
    }
    Ok(())
}

// ============================================================================
// `aislang apply` + `aislang destroy` — Friday of week 6
// ============================================================================
//
// `apply` runs the same plan pipeline as `aislang plan` but writes
// the Compose YAML to disk (`stack.yml`) instead of stdout, then
// invokes `docker compose up -d` on it. `destroy` finds the same
// `stack.yml` and runs `docker compose down -v`.
//
// Both commands gracefully degrade when Docker is missing: apply
// prints the path to the written stack.yml and exits 0 with a hint
// that the user can run docker compose themselves; destroy fails
// fast with a clear "docker not on PATH" message (nothing to do
// without docker — better to surface than silently no-op).

/// Bundle of artifacts produced by the plan pipeline. Shared by
/// `aislang plan` (which streams the YAML to stdout) and `aislang
/// apply` (which writes it to disk + hands it to docker compose).
struct PreparedStack {
    artifacts:           aislang_compose::ComposeArtifacts,
    output_dir:          PathBuf,
    litellm_config_path: PathBuf,
    env_example_path:    PathBuf,
    /// `Some` when a lockfile was written; `None` when `--no-lock`.
    /// Apply doesn't expose `--no-lock` in v0.1 — it always reconciles.
    lock_path:           Option<PathBuf>,
    /// Week 10 Tue: `aislang apply` reads these to populate the
    /// state file (`.aislang/state.json`).
    ///
    /// v0.3 Week 17 (post Week 16): `source_hash` is now the
    /// already-computed effective hash (single-file: SHA-256 of
    /// `.ais` bytes; multi-module: SHA-256 of stable concatenation
    /// of every module's name + body bytes — see
    /// [`LoadedProject::effective_source_hash`]).  Apply writes
    /// this directly to state.json; no second hashing pass.
    source_hash:         String,
    project_name:        String,
    catalog_version:     String,
}

/// Run the plan pipeline up to (and including) the `aislang-compose`
/// emit step. Writes litellm_config.yaml + .env.example + (optionally)
/// aislang.lock to `output_dir`. Returns the artifacts so callers can
/// route the compose YAML to stdout (plan) or to a stack.yml file
/// (apply).
///
/// This is the shared core of `run_plan` and `run_apply`. Errors
/// surface via the same source-aware path as `aislang validate`.
fn prepare_stack(
    file:       &Path,
    output_dir: Option<&Path>,
    no_lock:    bool,
    verbose:    u8,
) -> Result<PreparedStack> {
    // v0.3 Week 17 (follow-up to Week 16 Fri close): route through
    // load_project_input so multi-module dirs work end-to-end.  The
    // load_*_project helpers already handle parse + check_multi +
    // catalog-check + resolve for both single-file and multi-module
    // inputs, returning a uniform LoadedProject.
    let loaded = load_project_input(file)?;

    let output_dir_buf: PathBuf = match output_dir {
        Some(p) => p.to_path_buf(),
        None    => loaded.project_root.clone(),
    };
    let output_dir = std::fs::canonicalize(&output_dir_buf).map_err(|e| {
        anyhow::anyhow!("output dir `{}` doesn't exist or unreadable: {e}",
            output_dir_buf.display())
    })?;

    // Lockfile reconciliation — multi-module branches to reconcile_
    // multi (per-module hash diff); single-file keeps the v0.2 path.
    let lock_path = output_dir.join("aislang.lock");
    let lock_path_out: Option<PathBuf> = if no_lock {
        None
    } else {
        let existing = aislang_resolver::lockfile::read(&lock_path)
            .map_err(|e| anyhow::anyhow!("lockfile read error: {e}"))?;
        let diff = if loaded.is_multi_module {
            aislang_resolver::lockfile::reconcile_multi(
                existing.as_ref(),
                &loaded.resolved.catalog_version,
                &loaded.resolved.module_source_hashes,
                &loaded.resolved.prompt_hashes,
            )
        } else {
            aislang_resolver::lockfile::reconcile(
                &loaded.single_file_source, existing.as_ref(),
                &loaded.resolved.catalog_version,
                &loaded.resolved.prompt_hashes,
            )
        };
        match diff {
            aislang_resolver::LockfileDiff::Match => {}
            aislang_resolver::LockfileDiff::Fresh
            | aislang_resolver::LockfileDiff::Stale { .. } => {
                let new_lock = if loaded.is_multi_module {
                    aislang_resolver::Lockfile::build_multi(&loaded.resolved)
                } else {
                    aislang_resolver::Lockfile::build(
                        &loaded.single_file_source, &loaded.resolved)
                };
                aislang_resolver::lockfile::write(&lock_path, &new_lock)
                    .map_err(|e| anyhow::anyhow!("lockfile write error: {e}"))?;
            }
        }
        Some(lock_path)
    };
    // Emit + write supporting files.
    let catalog = aislang_catalog::bundled()
        .map_err(|e| anyhow::anyhow!("catalog load error: {e}"))?;
    let artifacts = aislang_compose::emit(&loaded.resolved, &catalog, &output_dir)
        .map_err(|e| anyhow::anyhow!("compose emit error: {e}"))?;
    let litellm_config_path = output_dir.join(aislang_compose::LITELLM_CONFIG_FILENAME);
    std::fs::write(&litellm_config_path, &artifacts.litellm_config)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", litellm_config_path.display()))?;
    let env_example_path = output_dir.join(aislang_compose::ENV_EXAMPLE_FILENAME);
    std::fs::write(&env_example_path, &artifacts.env_example)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", env_example_path.display()))?;

    if verbose >= 1 && !loaded.warnings.is_empty() {
        eprintln!("warnings:");
        for w in &loaded.warnings {
            eprintln!("  - {}", w.message);
        }
    }

    Ok(PreparedStack {
        artifacts,
        output_dir,
        litellm_config_path,
        env_example_path,
        lock_path: lock_path_out,
        source_hash:     loaded.effective_source_hash(),
        project_name:    loaded.project_name.clone(),
        catalog_version: loaded.resolved.catalog_version.clone(),
    })
}

/// `aislang apply <file>` — plan, then `docker compose up -d`.
fn run_apply(file: PathBuf) -> Result<()> {
    // Phase 2 Week 14 Thu — dispatch on the declared deploy target.
    // `target = kubernetes` deploys go through the helm path; v0.1
    // Compose path (the rest of this function) is unchanged.  v0.3
    // Week 16 follow-up: `peek_deploy_target` now accepts dir-input
    // too (multi-module manifest path).
    match peek_deploy_target(&file)? {
        aislang_core::types::DeployTarget::Kubernetes => {
            return run_apply_k8s(file);
        }
        aislang_core::types::DeployTarget::Compose => {
            // Fall through to the existing Compose flow.
        }
    }

    let prep = prepare_stack(&file, None, false, 0)?;

    // Write stack.yml so `docker compose -f stack.yml` (and the
    // companion `aislang destroy`) can find it.
    let stack_path = prep.output_dir.join(aislang_compose::STACK_YAML_FILENAME);
    std::fs::write(&stack_path, &prep.artifacts.docker_compose)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", stack_path.display()))?;

    eprintln!("generated:");
    eprintln!("  stack:            {}", stack_path.display());
    eprintln!("  litellm config:   {}", prep.litellm_config_path.display());
    eprintln!("  env example:      {}", prep.env_example_path.display());
    if let Some(p) = &prep.lock_path {
        eprintln!("  lockfile:         {}", p.display());
    }
    eprintln!();

    // Now bring the stack up.
    if !docker_on_path() {
        eprintln!("docker not on PATH — wrote artifacts but didn't bring stack up.");
        eprintln!("install Docker and run:");
        eprintln!("  docker compose -f {} up -d", stack_path.display());
        return Ok(());
    }
    eprintln!("bringing stack up: docker compose -f {} up -d", stack_path.display());
    let status = std::process::Command::new("docker")
        .args(["compose", "-f"])
        .arg(&stack_path)
        .args(["up", "-d"])
        .status()
        .map_err(|e| anyhow::anyhow!("invoke docker compose: {e}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "docker compose up -d exited {:?}", status.code()));
    }

    // ── Write `.aislang/state.json` (week 10 Tue, Decision 26) ──
    // Recorded after the stack is confirmed up so a failed
    // `docker compose up -d` doesn't leave behind a misleading
    // "applied" state. Failures here surface as a warning rather
    // than aborting the apply — the stack IS up; we just couldn't
    // persist the audit-trail record. Users can re-run apply or
    // file the missing state manually.
    let state_path = aislang_runtime::state_path_in(&prep.output_dir);
    let state = aislang_runtime::new_applied_state(
        &prep.project_name,
        prep.source_hash.clone(),
        &prep.catalog_version,
        stack_path.clone(),
    );
    match aislang_runtime::write_state_atomic(&state_path, &state) {
        Ok(()) => {
            eprintln!("  state:     {}", state_path.display());
        }
        Err(e) => {
            eprintln!(
                "warning: stack is up but failed to write {}: {e}",
                state_path.display());
        }
    }

    eprintln!();
    eprintln!("stack is up. next steps:");
    if let Some(agent) = first_agent_label_in_file(&file) {
        eprintln!("  chat:      aislang chat {agent} {}", file.display());
    }
    eprintln!("  teardown:  aislang destroy {}", file.display());
    Ok(())
}

/// `aislang destroy <file> [--force]` — locate the stack.yml emitted
/// by apply and run `docker compose down -v`. Idempotent per
/// Decision 27 + 29 (week 10 Wed): re-running on an already-torn-down
/// stack exits 0; absent stack.yml AND absent state file is also a
/// clean exit 0. `--force` bypasses state-file existence checks and
/// runs the compose down anyway (when stack.yml exists).
fn run_destroy(file: PathBuf, force: bool) -> Result<()> {
    // Phase 2 Week 14 Thu — dispatch on the declared deploy target.
    // `target = kubernetes` deploys go through `helm uninstall`; v0.1
    // Compose path (the rest of this function) is unchanged.
    match peek_deploy_target(&file)? {
        aislang_core::types::DeployTarget::Kubernetes => {
            return run_destroy_k8s(file, force);
        }
        aislang_core::types::DeployTarget::Compose => {
            // Fall through to the existing Compose flow.
        }
    }

    // v0.3 Week 16 follow-up: dir input → output_dir IS the project
    // dir (where aislang.toml + emitted artifacts live).  File input
    // → parent dir, matching v0.2.  `Path::parent` returns Some("")
    // for single-component relative paths like `foo.ais`, so the
    // empty-string guard mirrors `load_single_file_project`.
    let source_dir: PathBuf = if file.is_dir() {
        file.clone()
    } else {
        match file.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        }
    };
    let output_dir = std::fs::canonicalize(&source_dir).map_err(|e| {
        anyhow::anyhow!("source directory `{}` not readable: {e}",
            source_dir.display())
    })?;
    let stack_path = output_dir.join(aislang_compose::STACK_YAML_FILENAME);
    let state_path = aislang_runtime::state_path_in(&output_dir);

    // ── Idempotency check (Decision 27) ─────────────────────────
    // The state file's `applied_at` is the source of truth for
    // "is this stack currently considered up". --force skips this
    // check entirely.
    if !force {
        match aislang_runtime::read_state(&state_path) {
            Ok(Some(state)) if state.applied_at.is_none() => {
                // Already torn down. Idempotent success — important
                // for CI cleanup pipelines that re-run destroy without
                // tracking whether they've already run it.
                eprintln!("nothing to destroy: state file at `{}` shows the \
                           stack was already torn down.", state_path.display());
                eprintln!("(pass --force to run `docker compose down -v` \
                           against `{}` anyway.)", stack_path.display());
                return Ok(());
            }
            Ok(None) if !stack_path.exists() => {
                // No state file AND no stack.yml — nothing was ever
                // applied to this directory. Exit 0 with a hint.
                eprintln!("nothing to destroy: neither `{}` nor `{}` exists.",
                    state_path.display(), stack_path.display());
                eprintln!("(if the stack IS up but state was lost, pass \
                           --force to attempt cleanup via the stack.yml \
                           — you'll need to recreate stack.yml first via \
                           `aislang plan -o {}` or `aislang apply`.)",
                    output_dir.display());
                return Ok(());
            }
            Ok(_)  => {}   // applied_at = Some, OR state missing but stack.yml present — proceed.
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "read state file `{}`: {e}\n\
                     (pass --force to ignore the state file and tear \
                     down via stack.yml directly)",
                    state_path.display()));
            }
        }
    }

    // ── Stack.yml is required for the actual docker compose down ──
    if !stack_path.exists() {
        return Err(anyhow::anyhow!(
            "no stack.yml at `{}` — run `aislang apply {}` to recreate \
             it, then re-run destroy",
            stack_path.display(), file.display()));
    }
    if !docker_on_path() {
        return Err(anyhow::anyhow!(
            "docker not on PATH; cannot tear down stack at `{}`",
            stack_path.display()));
    }
    eprintln!("tearing down stack: docker compose -f {} down -v", stack_path.display());
    let status = std::process::Command::new("docker")
        .args(["compose", "-f"])
        .arg(&stack_path)
        .args(["down", "-v"])
        .status()
        .map_err(|e| anyhow::anyhow!("invoke docker compose: {e}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "docker compose down -v exited {:?}", status.code()));
    }

    // ── Mark state torn down (Decision 27) ──────────────────────
    // `applied_at` flips to None. No-op when state file is absent
    // (user is using --force without ever having had a state file,
    // e.g. they pre-date week 10). Failures here log-and-continue
    // — the stack IS down; we just couldn't update the audit-trail.
    if let Err(e) = aislang_runtime::mark_state_torn_down(&state_path) {
        eprintln!("warning: stack torn down but failed to update state \
                   file `{}`: {e}", state_path.display());
    }

    eprintln!();
    eprintln!("stack torn down — containers, networks, and volumes removed.");
    eprintln!("the stack.yml file is preserved; remove it manually if desired:");
    eprintln!("  rm {}", stack_path.display());
    Ok(())
}

// ============================================================================
// `aislang apply` / `aislang destroy` — K8s target (Phase 2 Week 14 Thu)
// ============================================================================

/// Apply path for `target = kubernetes` deploys (Decision 43.7 +
/// 43.8).  Mirrors `run_apply`'s shape but emits the K8s artifact
/// set + shells out to `helm upgrade --install` instead of `docker
/// compose up -d`.
///
/// Steps:
///   1. validate + resolve + lockfile (same as Compose path)
///   2. Build `ProjectSources` from .ais source + lockfile body +
///      per-prompt file bodies (Wed wiring re-used)
///   3. `aislang_k8s::emit` → manifests/ + chart/
///   4. Write manifests/*.yaml + chart/Chart.yaml + chart/values.yaml +
///      chart/templates/*.yaml under `<output_dir>/`
///   5. If `helm` on PATH: `helm upgrade --install <deploy-label>
///      <output_dir>/chart/`; else graceful-degrade with the manual
///      command printed
///   6. Write `.aislang/state.json` (chart dir recorded in
///      `stack_yaml_path` — repurposed for k8s; v0.3 may add a
///      proper target field)
fn run_apply_k8s(file: PathBuf) -> Result<()> {
    // v0.3 Week 16 follow-up: load_project_input handles single-file
    // (file arg) OR multi-module (dir arg).  The K8s emit then reads
    // `sources.is_multi_module()` and routes to the per-module CM
    // shape Thu wired in.
    let loaded = load_project_input(&file)?;
    let output_dir = std::fs::canonicalize(&loaded.project_root)
        .map_err(|e| anyhow::anyhow!(
            "project dir `{}` not readable: {e}",
            loaded.project_root.display()))?;

    // ── Lockfile (always reconciled in apply path — same posture
    //    as v0.1 Compose apply).  Multi-module branches to
    //    reconcile_multi + build_multi. ──
    let lock_path = output_dir.join("aislang.lock");
    {
        let existing = aislang_resolver::lockfile::read(&lock_path)
            .map_err(|e| anyhow::anyhow!("lockfile read error: {e}"))?;
        let diff = if loaded.is_multi_module {
            aislang_resolver::lockfile::reconcile_multi(
                existing.as_ref(),
                &loaded.resolved.catalog_version,
                &loaded.resolved.module_source_hashes,
                &loaded.resolved.prompt_hashes,
            )
        } else {
            aislang_resolver::lockfile::reconcile(
                &loaded.single_file_source, existing.as_ref(),
                &loaded.resolved.catalog_version,
                &loaded.resolved.prompt_hashes,
            )
        };
        match diff {
            aislang_resolver::LockfileDiff::Match => {}
            aislang_resolver::LockfileDiff::Fresh
            | aislang_resolver::LockfileDiff::Stale { .. } => {
                let new_lock = if loaded.is_multi_module {
                    aislang_resolver::Lockfile::build_multi(&loaded.resolved)
                } else {
                    aislang_resolver::Lockfile::build(
                        &loaded.single_file_source, &loaded.resolved)
                };
                aislang_resolver::lockfile::write(&lock_path, &new_lock)
                    .map_err(|e| anyhow::anyhow!("lockfile write error: {e}"))?;
            }
        }
    }

    // ── ProjectSources for the emitter — load_project_input already
    //    populated `ais_source` (single-file) or `modules` + `manifest`
    //    (multi-module) + prompt_bodies; we just attach the lockfile
    //    body that we may have just written. ──
    let lockfile_body = std::fs::read_to_string(&lock_path).ok();
    let mut sources = loaded.sources.clone();
    sources.lockfile = lockfile_body;

    // ── Emit. ──
    let catalog = aislang_catalog::bundled()
        .map_err(|e| anyhow::anyhow!("catalog load error: {e}"))?;
    let artifacts = aislang_k8s::emit(&loaded.resolved, &catalog, &sources)
        .map_err(|e| anyhow::anyhow!("k8s emit error: {e}"))?;

    // ── Stage 7: write manifests/ + chart/ to disk. ──
    let manifests_dir = output_dir.join(aislang_k8s::MANIFESTS_DIRNAME);
    std::fs::create_dir_all(&manifests_dir).map_err(|e|
        anyhow::anyhow!("create {}: {e}", manifests_dir.display()))?;
    for (name, body) in &artifacts.manifests {
        let p = manifests_dir.join(name);
        std::fs::write(&p, body).map_err(|e|
            anyhow::anyhow!("write {}: {e}", p.display()))?;
    }
    let chart_dir = output_dir.join(aislang_k8s::CHART_DIRNAME);
    // Recreate chart/ from scratch on every apply so stale
    // templates from a previous emit don't leak through (the chart
    // is a derived artifact + helm doesn't care if we re-overwrite).
    let _ = std::fs::remove_dir_all(&chart_dir);
    for (rel_path, body) in &artifacts.chart_files {
        let abs = chart_dir.join(rel_path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e|
                anyhow::anyhow!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(&abs, body).map_err(|e|
            anyhow::anyhow!("write {}: {e}", abs.display()))?;
    }

    // Pick the deploy label (== Helm release name per Decision 43.1).
    let release_name = loaded.resolved.project.deploys.iter()
        .find(|d| d.target == aislang_core::types::DeployTarget::Kubernetes)
        .map(|d| d.label.clone())
        .unwrap_or_else(|| "aislang".to_string());

    eprintln!("generated:");
    eprintln!("  manifests dir:   {}", manifests_dir.display());
    eprintln!("  chart dir:       {}", chart_dir.display());
    eprintln!("  lockfile:        {}", lock_path.display());
    eprintln!();

    // ── Stage 7b: preflight — required prerequisites (Decision 43.5). ──
    //
    // The chart's pods envFrom `aislang-providers` Secret with
    // `optional: false` — if the Secret doesn't exist, kubelet
    // refuses to start the pod (the pod sits Pending with a
    // `CreateContainerConfigError` event the user only sees
    // by running `kubectl describe pod`).  ALSO checks that each
    // referenced env var has a non-empty value (catches the
    // `--from-literal=KEY="$UNSET_VAR"` foot-gun where the user's
    // shell didn't have the env var set when they ran kubectl
    // create secret).  Both warnings are non-fatal: helm install
    // proceeds so the rest of the stack is in place when the user
    // fixes the Secret afterwards.
    let referenced_vars =
        aislang_compose::litellm::referenced_env_vars(&loaded.resolved);
    preflight_check_providers_secret(&referenced_vars);

    // ── Stage 8: shell to `helm upgrade --install` (or graceful-degrade). ──
    if !helm_on_path() {
        eprintln!("helm not on PATH — wrote artifacts but didn't install chart.");
        eprintln!("install Helm 3 (https://helm.sh) and run:");
        eprintln!("  helm upgrade --install {release_name} {}", chart_dir.display());
        eprintln!("  # … or apply the raw manifests:");
        eprintln!("  kubectl apply -f {}", manifests_dir.display());
        return Ok(());
    }
    eprintln!("installing chart: helm upgrade --install {release_name} {}",
        chart_dir.display());
    let status = std::process::Command::new("helm")
        .args(["upgrade", "--install", &release_name])
        .arg(&chart_dir)
        .status()
        .map_err(|e| anyhow::anyhow!("invoke helm: {e}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "helm upgrade --install exited {:?}", status.code()));
    }

    // ── Stage 9: write state.json (Decision 26 reuse).  For the k8s
    //    target the `stack_yaml_path` field holds the chart directory
    //    (where destroy can find the release name + manifests for the
    //    rare `helm uninstall` fallback to kubectl).
    let state_path = aislang_runtime::state_path_in(&output_dir);
    let state = aislang_runtime::new_applied_state(
        &loaded.project_name,
        loaded.effective_source_hash(),
        &loaded.resolved.catalog_version,
        chart_dir.clone(),
    );
    match aislang_runtime::write_state_atomic(&state_path, &state) {
        Ok(())  => eprintln!("  state:           {}", state_path.display()),
        Err(e)  => eprintln!("warning: chart is installed but failed to write {}: {e}",
                             state_path.display()),
    }

    eprintln!();
    eprintln!("chart installed.  next steps:");
    eprintln!("  status:           kubectl get all -l aislang.deploy={release_name}");
    eprintln!("  open webui:       kubectl port-forward svc/openwebui 3000:8080 \
                                    (then http://localhost:3000)");
    // `aislang chat` always dials LiteLLM at 127.0.0.1:4000 — the
    // local-Compose convention.  Under K8s, LiteLLM is a ClusterIP
    // service inside the cluster; without a port-forward the chat
    // attempt fails with "error sending request" against the local
    // address.  Surface the port-forward command so users have a
    // one-step "make chat work" recipe.  v0.3 carry-over: make
    // chat topology-aware so this hand-step goes away.
    if let Some(agent) = first_agent_label_in_file(&file) {
        eprintln!("  chat (k8s mode):");
        eprintln!("    1. kubectl port-forward svc/litellm 4000:4000   \
                       # in another terminal; leave running");
        eprintln!("    2. aislang chat {agent} {}", file.display());
    }
    eprintln!("  teardown:         aislang destroy {}", file.display());
    Ok(())
}

/// Destroy path for `target = kubernetes` deploys.  Mirrors
/// `run_destroy`'s shape but uses `helm uninstall <release>` instead
/// of `docker compose down -v`.
fn run_destroy_k8s(file: PathBuf, force: bool) -> Result<()> {
    let source = std::fs::read_to_string(&file)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    let (project, _) = match aislang_core::validate(&source) {
        Ok(v) => v,
        Err(err) => {
            render_validate_error(&source, &err);
            return Err(anyhow::anyhow!("(see error above)"));
        }
    };
    let source_dir = file.parent().unwrap_or_else(|| Path::new("."));
    let output_dir = std::fs::canonicalize(source_dir).map_err(|e| {
        anyhow::anyhow!("source directory `{}` not readable: {e}",
            source_dir.display())
    })?;
    let state_path = aislang_runtime::state_path_in(&output_dir);
    let chart_dir  = output_dir.join(aislang_k8s::CHART_DIRNAME);
    let release_name = project.deploys.iter()
        .find(|d| d.target == aislang_core::types::DeployTarget::Kubernetes)
        .map(|d| d.label.clone())
        .unwrap_or_else(|| "aislang".to_string());

    // ── Idempotency check (mirrors v0.1 Compose destroy). ──
    if !force {
        match aislang_runtime::read_state(&state_path) {
            Ok(Some(state)) if state.applied_at.is_none() => {
                eprintln!("nothing to destroy: state file at `{}` shows the \
                           chart was already uninstalled.", state_path.display());
                eprintln!("(pass --force to run `helm uninstall {release_name}` anyway.)");
                return Ok(());
            }
            Ok(None) if !chart_dir.exists() => {
                eprintln!("nothing to destroy: neither `{}` nor `{}` exists.",
                    state_path.display(), chart_dir.display());
                eprintln!("(if the chart IS installed but state was lost, pass \
                           --force to attempt `helm uninstall {release_name}` directly.)");
                return Ok(());
            }
            Ok(_)  => {}
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "read state file `{}`: {e}\n\
                     (pass --force to ignore the state file)",
                    state_path.display()));
            }
        }
    }

    if !helm_on_path() {
        return Err(anyhow::anyhow!(
            "helm not on PATH; cannot uninstall release `{release_name}`.  \
             Install Helm 3 (https://helm.sh) or use `kubectl delete -f {}` \
             to manually remove the resources.",
            output_dir.join(aislang_k8s::MANIFESTS_DIRNAME).display()));
    }
    eprintln!("uninstalling chart: helm uninstall {release_name}");
    let status = std::process::Command::new("helm")
        .args(["uninstall", &release_name])
        .status()
        .map_err(|e| anyhow::anyhow!("invoke helm: {e}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "helm uninstall exited {:?}", status.code()));
    }

    if let Err(e) = aislang_runtime::mark_state_torn_down(&state_path) {
        eprintln!("warning: chart uninstalled but failed to update state \
                   file `{}`: {e}", state_path.display());
    }

    eprintln!();
    eprintln!("chart uninstalled — release `{release_name}` removed.");
    eprintln!("manifests/ + chart/ files preserved for reference; remove them \
               manually if desired:");
    eprintln!("  rm -rf {} {}",
        output_dir.join(aislang_k8s::MANIFESTS_DIRNAME).display(),
        chart_dir.display());
    Ok(())
}

/// Check whether `docker` is callable. Used by apply (graceful
/// degrade — write artifacts then tell the user how to bring them
/// up manually) and destroy (hard fail — there's nothing useful to
/// do without docker).
fn docker_on_path() -> bool {
    std::process::Command::new("docker")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Phase 2 Week 14 Thu — `helm` CLI presence check, mirror of
/// `docker_on_path` for the K8s deploy target.  Used by the
/// `target = kubernetes` paths of `run_apply` / `run_destroy`:
/// apply graceful-degrades (writes manifests/ + chart/ and prints
/// the manual `helm upgrade --install …` command), destroy hard-
/// fails (without helm there's no way to release-uninstall, and
/// `kubectl delete -f manifests/` doesn't track Helm release state).
fn helm_on_path() -> bool {
    std::process::Command::new("helm")
        .arg("version")
        .arg("--short")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Phase 2 Week 14 Thu follow-up — soft preflight check for the
/// user-created `aislang-providers` Secret (Decision 43.5).  Emits
/// loud warnings + actionable recipes when:
///   1. the Secret doesn't exist; OR
///   2. the Secret exists but a referenced env var's value is empty
///      (catches the `--from-literal=ANTHROPIC_API_KEY="$EMPTY_VAR"`
///      foot-gun where the user's shell didn't have the env var set
///      when they ran kubectl create secret).
///
/// Never blocks the apply: helm install proceeds, the user fixes
/// the Secret afterwards, kubelet picks up the change on the next
/// pod restart.
///
/// Why a warning and not a hard fail: the rest of the chart still
/// needs to be installed (ConfigMaps, Services, Deployments — the
/// Secret is just one Pod-side dependency).  Stopping the install
/// would leave the cluster in a worse state ("nothing applied") +
/// would force users to skip the check with a `--no-preflight`
/// flag when they're sure the Secret IS there (multi-namespace
/// installs, externally-managed Secret, etc.).
///
/// Quiet when kubectl is absent — the user's already in a degraded
/// state (we'd never have reached helm-install either); no point
/// adding noise.  Same posture as `docker_on_path()` / `helm_on_path()`.
///
/// `referenced_vars` comes from `aislang_compose::litellm::
/// referenced_env_vars(resolved)` — the same set the .env.example
/// template + Compose container env-forwarding consume, so the
/// three checks stay in lockstep.
fn preflight_check_providers_secret(referenced_vars: &std::collections::BTreeSet<String>) {
    // kubectl may not be on PATH (helm-only environments do exist
    // — `helm upgrade --install` works against ~/.kube/config without
    // kubectl).  Skip silently in that case.
    let kubectl_present = std::process::Command::new("kubectl")
        .arg("version").arg("--client").arg("--output=yaml")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !kubectl_present { return; }

    // ── Check #1: does the Secret exist? ──
    let out = std::process::Command::new("kubectl")
        .args(["get", "secret", "aislang-providers",
               "-o", "name", "--ignore-not-found"])
        .output();
    let absent = match out {
        Ok(o) if o.status.success() => o.stdout.is_empty(),
        // Any error from kubectl (RBAC denial, cluster unreachable,
        // namespace permission issue) → skip the check rather than
        // false-positive.  Apply continues; if there's a real
        // cluster issue, helm will fail loudly enough.
        _ => return,
    };
    if absent {
        preflight_warn_missing_secret(referenced_vars);
        return;
    }

    // ── Check #2 + #3: per-var value sanity ──
    //
    // `kubectl get secret aislang-providers -o jsonpath='{.data.<VAR>}'`
    // returns the base64-encoded value (or empty string if the key
    // is absent OR present but empty).  We base64-decode the
    // non-empty values to check for non-ASCII bytes (typically
    // signals a placeholder was pasted including unicode `…`
    // ellipsis or smart-quotes — Anthropic's SDK ASCII-encodes the
    // key for the x-api-key header and explodes with
    // `'ascii' codec can't encode character '\u2026'`).
    //
    // Foot-guns caught:
    //   - empty value:  shell didn't have $VAR set when user ran
    //                   `kubectl create secret … --from-literal=
    //                   <VAR>="$<VAR>"`  → LiteLLM forwards empty
    //                   header → 401.
    //   - non-ASCII:    user pasted a placeholder containing `…`
    //                   ellipsis or smart-quotes (e.g.
    //                   `sk-ant-…YOUR-KEY…`) → Anthropic SDK
    //                   ASCII-encode fails → HTTP 500.
    let mut empty_vars:     Vec<String> = Vec::new();
    let mut non_ascii_vars: Vec<String> = Vec::new();
    for var in referenced_vars {
        let v = std::process::Command::new("kubectl")
            .args(["get", "secret", "aislang-providers",
                   "-o", &format!("jsonpath={{.data.{var}}}")])
            .output();
        let Ok(o) = v else { continue };
        if !o.status.success() { continue; }
        if o.stdout.is_empty() {
            empty_vars.push(var.clone());
            continue;
        }
        // Decode the base64 to inspect the actual bytes.  kubectl
        // returns standard base64 with no whitespace; a hand-rolled
        // decoder keeps us from pulling in the `base64` crate.
        let b64_str = String::from_utf8_lossy(&o.stdout).into_owned();
        let decoded = decode_base64(b64_str.trim());
        if let Some(bytes) = decoded {
            if bytes.iter().any(|&b| b > 0x7f) {
                non_ascii_vars.push(var.clone());
            }
        }
        // Decode failure → can't tell; skip silently (kubectl
        // output may have changed, or the data was binary).
    }
    if !empty_vars.is_empty() {
        preflight_warn_empty_secret_values(&empty_vars);
    }
    if !non_ascii_vars.is_empty() {
        preflight_warn_non_ascii_secret_values(&non_ascii_vars);
    }
}

/// Minimal base64 decoder — kubectl returns standard alphabet (`+/=`)
/// with no whitespace.  Returns `None` on invalid input (treated as
/// "can't sanity-check; skip the non-ASCII probe for this var").
/// Hand-rolled to avoid pulling in the `base64` crate for one call
/// site.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    fn idx(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+'        => Some(62),
            b'/'        => Some(63),
            _           => None,
        }
    }
    let bytes = s.as_bytes();
    if bytes.len() % 4 != 0 { return None; }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let pad = (bytes[i+2] == b'=') as usize + (bytes[i+3] == b'=') as usize;
        let a = idx(bytes[i])?  as u32;
        let b = idx(bytes[i+1])? as u32;
        let c = if bytes[i+2] == b'=' { 0 } else { idx(bytes[i+2])? as u32 };
        let d = if bytes[i+3] == b'=' { 0 } else { idx(bytes[i+3])? as u32 };
        let chunk = (a << 18) | (b << 12) | (c << 6) | d;
        out.push(((chunk >> 16) & 0xff) as u8);
        if pad < 2 { out.push(((chunk >> 8) & 0xff) as u8); }
        if pad < 1 { out.push((chunk        & 0xff) as u8); }
        i += 4;
    }
    Some(out)
}

/// Print the "Secret doesn't exist" warning + the create-secret
/// recipe customised to the project's referenced env vars.
fn preflight_warn_missing_secret(referenced_vars: &std::collections::BTreeSet<String>) {
    eprintln!();
    eprintln!("!  preflight WARNING: required Secret `aislang-providers` not found");
    eprintln!("   (Decision 43.5 — provider creds Secret is user-created)");
    eprintln!();
    eprintln!("   The chart's litellm + agent-runtime pods envFrom this Secret;");
    eprintln!("   without it the pods will install but stay Pending with a");
    eprintln!("   `CreateContainerConfigError` event.  Create the Secret then");
    eprintln!("   the pods will progress automatically on the next kubelet sync:");
    eprintln!();
    eprintln!("     kubectl create secret generic aislang-providers \\");
    for (i, var) in referenced_vars.iter().enumerate() {
        let last = i == referenced_vars.len() - 1;
        let backslash = if last { "" } else { " \\" };
        eprintln!("       --from-literal={var}=\"${var}\"{backslash}");
    }
    eprintln!();
    eprintln!("   !  Make sure those env vars are actually exported in your shell");
    eprintln!("      first — empty `$ANTHROPIC_API_KEY` → empty key in the Secret");
    eprintln!("      → LiteLLM 401s on every request.");
    eprintln!();
    eprintln!("   Helm install will proceed; create the Secret afterwards.");
    eprintln!();
}

/// Print the "Secret exists but some values are empty" warning +
/// the patch-secret recipe.
fn preflight_warn_empty_secret_values(empty_vars: &[String]) {
    eprintln!();
    eprintln!("!  preflight WARNING: Secret `aislang-providers` has EMPTY values");
    eprintln!("   for: {}", empty_vars.join(", "));
    eprintln!();
    eprintln!("   These keys are referenced by the resolved model bindings;");
    eprintln!("   LiteLLM will forward empty headers/credentials and providers");
    eprintln!("   will reject the calls (typically HTTP 401).");
    eprintln!();
    eprintln!("   Patch the Secret with the real values:");
    eprintln!();
    eprintln!("     kubectl create secret generic aislang-providers \\");
    for (i, var) in empty_vars.iter().enumerate() {
        let last = i == empty_vars.len() - 1;
        let backslash = if last { "" } else { " \\" };
        // ASCII-only placeholder (no unicode `…` — that triggers
        // the very bug we're trying to prevent if the user pastes
        // it literally).  Uppercase + obvious "PASTE" verb makes
        // it impossible to mistake for a real value.
        eprintln!("       --from-literal={var}='PASTE_{var}_VALUE_HERE'{backslash}");
    }
    eprintln!("       --dry-run=client -o yaml | kubectl apply -f -");
    eprintln!();
    eprintln!("   Then restart the pods so envFrom re-reads the Secret:");
    eprintln!();
    eprintln!("     kubectl rollout restart deploy/litellm");
    eprintln!("     kubectl rollout restart deploy/agent-runtime");
    eprintln!();
    eprintln!("   Helm install will proceed; fix the Secret afterwards.");
    eprintln!();
}

/// Print the "Secret exists but some values contain non-ASCII bytes"
/// warning + the patch recipe.  Almost always caused by the user
/// pasting a placeholder containing unicode ellipsis (`...`) or smart
/// quotes — the Anthropic SDK ASCII-encodes the key for the
/// `x-api-key` header and explodes with
/// `'ascii' codec can't encode character '\u2026'`.  Other providers
/// have similar (less helpful) failure modes.
fn preflight_warn_non_ascii_secret_values(non_ascii_vars: &[String]) {
    eprintln!();
    eprintln!("!  preflight WARNING: Secret `aislang-providers` contains NON-ASCII");
    eprintln!("   bytes in: {}", non_ascii_vars.join(", "));
    eprintln!();
    eprintln!("   Provider API keys are ASCII-only — typically `sk-…` prefix +");
    eprintln!("   base64-alphabet body.  Non-ASCII bytes almost always mean a");
    eprintln!("   placeholder was pasted literally, e.g.");
    eprintln!("       sk-ant-{}YOUR-REAL-KEY{}", '\u{2026}', '\u{2026}');
    eprintln!("   …where `{}` (U+2026) crashes the Anthropic SDK with",       '\u{2026}');
    eprintln!("       'ascii' codec can't encode character '\\u2026'");
    eprintln!("   …surfacing as HTTP 500 from LiteLLM with that message in");
    eprintln!("   the body.  Smart quotes (curly `'`, `\"`) hit the same trap.");
    eprintln!();
    eprintln!("   Get a real API key from the provider's console and patch:");
    eprintln!();
    eprintln!("     kubectl create secret generic aislang-providers \\");
    for (i, var) in non_ascii_vars.iter().enumerate() {
        let last = i == non_ascii_vars.len() - 1;
        let backslash = if last { "" } else { " \\" };
        eprintln!("       --from-literal={var}='PASTE_REAL_{var}_HERE'{backslash}");
    }
    eprintln!("       --dry-run=client -o yaml | kubectl apply -f -");
    eprintln!();
    eprintln!("     kubectl rollout restart deploy/litellm");
    eprintln!("     kubectl rollout restart deploy/agent-runtime");
    eprintln!();
    eprintln!("   Helm install will proceed; fix the Secret afterwards.");
    eprintln!();
}

/// Phase 2 Week 14 Thu — peek the deploy target without doing the
/// full validate+resolve+lockfile dance.  Used at the top of
/// `run_apply` / `run_destroy` to dispatch between the v0.1 Compose
/// path (unchanged) and the new k8s path.
///
/// **Why not factor `prepare_stack` instead**: the Compose pipeline
/// has deeper assumptions baked in (Compose emit unconditionally,
/// state file shape pins stack.yml path).  A surface-level dispatch
/// at the very top keeps the two paths cleanly separable + lets
/// the v0.1 Compose flow stay byte-stable.  Double-validates the
/// source as a result; the second validate is cheap (parser is fast)
/// and `aislang validate` happens often anyway.
fn peek_deploy_target(file: &Path) -> Result<aislang_core::types::DeployTarget> {
    // v0.3 Week 16 follow-up: dir input goes through load_project_
    // input which scans the merged ResolvedProject's deploys; file
    // input keeps the v0.2 lightweight parser-only path so the
    // existing "source ... not readable" diagnostic shape stays
    // stable for error-message tests.
    if file.is_dir() {
        let loaded = load_project_input(file)?;
        return Ok(loaded.resolved.project.deploys.iter()
            .find(|d| d.target == aislang_core::types::DeployTarget::Kubernetes)
            .map(|_| aislang_core::types::DeployTarget::Kubernetes)
            .unwrap_or(aislang_core::types::DeployTarget::Compose));
    }
    let source = std::fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("source file `{}` not readable: {e}",
            file.display()))?;
    let (project, _) = match aislang_core::validate(&source) {
        Ok(v) => v,
        Err(err) => {
            render_validate_error(&source, &err);
            return Err(anyhow::anyhow!("(see error above)"));
        }
    };
    Ok(project.deploys.iter()
        .find(|d| d.target == aislang_core::types::DeployTarget::Kubernetes)
        .map(|_| aislang_core::types::DeployTarget::Kubernetes)
        .unwrap_or(aislang_core::types::DeployTarget::Compose))
}

/// Diagnostic helper for the apply success line. Re-parses the
/// source just to find the first agent label so we can suggest a
/// concrete chat command. Failure is silent — the suggestion is
/// nice-to-have, not load-bearing.
fn first_agent_label_in_file(file: &Path) -> Option<String> {
    let source = std::fs::read_to_string(file).ok()?;
    let (project, _) = aislang_core::validate(&source).ok()?;
    project.agents.first().map(|a| a.label.clone())
}

// ============================================================================
// `aislang cost` — week 10 Tue extra (was stubbed since week 7)
// ============================================================================
//
// Read-only command: surfaces today's UTC spend (total + per-model
// breakdown) for the project, plus the static `budget {…}` caps from
// the .ais source. Pairs the persistent `~/.aislang/spend.json`
// tracker (week 7 Thu) with the new `by_model` attribution
// (week 10 Tue, Decision 32). No network, no docker — fully offline.
//
// Output is plain text on stdout, dollar-formatted via the same
// `cost::format_money` helper the rest of the CLI uses. Designed for
// `aislang cost foo.ais | grep total` ergonomics; v0.2 may add
// `--json` for CI pipelines.

fn run_cost(file: PathBuf) -> Result<()> {
    // Validate so the user sees the same diagnostics they'd see on
    // `aislang validate` if the source is broken. Cheap; the cost
    // command is rare so we don't optimize away the parse.
    let source = std::fs::read_to_string(&file)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    let (project, _warnings) = match aislang_core::validate(&source) {
        Ok(v) => v,
        Err(err) => {
            render_validate_error(&source, &err);
            return Err(anyhow::anyhow!("(see error above)"));
        }
    };

    // Open the tracker read-only — `open_default` reads
    // ~/.aislang/spend.json if present, else seeds an empty one in
    // memory (doesn't write anything until add() is called, which
    // we never do here).
    let tracker = aislang_runtime::SpendTracker::open_default(&project.name)
        .map_err(|e| anyhow::anyhow!("open spend tracker: {e}"))?;
    let today_total    = tracker.today_spend();
    let today_by_model = tracker.today_by_model();

    use aislang_runtime::format_money;
    println!("project:    {}", project.name);
    println!("spend file: {}", tracker.path().display());
    println!();
    println!("today's spend ({}):", aislang_runtime::spend::utc_today());
    println!("  total:   {}", format_money(&today_total));
    if today_by_model.is_empty() {
        println!("  by model: (no per-model attribution yet — \
                   spend.json may predate week-10 Tue, or no chat/eval \
                   run has recorded a cost today)");
    } else {
        println!("  by model:");
        // Find the longest key for column alignment — keeps the
        // output readable when model ids vary in length.
        let pad = today_by_model.keys().map(|k| k.len()).max().unwrap_or(0);
        for (model, amount) in &today_by_model {
            println!("    {model:<pad$}  {}", format_money(amount));
        }
    }

    println!();
    println!("budget caps (from {}):", file.display());
    match project.budget.as_ref() {
        Some(b) => {
            println!("  per-run cap:  {}", format_money(&b.hard_cap_per_run));
            println!("  per-day cap:  {}", format_money(&b.hard_cap_per_day));
            // Day-cap utilization, when both caps are Capped and
            // non-zero. Round to whole percent — over-precision
            // here implies a confidence in micro-USD accuracy that
            // upstream providers don't actually give us.
            if let (Some(cap_mu), Some(spent_mu)) =
                (b.hard_cap_per_day.micro_units(), today_total.micro_units())
            {
                if cap_mu > 0 {
                    let pct = (spent_mu as f64 / cap_mu as f64) * 100.0;
                    println!("  used today:   {pct:.1}%");
                }
            }
        }
        None => {
            println!("  (no `budget {{ … }}` block in source — caps not enforced)");
        }
    }

    Ok(())
}

// ============================================================================
// `aislang status` — week 10 Thu (Decision 28 revised)
// ============================================================================
//
// Three sections, gated by flags:
//   default (no flag): stack status from state.json + `docker compose ps`
//   --spend          : append today's UTC total from spend.json
//   --model-spend [M]: append today's by-model breakdown (filter to M
//                       when a value is given; show all otherwise)
//
// Flags compose: `--spend --model-spend` renders the stack section,
// then the total, then the per-model breakdown.
//
// Exit codes (Decision 28):
//   0  stack OK (state present + applied_at Some + compose ps clean
//      OR no stack section requested but spend section rendered)
//   1  drift — state file absent, applied_at == None, or compose ps
//      shows services down
//   2  docker unavailable when a stack check was attempted

fn run_status(file: PathBuf, spend: bool, model_spend: Option<String>, main_spend: bool) -> Result<ExitCode> {
    // Validate so the user sees the same diagnostics as `aislang validate`
    // when the source is broken. Status is rare; the parse cost is
    // immaterial.
    let source = std::fs::read_to_string(&file)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    let (project, _warnings) = match aislang_core::validate(&source) {
        Ok(v) => v,
        Err(err) => {
            render_validate_error(&source, &err);
            return Err(anyhow::anyhow!("(see error above)"));
        }
    };

    let source_dir = file.parent().unwrap_or_else(|| Path::new("."));
    // canonicalize() fails on a missing dir; degrade to the raw path
    // so the diagnostic still mentions a useful location.
    let output_dir = std::fs::canonicalize(source_dir).unwrap_or_else(|_| source_dir.to_path_buf());
    let state_path = aislang_runtime::state_path_in(&output_dir);
    let stack_path = output_dir.join(aislang_compose::STACK_YAML_FILENAME);

    // Drift accumulator — any section that detects a problem flips
    // this. We render ALL requested sections regardless, then exit
    // with the appropriate non-zero code at the end so users see
    // the full state in one shot.
    let mut drift = false;
    let mut docker_unavailable_for_stack = false;

    // ── Section 1: stack status ────────────────────────────────
    println!("project:  {}", project.name);
    println!("source:   {}", file.display());
    println!();
    println!("stack:");
    match aislang_runtime::read_state(&state_path) {
        Ok(Some(state)) => {
            match state.applied_at {
                Some(ref ts) => {
                    println!("  applied_at:      {ts}");
                    println!("  catalog_version: {}", state.catalog_version);
                    println!("  source_hash:     {}", short_hash(&state.source_hash));
                    println!("  stack.yml:       {}", state.stack_yaml_path.display());

                    if stack_path.exists() && docker_on_path() {
                        match docker_compose_ps_json(&stack_path) {
                            Ok(rows) => render_service_rows(&rows, &mut drift),
                            Err(e) => {
                                println!("  services: (failed to query docker: {e})");
                                drift = true;
                            }
                        }
                    } else if !docker_on_path() {
                        println!("  services: (docker not on PATH — cannot query live status)");
                        docker_unavailable_for_stack = true;
                    } else {
                        println!("  services: (stack.yml missing at `{}`)", stack_path.display());
                        drift = true;
                    }
                }
                None => {
                    println!("  status:  torn down (applied_at: null)");
                    println!("  catalog_version: {}", state.catalog_version);
                    println!("  hint:    `aislang apply {}` to redeploy", file.display());
                    drift = true;
                }
            }
        }
        Ok(None) => {
            println!("  status:  not applied (no state file at `{}`)", state_path.display());
            println!("  hint:    `aislang apply {}` to deploy the stack", file.display());
            drift = true;
        }
        Err(e) => {
            println!("  status:  ERROR reading state file: {e}");
            drift = true;
        }
    }

    // ── Section 2: --spend total ───────────────────────────────
    if spend {
        let tracker = aislang_runtime::SpendTracker::open_default(&project.name)
            .map_err(|e| anyhow::anyhow!("open spend tracker: {e}"))?;
        println!();
        println!("spend ({} UTC):", aislang_runtime::spend::utc_today());
        let total = tracker.today_spend();
        println!("  total:   {}", aislang_runtime::format_money(&total));
        // Show the % of per-day cap when budget present + cap is Capped.
        if let Some(b) = project.budget.as_ref() {
            if let (Some(cap_mu), Some(spent_mu)) =
                (b.hard_cap_per_day.micro_units(), total.micro_units())
            {
                if cap_mu > 0 {
                    let pct = (spent_mu as f64 / cap_mu as f64) * 100.0;
                    println!("  cap:     {} ({pct:.1}% used)",
                        aislang_runtime::format_money(&b.hard_cap_per_day));
                }
            }
        }
    }

    // ── Section 3: --model-spend [MODEL] breakdown ─────────────
    if let Some(filter) = model_spend {
        let tracker = aislang_runtime::SpendTracker::open_default(&project.name)
            .map_err(|e| anyhow::anyhow!("open spend tracker: {e}"))?;
        let by_model = tracker.today_by_model();
        println!();
        if filter.is_empty() {
            println!("spend by model ({} UTC):", aislang_runtime::spend::utc_today());
        } else {
            println!("spend for model `{}` ({} UTC):",
                filter, aislang_runtime::spend::utc_today());
        }
        if by_model.is_empty() {
            println!("  (no spend recorded today)");
            print_external_client_spend_hint(&project);
        } else if filter.is_empty() {
            // All models. Sort by amount descending for readability.
            let mut rows: Vec<_> = by_model.iter().collect();
            rows.sort_by(|a, b| b.1.micro_units().cmp(&a.1.micro_units()));
            let pad = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
            for (model, amount) in rows {
                println!("  {model:<pad$}  {}",
                    aislang_runtime::format_money(amount));
            }
        } else {
            match by_model.get(&filter) {
                Some(amount) => {
                    println!("  {}", aislang_runtime::format_money(amount));
                }
                None => {
                    println!("  (no spend recorded for `{filter}` today)");
                    print_external_client_spend_hint(&project);
                    // NOT drift — absent model just means the agent
                    // didn't use that one today. v0.2 might warn when
                    // the requested model isn't in the project's
                    // resolved model set at all.
                }
            }
        }
    }

    // ── Section 4: --main-spend breakdown (week 13 Thu) ────────
    //
    // Reads `~/.aislang/spend.json`'s `by_main` map for today and
    // renders one line per declared `main` block.  Source-conditional
    // formatting: `model.source = subscription` mains render
    // `subscription (no API charge)`; `source = api` mains render
    // the dollar amount.
    //
    // When the file's `by_main` map is empty AND the project declares
    // at least one main block, surface a hint pointing at the v0.3
    // OpenAI-compat adapter carry-over — same posture as Tue's
    // --model-spend hint for Open WebUI users.
    if main_spend {
        let tracker = aislang_runtime::SpendTracker::open_default(&project.name)
            .map_err(|e| anyhow::anyhow!("open spend tracker: {e}"))?;
        let by_main = tracker.today_by_main();
        println!();
        println!("spend by main ({} UTC):", aislang_runtime::spend::utc_today());
        if project.mains.is_empty() {
            println!("  (no `main` blocks declared in this project)");
        } else {
            // Always walk declared mains in source order — even when
            // by_main is empty, the per-main line communicates the
            // source kind (subscription vs api) so the user can see
            // at a glance which entries WILL incur API charges once
            // the v0.3 OpenAI-compat adapter starts recording them.
            let pad = project.mains.iter()
                .map(|m| m.label.len()).max().unwrap_or(0);
            for main in &project.mains {
                let amount = by_main.get(&main.label);
                let source = resolved_main_source(&project, main);
                match (amount, source) {
                    (Some(a), MainSourceKind::Api) =>
                        println!("  {:<pad$}  {}",
                            main.label, aislang_runtime::format_money(a)),
                    (Some(a), MainSourceKind::Subscription) =>
                        println!("  {:<pad$}  subscription (no API charge; \
                                 tracked tokens-only: {} equivalent)",
                            main.label, aislang_runtime::format_money(a)),
                    (None, MainSourceKind::Api) =>
                        println!("  {:<pad$}  (no spend recorded today)",
                            main.label),
                    (None, MainSourceKind::Subscription) =>
                        println!("  {:<pad$}  subscription (no API charge)",
                            main.label),
                }
            }
            // When NONE of the declared mains have recorded spend,
            // surface the v0.3-direction hint at the bottom (same
            // posture --model-spend uses for the empty-spend case).
            // Skipped when at least one main HAS recorded spend —
            // the per-main lines speak for themselves there.
            if by_main.is_empty() {
                print_main_spend_empty_hint(&project);
            }
        }
    }

    // ── Exit code resolution ───────────────────────────────────
    // Decision 28: 0/1/2. Docker-unavailable takes precedence over
    // generic drift — the user needs to fix docker before drift
    // diagnostics are meaningful.
    Ok(if docker_unavailable_for_stack {
        ExitCode::from(2)
    } else if drift {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Discriminator for `main` block source kinds, used by the
/// `--main-spend` formatter (week 13 Thu).  The .ais grammar
/// declares `model.source = subscription | api` on the model the
/// main block points at; we resolve it once per main during
/// status rendering so the dispatch table above stays terse.
enum MainSourceKind { Api, Subscription }

/// Resolve a main block's source kind by walking back to its
/// model reference + reading the model's `source` field.  Used
/// only by the status renderer (CLI cosmetic), so we tolerate
/// the unlikely "model ref doesn't resolve" case by defaulting
/// to Api (the v0.1-equivalent behaviour) rather than erroring.
fn resolved_main_source(
    project: &aislang_core::ast::Project,
    main:    &aislang_core::ast::Main,
) -> MainSourceKind {
    use aislang_core::types::ModelSource;
    project.models.iter()
        .find(|m| m.label == main.model.label)
        .map(|m| match m.source {
            ModelSource::Subscription => MainSourceKind::Subscription,
            ModelSource::Api          => MainSourceKind::Api,
        })
        .unwrap_or(MainSourceKind::Api)
}

/// Print a hint when `--main-spend` is empty but the project has
/// declared `main` blocks.  Same UX shape as the `--model-spend`
/// Open-WebUI hint (Tue follow-up) — explains why a user who
/// expected to see their principal-LLM's token spend isn't seeing
/// it, and points at the v0.3 OpenAI-compat adapter carry-over
/// that closes the gap.
///
/// Quiet when the project has no main blocks (handled by the
/// caller via `if project.mains.is_empty()` short-circuit).
fn print_main_spend_empty_hint(project: &aislang_core::ast::Project) {
    use aislang_core::types::ModelSource;
    let has_subscription = project.mains.iter().any(|m| {
        project.models.iter()
            .find(|mm| mm.label == m.model.label)
            .map(|mm| matches!(mm.source, ModelSource::Subscription))
            .unwrap_or(false)
    });
    println!();
    println!("  note: v0.2's SpendTracker only records spend that flowed through");
    println!("        `aislang chat` / `aislang mcp-serve`'s chat_turn or eval tools.");
    if has_subscription {
        println!("        Subscription-source `main` entries (Claude Pro, Cursor Team) by");
        println!("        design have no API cost — the empty-state above is correct for them.");
        println!("        Token-count observability for subscription mains requires v0.3's");
    } else {
        println!("        Principal-LLM inference happens inside the host chat app (Claude");
        println!("        Desktop, Cursor, etc.); aislang has no visibility into those tokens.");
        println!("        Closing this gap requires v0.3's");
    }
    println!("        OpenAI-compatible adapter for `aislang mcp-serve` (see PLAN");
    println!("        v0.3 carry-overs for the design sketch).");
}

/// Print a hint about the v0.2 spend-tracking gap when the project
/// has Open WebUI enabled (`expose { web_ui = true }` on any deploy
/// block) but `aislang status --model-spend` shows an empty bucket.
///
/// **Why this hint exists.**  v0.2 Tue (Track A2) adds Open WebUI as
/// a second client surface for the deployed agents.  Open WebUI
/// talks to LiteLLM over OpenAI-compatible HTTP — totally bypassing
/// the in-process [`aislang_runtime::SpendTracker`], which only
/// instruments `Session::turn` (called by `aislang chat` and the MCP
/// `aislang_chat_turn` / `aislang_eval` tools).  So a user who
/// chats from Open WebUI sees real provider-side billing tick up
/// (Anthropic / OpenAI console) while `aislang status --model-spend`
/// shows zero — a confusing mismatch the user surfaced first thing
/// after Tue's live verification.
///
/// **What this function does NOT do.**  It does not actually count
/// the missing spend — that's the v0.3 carry-over work (LiteLLM
/// callbacks or `/spend/logs` query, see PLAN v0.3 carry-overs).
/// It only nudges the reader to look in the right place.
///
/// Quiet on projects that never declared `web_ui = true` — the hint
/// would just be noise there.
fn print_external_client_spend_hint(project: &aislang_core::ast::Project) {
    let has_web_ui = project.deploys.iter()
        .any(|d| d.expose.as_ref().is_some_and(|e| e.web_ui));
    if !has_web_ui {
        return;
    }
    // Three short lines.  Stay terse — verbose hint text in a
    // single-shot status command is its own UX bug.  Pointers to
    // LiteLLM logs + v0.3 carry-overs let interested users dig
    // further on their own time.
    println!();
    println!("  note: only spend from `aislang chat` / `aislang mcp-serve` is tracked here.");
    println!("        Open WebUI talks to LiteLLM directly, bypassing the SpendTracker.");
    println!("        see `docker logs $(docker ps -qf label=aislang.component=litellm)`");
    println!("        for per-request model resolutions; unified counting is a v0.3");
    println!("        carry-over (LiteLLM callback or `/spend/logs` query — see PLAN).");
}

/// Display the first 12 hex chars of the source hash for the status
/// summary. Full 64-char hash is in the state file for audit; the
/// truncated form keeps the human-readable summary scannable.
fn short_hash(full: &str) -> String {
    if full.len() >= 12 {
        format!("{}…", &full[..12])
    } else {
        full.to_owned()
    }
}

/// Minimal compose-ps JSON line. Docker emits `--format json` as
/// NDJSON in 24+; older versions emit a JSON array. We tolerate both.
#[derive(serde::Deserialize)]
struct ComposeService {
    #[serde(rename = "Name", alias = "name", default)]
    name:    String,
    #[serde(rename = "Service", alias = "service", default)]
    service: String,
    #[serde(rename = "State",   alias = "state",   default)]
    state:   String,
    #[serde(rename = "Status",  alias = "status",  default)]
    status:  String,
}

/// Query `docker compose -f <stack_path> ps --format json` and parse
/// the result. Tolerates both NDJSON (Docker Compose v2.24+) and
/// JSON-array (older) output shapes.
fn docker_compose_ps_json(stack_path: &Path) -> anyhow::Result<Vec<ComposeService>> {
    let out = std::process::Command::new("docker")
        .args(["compose", "-f"])
        .arg(stack_path)
        .args(["ps", "--format", "json"])
        .output()
        .map_err(|e| anyhow::anyhow!("invoke docker compose ps: {e}"))?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "docker compose ps exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()));
    }
    let body = String::from_utf8_lossy(&out.stdout);
    let body = body.trim();
    if body.is_empty() { return Ok(Vec::new()); }
    // Try JSON-array first (older), then NDJSON (newer).
    if body.starts_with('[') {
        let v: Vec<ComposeService> = serde_json::from_str(body)
            .map_err(|e| anyhow::anyhow!("parse compose ps json array: {e}"))?;
        Ok(v)
    } else {
        let mut rows = Vec::new();
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() { continue; }
            let row: ComposeService = serde_json::from_str(line)
                .map_err(|e| anyhow::anyhow!("parse compose ps json line `{line}`: {e}"))?;
            rows.push(row);
        }
        Ok(rows)
    }
}

/// Render a `services:` sub-block for the stack section, flipping
/// `drift` when any service isn't in a healthy state.
fn render_service_rows(rows: &[ComposeService], drift: &mut bool) {
    if rows.is_empty() {
        println!("  services: (none running — compose ps returned empty)");
        *drift = true;
        return;
    }
    println!("  services:");
    let pad = rows.iter()
        .map(|r| if r.service.is_empty() { r.name.len() } else { r.service.len() })
        .max().unwrap_or(0);
    for r in rows {
        let label = if r.service.is_empty() { &r.name } else { &r.service };
        // `state` is the canonical lowercase form; `status` is the
        // human-readable string ("Up 5 minutes"). Prefer state for
        // the drift check, status for the display.
        let is_up = matches!(r.state.as_str(), "running" | "Up" | "up");
        let mark  = if is_up { "✓" } else { "✗" };
        println!("    {mark} {label:<pad$}  {}", r.status);
        if !is_up { *drift = true; }
    }
}

// ============================================================================
// `aislang chat` — Monday of week 6 (stub) → Wednesday (live LiteLLM loop)
// ============================================================================

/// Open an interactive chat session against a declared agent — week
/// 12 Fri carry-over from Thu (the proxy-via-MCP refactor that the
/// Thu DoD deferred to v0.3).
///
/// v0.1 ran the chat loop in-process: parse source → find agent →
/// resolve refs → spawn MCP children directly → open `Session`
/// against LiteLLM → stdin loop. All the heavy lifting lived in
/// the CLI.
///
/// v0.2 (this function) refactors `aislang chat` to be a **thin MCP
/// client of `aislang mcp-serve`**, materialising planning-doc Track
/// A architectural decision #3 ("proxy-via-MCP — the MCP server IS
/// the agent runtime"). Concretely:
///   1. CLI does validate + agent-existence check upfront so the
///      user sees the same diagnostics they'd see in `aislang
///      validate` for bad source / wrong agent label.
///   2. CLI then spawns `aislang mcp-serve --stdio` as a child via
///      `current_exe()` and connects via rmcp's client side.
///   3. Stdin loop reads one line at a time and dispatches each as
///      `aislang_chat_turn(source, source_dir, agent_label, user_input)`
///      — same Session pool the Claude Desktop / Cursor chat uses.
///   4. `/exit` and EOF close the MCP connection cleanly; mcp-serve
///      drops the cached Session + MCP children on its end.
///
/// What this gives us:
///   - One Session-pool implementation (mcp-serve) feeding both the
///     `aislang chat` CLI AND any external MCP client. No surface
///     duplication between the in-CLI loop and the MCP-tool path.
///   - Same OTel span tree regardless of whether chat is driven by
///     the CLI or by Claude Desktop — mcp-serve owns the root span.
///   - Prompt-hash verification, budget enforcement, spend tracking,
///     tool-use loop — all happen exactly once, in mcp-serve.
///
/// What we lose (acceptable):
///   - The chat CLI no longer prints "connected to LiteLLM at …" or
///     "tools: N available" up front. Both were nice-to-have status
///     lines whose information is now discovered lazily on the first
///     `aislang_chat_turn` call. We trade them for the architectural
///     simplification.
///   - The CLI's OTel tracing init moves to mcp-serve. The chat CLI
///     itself emits no spans now — just dispatches MCP tool calls.
fn run_chat(agent_label: String, file: PathBuf) -> Result<()> {
    // ── 1. read + validate source ──
    let source = std::fs::read_to_string(&file)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    let (project, _warnings) = match aislang_core::validate(&source) {
        Ok(v) => v,
        Err(err) => {
            render_validate_error(&source, &err);
            return Err(anyhow::anyhow!("(see error above)"));
        }
    };

    // ── 2. find agent (with did-you-mean + available-list UX) ──
    // We do this in the CLI rather than letting mcp-serve report it
    // because v0.1's chat tests pin the error message shape — and
    // having the CLI fail fast on a bad agent label is better UX
    // than starting mcp-serve and surfacing the same error from a
    // failed `aislang_chat_turn` call.
    let agent = match project.agents.iter().find(|a| a.label == agent_label) {
        Some(a) => a,
        None => {
            let mut available: Vec<&str> = project.agents.iter()
                .map(|a| a.label.as_str())
                .collect();
            available.sort();
            let hint = nearest_agent_label(&agent_label, &available);
            let hint_line = match hint {
                Some(h) => format!(" — did you mean `{h}`?"),
                None    => String::new(),
            };
            let avail_line = if available.is_empty() {
                "  (no agents declared in this project)".to_string()
            } else {
                format!("  available: {}", available.join(", "))
            };
            return Err(anyhow::anyhow!(
                "no agent named `{agent_label}` in {}{hint_line}\n{avail_line}",
                file.display()
            ));
        }
    };

    // Stash the model alias for the banner (the only thing we know
    // upfront about the resolved chat target).  The actual model
    // resolution, prompt loading, MCP children spawning, budget +
    // tracker setup all happen inside mcp-serve on the first
    // `aislang_chat_turn` call.
    let model_label = agent.model.label.clone();

    // Canonical source_dir for the MCP server's prompt-resolution.
    let source_dir = file.parent().unwrap_or_else(|| Path::new("."));
    let source_dir = std::fs::canonicalize(source_dir)
        .map_err(|e| anyhow::anyhow!(
            "source dir `{}` not readable: {e}", source_dir.display()))?;

    // Canonical absolute file path so mcp-serve doesn't have to guess
    // when it later re-reads the .ais via source_path (week 13 Thu
    // follow-up — keeps each chat_turn MCP request envelope tiny so
    // Claude Desktop's confirmation preview stays scannable).
    let source_path = std::fs::canonicalize(&file)
        .map_err(|e| anyhow::anyhow!(
            "source file `{}` not readable: {e}", file.display()))?;

    // ── 3. spin tokio runtime + drive the MCP-proxied chat loop ──
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;
    let result = rt.block_on(chat_via_mcp_serve(
        source_path, source_dir, agent_label, model_label,
    ));
    drop(rt);
    result
}

/// Drive an interactive chat session by spawning `aislang mcp-serve
/// --stdio` as a child process and proxying each stdin line through
/// the `aislang_chat_turn` MCP tool.  Returns Ok on clean close
/// (EOF or `/exit`); bubbles errors that occur before the chat loop
/// starts (spawn failure, MCP handshake failure).
///
/// Per-turn errors (parse error in source / unknown agent / LiteLLM
/// HTTP failure / etc.) print to stderr but DON'T abort the session,
/// matching the v0.1 in-process chat behaviour: the user can
/// continue typing.
/// rmcp `ClientHandler` impl for `aislang chat` — week 13 Wed.
///
/// The only callback we care about is `on_progress`: it fires when
/// `mcp-serve`'s `aislang_chat_turn` tool sends a `notifications/
/// progress` message to keep the user informed during a multi-
/// iteration tool-use turn.  We render each progress message as a
/// dim-stderr line so the assistant's final reply (which lands on
/// stdout via the synchronous `call_tool` future) stays the visual
/// focus.
///
/// PII-safe by construction: `mcp_server::chat_turn` builds the
/// `message` field from `SessionProgress::message()`, which only
/// contains labels + sizes — never raw arg content or tool-output
/// bodies (Decision 21 carry-over).
#[derive(Default, Clone)]
struct AislangChatClient;

impl rmcp::ClientHandler for AislangChatClient {
    async fn on_progress(
        &self,
        params:   rmcp::model::ProgressNotificationParam,
        _context: rmcp::service::NotificationContext<rmcp::RoleClient>,
    ) {
        // Indent + dim-italic prefix so the progress lines visually
        // separate from the assistant's eventual reply line (which
        // is the user's actual data).  ANSI codes are safe on stderr
        // — even non-TTY pipes (tee'd logs) tolerate them.
        let msg = params.message.as_deref().unwrap_or("(no message)");
        eprintln!("\x1b[2;3m  · {msg}\x1b[0m");
    }
}

async fn chat_via_mcp_serve(
    source_path: PathBuf,
    source_dir:  PathBuf,
    agent_label: String,
    model_label: String,
) -> Result<()> {
    use rmcp::{ServiceExt,
        model::{CallToolRequestParams, NumberOrString, ProgressToken, RawContent,
                RequestParamsMeta},
        transport::TokioChildProcess};
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    // ── Spawn the same `aislang` binary as a `mcp-serve --stdio`
    // child.  `current_exe()` returns the path to this running
    // binary, so the child shares our build + version (no PATH
    // lookup risk; no version drift if multiple `aislang` binaries
    // exist on the system).
    let bin = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("locate own binary: {e}"))?;
    let mut cmd = Command::new(&bin);
    cmd.arg("mcp-serve").arg("--stdio");
    // Inherit our stderr: mcp-serve's "starting stdio MCP server …"
    // banner + any internal warnings still surface to the user.
    cmd.stderr(std::process::Stdio::inherit());

    let transport = TokioChildProcess::new(cmd)
        .map_err(|e| anyhow::anyhow!("spawn aislang mcp-serve: {e}"))?;
    // Use the custom client handler so progress notifications get
    // rendered as dim-stderr lines via `AislangChatClient::on_progress`.
    let client = AislangChatClient::default().serve(transport).await
        .map_err(|e| anyhow::anyhow!("MCP handshake failed: {e}"))?;

    // ── Banner.  Stderr so stdout stays pipe-clean for assistant
    // replies.  v0.1's chat banner mentioned the LiteLLM URL +
    // tool count; both move into mcp-serve and surface lazily on
    // the first `aislang_chat_turn` call.  The CLI knows only the
    // agent label + model alias upfront.
    eprintln!("aislang chat — agent `{agent_label}` (model alias `{model_label}`) \
               via aislang mcp-serve --stdio");
    eprintln!("connected to LiteLLM via mcp-serve's Session pool.");
    eprintln!("type your message and press Enter; `/exit` or Ctrl-D ends the session.");
    eprintln!();

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    // Monotonic per-process progress-token sequence.  rmcp uses the
    // value verbatim to subscribe handlers; making each turn unique
    // avoids cross-turn confusion if a host ever forks the chat
    // session across multiple stdin lines (today: doesn't, but the
    // sequence is cheap insurance).
    let mut turn_counter: u64 = 0;

    let exit_reason = loop {
        eprint!("> ");
        use std::io::Write;
        let _ = std::io::stderr().flush();

        let line = match reader.next_line().await {
            Ok(Some(l)) => l,
            Ok(None)    => break "(end of input)",
            Err(e)      => {
                client.cancel().await.ok();
                return Err(anyhow::anyhow!("stdin read error: {e}"));
            }
        };
        let line = line.trim();
        if line.is_empty() { continue; }
        if line == "/exit" { break "(/exit — session ended)"; }

        // ── Dispatch one turn via the MCP tool.  Same Session
        // pool the Claude Desktop / Cursor chat uses (planning-doc
        // Track A architectural decision #3).
        //
        // Week 13 Wed: include a per-turn `progressToken` in the
        // request meta so mcp-serve's chat_turn knows to emit
        // `notifications/progress` updates during the tool-use loop.
        // Our `AislangChatClient::on_progress` handler renders them
        // as dim-stderr lines as they arrive.
        turn_counter += 1;
        let progress_token = ProgressToken(NumberOrString::Number(turn_counter as i64));
        // Pass `source_path` (file path) instead of `source` (inline
        // file contents) so the request envelope stays tiny.  Same
        // resolution policy as Claude Desktop / Cursor / any other
        // MCP host that drives chat_turn — the path WINS over inline
        // source if both are set; here we set only the path.
        let mut params = CallToolRequestParams::new("aislang_chat_turn")
            .with_arguments(serde_json::json!({
                "source_path": source_path.to_string_lossy(),
                "source_dir":  source_dir.to_string_lossy(),
                "agent_label": agent_label,
                "user_input":  line,
            }).as_object().unwrap().clone());
        // RequestParamsMeta trait gives us set_progress_token —
        // populates `params.meta._meta.progressToken` per the MCP
        // 2025-11-25 spec.
        params.set_progress_token(progress_token);
        match client.peer().call_tool(params).await {
            Ok(result) => {
                let reply = result.content.first()
                    .and_then(|c| match &c.raw {
                        RawContent::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .unwrap_or("(empty response)");
                println!("{reply}");
            }
            Err(e) => {
                // Per-turn error — don't abort the session.  Same
                // posture v0.1's chat loop took: surface the error
                // on stderr and let the user retry or `/exit`.
                eprintln!("aislang chat: {e}");
            }
        }
    };

    // Clean shutdown — closes the stdio pipe; mcp-serve sees EOF
    // and exits.
    eprintln!("{exit_reason}");
    client.cancel().await
        .map_err(|e| anyhow::anyhow!("shutdown mcp-serve child: {e}"))?;
    Ok(())
}

/// Render an `OciImage` into the docker-friendly form. Mirrors
/// `aislang_compose::format_image` (private there) — duplicated here
/// rather than re-exported so the CLI and emitter can diverge later
/// (the emitter writes a YAML scalar; the CLI hands it to
/// `docker run`).
fn format_image(img: &aislang_core::types::OciImage) -> String {
    let base = format!("{}/{}", img.registry, img.repository);
    match (&img.tag, &img.digest) {
        (Some(t), Some(d)) => format!("{base}:{t}@{d}"),
        (Some(t), None)    => format!("{base}:{t}"),
        (None,    Some(d)) => format!("{base}@{d}"),
        (None,    None)    => base,
    }
}

/// Walk one `mcp_server`'s `secrets = [secret.X, secret.Y]` field,
/// look up each ref in `project.secrets`, resolve its `source` URI
/// via `aislang_runtime::resolve_secret`, and assemble the
/// `(env_var_name, value)` pairs that'll be passed to the spawned
/// child's environment.
///
/// Naming convention (v0.1): **the secret's label IS the env-var
/// name inside the container**, used verbatim (case-preserved).
/// Users should follow the standard `UPPER_SNAKE_CASE` convention
/// for secret labels intended for env-var consumption.
///
/// Scoping (Decision 9): this function only ever reads the
/// specific `secrets` list on the GIVEN mcp_server. Other secret
/// blocks in `project.secrets` that don't appear here are simply
/// not read. There's no "deny list" — by construction, only what
/// was declared reaches the child.
///
/// Returns an empty `Vec` when `mcp.secrets` is `None` or empty.
fn resolve_mcp_secrets(
    project:    &aislang_core::ast::Project,
    mcp:        &aislang_core::ast::McpServer,
    source_dir: &std::path::Path,
) -> Result<Vec<(String, String)>, anyhow::Error> {
    let refs = match &mcp.secrets {
        Some(refs) if !refs.is_empty() => refs,
        _ => return Ok(Vec::new()),
    };
    let mut env = Vec::with_capacity(refs.len());
    for s_ref in refs {
        // Lookup the secret block by label. The checker (week 3)
        // already validated that all such refs resolve, but we
        // double-check here so a programmer error in tests can't
        // panic — surfaces as a clean error instead.
        let secret = project.secrets.iter()
            .find(|s| s.label == s_ref.label)
            .ok_or_else(|| anyhow::anyhow!(
                "mcp_server `{}` references secret `{}` which doesn't \
                 exist in this project",
                mcp.label, s_ref.label))?;
        let value = aislang_runtime::resolve_secret(
            &secret.label, &secret.source, source_dir,
        ).map_err(|e| anyhow::anyhow!("{e}"))?;
        env.push((secret.label.clone(), value));
    }
    Ok(env)
}

/// Levenshtein-style "closest match" heuristic. Threshold ≤2 edits;
/// shorter than that and we'd suggest random labels.
///
/// Lives in the CLI rather than `aislang-core` because the chat-time
/// error UX is the only consumer in v0.1; if more callers want it,
/// promote later.
fn nearest_agent_label<'a>(target: &str, candidates: &[&'a str]) -> Option<&'a str> {
    candidates.iter()
        .map(|c| (c, levenshtein(target, c)))
        .filter(|(_, d)| *d <= 2)
        .min_by_key(|(_, d)| *d)
        .map(|(c, _)| *c)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let (m, n) = (av.len(), bv.len());
    if m == 0 { return n; }
    if n == 0 { return m; }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut cur:  Vec<usize> = vec![0; n + 1];
    for i in 1..=m {
        cur[0] = i;
        for j in 1..=n {
            let cost = if av[i - 1] == bv[j - 1] { 0 } else { 1 };
            cur[j] = (cur[j - 1] + 1)         // insertion
                .min(prev[j] + 1)             // deletion
                .min(prev[j - 1] + cost);     // substitution
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[n]
}

/// On validate failure, try the grammar-only `parse_raw_*` path. If it
/// succeeds, the failure was at conversion time and we have a partial
/// tree to show. If the grammar fails too, there's no tree to render —
/// the user already saw the grammar error in the main message.
fn emit_failure_diagnostic(source: &str, verbose: u8) {
    if verbose >= 3 {
        match aislang_core::parser::parse_raw_json(source) {
            Ok(json) => {
                eprintln!();
                eprintln!("diagnostic: grammar parse succeeded; conversion to typed AST failed.");
                eprintln!("partial RawBlock tree (what got parsed before the converter rejected):");
                eprintln!("{json}");
            }
            Err(_) => emit_grammar_level_note(),
        }
    } else if verbose >= 2 {
        match aislang_core::parser::parse_raw_debug(source) {
            Ok(dbg) => {
                eprintln!();
                eprintln!("diagnostic: grammar parse succeeded; conversion to typed AST failed.");
                eprintln!("partial RawBlock tree (Debug):");
                eprintln!("{dbg}");
            }
            Err(_) => emit_grammar_level_note(),
        }
    } else {
        // verbose == 1: brief summary only
        match aislang_core::parser::parse_raw_summary(source) {
            Ok(summary) => {
                eprintln!();
                eprintln!("diagnostic: grammar parse succeeded; conversion to typed AST failed.");
                eprintln!("partial parse summary:");
                eprintln!("{summary}");
                eprintln!();
                eprintln!("(re-run with -vv for the Debug tree, -vvv for the full JSON RawBlock)");
            }
            Err(_) => emit_grammar_level_note(),
        }
    }
}

fn emit_grammar_level_note() {
    eprintln!();
    eprintln!("diagnostic: grammar-level parse error — no partial AST available.");
    eprintln!("the main error above carries the most specific information chumsky produced.");
    eprintln!("(line/column highlighting is not yet implemented — see PLAN week-3+)");
}

fn print_summary(project: &aislang_core::ast::Project) {
    eprintln!();
    eprintln!("summary:");
    eprintln!("  project:     {}", project.name);
    let counts: [(&str, usize); 9] = [
        ("budget",      project.budget.is_some() as usize),
        ("models",      project.models.len()),
        ("prompts",     project.prompts.len()),
        ("mcp_servers", project.mcp_servers.len()),
        ("agents",      project.agents.len()),
        ("evals",       project.evals.len()),
        ("secrets",     project.secrets.len()),
        ("deploys",     project.deploys.len()),
        // v0.2 Fri: new `main` resource type.  Only printed when
        // present (count > 0) — preserves the existing terse
        // verbose-output shape for v0.1-shape files.
        ("mains",       project.mains.len()),
    ];
    for (kind, count) in counts {
        if count > 0 {
            eprintln!("  {kind:<12}{count}");
        }
    }
}

// (`stub` helper removed week 10 Tue — every subcommand is now wired
// to its real implementation. v0.2 commands that need placeholders
// should declare them in `Command` then route to a fresh inline
// helper, not bring this back; the type-checker should refuse to
// build with an unimplemented subcommand.)

// ============================================================================
// `aislang eval` — week 8 runner
// ============================================================================
//
// Walks `project.evals`, filters to the named one (or runs all),
// resolves the agent per Decision 13, opens a `Session` (reusing
// week-7's Budget + Tracker plumbing — caps apply per case), and
// iterates the dataset. JSON `EvalReport` per eval on stdout; human
// summary on stderr.
//
// Exit codes (Decision 16):
//   0 — every eval passed its threshold.
//   1 — at least one eval below threshold (CI-fail trigger).
//   2 — configuration error (file missing, unknown agent, malformed
//       dataset, unimplemented metric). Returned via `anyhow::Error`
//       so the existing `main()` exit-code handling fires
//       ExitCode::FAILURE = 1 — for now we collapse 1 and 2 into
//       "non-zero". Friday's polish may split them by routing
//       config errors via a distinct sentinel.

fn run_eval(eval_label_opt: Option<String>, file: PathBuf) -> Result<()> {
    let source = std::fs::read_to_string(&file)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    let source_dir = file.parent().unwrap_or_else(|| Path::new("."));

    let (project, _warnings) = match aislang_core::validate(&source) {
        Ok(v) => v,
        Err(err) => {
            render_validate_error(&source, &err);
            return Err(anyhow::anyhow!("(see error above)"));
        }
    };

    // Filter to the requested eval(s).
    let evals: Vec<&aislang_core::ast::Eval> = match &eval_label_opt {
        Some(name) => {
            let found = project.evals.iter().find(|e| &e.label == name);
            match found {
                Some(e) => vec![e],
                None => {
                    let available: Vec<&str> = project.evals.iter()
                        .map(|e| e.label.as_str()).collect();
                    return Err(anyhow::anyhow!(
                        "no eval named `{name}` in {}.\n  available: {}",
                        file.display(),
                        if available.is_empty() { "(none declared)".to_string() }
                        else { available.join(", ") },
                    ));
                }
            }
        }
        None => project.evals.iter().collect(),
    };

    if evals.is_empty() {
        return Err(anyhow::anyhow!(
            "no `eval` blocks declared in {}", file.display()));
    }

    // Spin up tokio once for the whole batch — each eval gets its
    // own Session within.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;

    // OTel tracing init (week 9 Mon, fail-soft). Same sync-only
    // boundary rules as `run_chat` — see the comment block there.
    if let Err(e) = aislang_runtime::init_tracer("aislang") {
        eprintln!("tracing disabled: {e}");
    }

    let mut all_passed = true;
    for eval in evals {
        // One RootSpan per eval invocation — N+1 spans (root +
        // per-case) per Mon DoD. Created in sync ctx; the inner
        // async block re-attaches the cloned context so any child
        // turn spans (Tue/Wed) inherit this root.
        let root_span = aislang_runtime::RootSpan::for_eval(&eval.label);
        let root_ctx  = root_span.context();
        // Capture the eval-root URL BEFORE handing the span into the
        // async block — `run_eval_inner` stamps it on EvalReport.trace_url
        // (week 9 Thu, Decision 24). Returns `None` when tracing is
        // disabled so the JSON omits the field rather than emitting
        // a useless …/trace/00000…0 link.
        let eval_trace_url = root_span.trace_url();
        let report = rt.block_on(async {
            let _g = root_ctx.attach();
            run_eval_inner(&project, eval, source_dir, eval_trace_url).await
        })?;
        // Stdout: machine-readable JSON.
        let json = serde_json::to_string_pretty(&report)
            .map_err(|e| anyhow::anyhow!("serialize report: {e}"))?;
        println!("{json}");
        // Stderr: human summary.
        eprintln!();
        eprintln!("eval `{}`: {} ({:.4} {} {})",
            report.eval_label,
            if report.pass { "PASS" } else { "FAIL" },
            report.aggregate_score,
            if report.pass { "≥" } else { "<" },
            report.threshold,
        );
        eprintln!("  metric:      {}", report.metric);
        eprintln!("  aggregator:  {}", report.aggregator);
        eprintln!("  cases:       {}", report.cases.len());
        eprintln!("  dataset:     {}", report.dataset_path);
        if !report.pass { all_passed = false; }
        // Drop the root span in sync ctx between iterations →
        // simple processor exports synchronously, stderr breadcrumb
        // for THIS eval prints here.
        drop(root_span);
    }

    // Drop the tokio runtime in sync ctx, then flush the provider.
    drop(rt);
    aislang_runtime::shutdown_tracer();

    if all_passed { Ok(()) } else {
        Err(anyhow::anyhow!(
            "one or more evals failed their threshold (see reports above)"))
    }
}

/// Async: open a Session for one eval, run the dataset, build the
/// EvalReport. Mirrors `chat_interactive`'s session-construction
/// path so the agent runs exactly as it would for `aislang chat`.
async fn run_eval_inner(
    project:        &aislang_core::ast::Project,
    eval:           &aislang_core::ast::Eval,
    source_dir:     &Path,
    eval_trace_url: Option<String>,
) -> Result<aislang_runtime::EvalReport> {
    use aislang_runtime::SpawnConfig;

    // (OTel tracing init + RootSpan are owned by `run_eval` — they
    // MUST drop in sync ctx. The root context is already attached
    // on this worker thread by the calling block's `_g` guard.
    // `eval_trace_url` was captured from the RootSpan before this
    // function ran; we stamp it on the produced EvalReport at the
    // bottom. Per-case URLs are computed inline as we open each
    // `aislang.case` span below.)

    // ── 1. resolve which agent this eval runs against (Decision 13) ──
    let agent: &aislang_core::ast::Agent = match &eval.agent {
        Some(a_ref) => project.agents.iter().find(|a| a.label == a_ref.label)
            .ok_or_else(|| anyhow::anyhow!(
                "eval `{}` references agent `{}` which doesn't exist",
                eval.label, a_ref.label))?,
        None => match project.agents.len() {
            1 => &project.agents[0],
            0 => return Err(anyhow::anyhow!(
                "eval `{}` has no `agent = …` field and the project declares zero \
                 agents — nothing to evaluate", eval.label)),
            n => return Err(anyhow::anyhow!(
                "eval `{}` has no `agent = …` field but the project declares {n} \
                 agents — add an explicit `agent = agent.<label>` to disambiguate",
                eval.label)),
        },
    };

    let model_label = agent.model.label.clone();

    // ── 2. resolve prompt + load + verify hash ──
    let prompt_block = project.prompts.iter()
        .find(|p| p.label == agent.system_prompt.label)
        .ok_or_else(|| anyhow::anyhow!(
            "agent `{}` references prompt `{}` which doesn't exist",
            agent.label, agent.system_prompt.label))?;
    let prompt_path = source_dir.join(&prompt_block.file);
    let lock_path = source_dir.join("aislang.lock");
    let expected_hash: Option<String> =
        match aislang_resolver::lockfile::read(&lock_path) {
            Ok(Some(lock)) => lock.prompt_hashes.get(&prompt_block.label).cloned(),
            _              => None,
        };
    let system_prompt = aislang_runtime::load_system_prompt(
        &prompt_path, expected_hash.as_deref(),
    ).map_err(|e| anyhow::anyhow!("{e}"))?;

    // ── 3. resolve model bindings + build budget (mirrors run_chat) ──
    use aislang_runtime::BudgetConfig;
    let budget_cfg: Option<BudgetConfig> = match project.budget.as_ref() {
        Some(b) => {
            let resolved = aislang_resolver::resolve_against_bundled_catalog(
                project, source_dir,
            ).map_err(|e| anyhow::anyhow!("resolve error: {e}"))?;
            let model_id = resolved.model_bindings.get(&model_label)
                .ok_or_else(|| anyhow::anyhow!(
                    "model `{model_label}` has no resolved binding"))?;
            let catalog = aislang_catalog::bundled()
                .map_err(|e| anyhow::anyhow!("catalog load error: {e}"))?;
            let entry = catalog.find(model_id)
                .ok_or_else(|| anyhow::anyhow!(
                    "catalog entry `{}.{}` (resolved primary for `{model_label}`) \
                     not found in bundled catalog",
                    model_id.provider, model_id.name))?;
            Some(BudgetConfig {
                per_run_cap:        b.hard_cap_per_run,
                per_day_cap:        b.hard_cap_per_day,
                cost_input_per_1k:  entry.cost_input,
                cost_output_per_1k: entry.cost_output,
                model_id_str:       format!("{}.{}", model_id.provider, model_id.name),
            })
        }
        None => None,
    };
    let tracker = match budget_cfg.as_ref() {
        Some(_) => Some(aislang_runtime::SpendTracker::open_default(&project.name)
            .map_err(|e| anyhow::anyhow!("open spend tracker: {e}"))?),
        None => None,
    };

    // ── 4. collect MCP specs for the resolved agent (same as chat) ──
    let mut mcp_specs: Vec<(String, SpawnConfig)> = Vec::new();
    for tool_ref in &agent.tools {
        if tool_ref.kind != aislang_core::ast::RefKind::McpServer {
            return Err(anyhow::anyhow!(
                "agent `{}.tools` ref `{}` has kind `{:?}` — \
                 v0.1 only supports `mcp_server` tool refs",
                agent.label, tool_ref.label, tool_ref.kind));
        }
        let mcp = project.mcp_servers.iter()
            .find(|m| m.label == tool_ref.label)
            .ok_or_else(|| anyhow::anyhow!(
                "agent `{}` references mcp_server `{}` which doesn't exist",
                agent.label, tool_ref.label))?;
        let mount = mcp.mount.as_ref().map(|p| {
            let resolved = source_dir.join(p);
            std::fs::canonicalize(&resolved).unwrap_or(resolved)
        });
        let env = resolve_mcp_secrets(project, mcp, source_dir)
            .map_err(|e| anyhow::anyhow!(
                "secret resolution failed for mcp_server `{}`: {e}", mcp.label))?;
        mcp_specs.push((mcp.label.clone(), SpawnConfig {
            image: format_image(&mcp.image),
            args:  Vec::new(),
            mount,
            env,
        }));
    }

    // ── 5. spawn MCP children + build registry ──
    let mut mcp_clients = Vec::with_capacity(mcp_specs.len());
    let mut server_tools: Vec<(String, Vec<aislang_runtime::ToolSpec>)> = Vec::new();
    for (label, cfg) in &mcp_specs {
        let child = aislang_runtime::StdioChild::spawn(cfg).await
            .map_err(|e| anyhow::anyhow!(
                "spawn MCP server `{label}` ({}): {e}", cfg.image))?;
        let tools = child.list_tools().await
            .map_err(|e| anyhow::anyhow!(
                "list tools from MCP server `{label}`: {e}"))?;
        server_tools.push((label.clone(), tools));
        mcp_clients.push(child);
    }
    let tool_registry = aislang_runtime::ToolRegistry::from_servers(&server_tools);

    // ── 6. open Session (reuses week-7 budget + tracker plumbing) ──
    let session = {
        let s = aislang_runtime::Session::open_with_tools(
            &model_label, system_prompt, None,
            mcp_clients, tool_registry,
        ).map_err(|e| anyhow::anyhow!("open session: {e}"))?;
        let s = match budget_cfg { Some(b) => s.with_budget(b), None => s };
        match tracker { Some(t) => s.with_tracker(t), None => s }
    };

    // ── 7. load dataset + iterate cases ──
    let dataset_path = source_dir.join(&eval.dataset);
    let dataset = aislang_runtime::load_dataset(&dataset_path)
        .map_err(|e| anyhow::anyhow!("load dataset: {e}"))?;

    let total = dataset.len();
    eprintln!("eval `{}`: running {total} cases against agent `{}`...",
        eval.label, agent.label);

    // Per-case progress feedback — local-LLM eval runs can take
    // minutes, especially when individual turns hit the tool-use
    // loop's iteration cap. Print one line per case BEFORE the
    // turn (so the user sees what's running) and one line AFTER
    // (score + latency). Stderr only, so the JSON stdout stays
    // clean for downstream pipes / jq.
    use std::io::Write as _;
    use opentelemetry::{
        global, KeyValue,
        trace::{Span, SpanKind, TraceContextExt, Tracer},
        Context,
    };
    let mut case_results: Vec<aislang_runtime::CaseResult> =
        Vec::with_capacity(dataset.len());
    let tracer = global::tracer("aislang");
    for (idx, case) in dataset.iter().enumerate() {
        let n = idx + 1;   // 1-indexed for human display
        // Pre-turn line — flush immediately so the user sees the
        // "running" state during the long wait, not just after.
        eprint!("  [{n}/{total}] {} … ", case.id);
        let _ = std::io::stderr().flush();

        // ── Per-case OTel span (week 9 Thu, Decision 24) ──
        // Opened here, BEFORE session.turn, so `aislang.turn` nests
        // under `aislang.case` (Context::current() = case_ctx during
        // the turn). Span_id is what we deep-link to in the
        // EvalReport via `uiFind=<span_id>`.
        let case_span = tracer.span_builder("aislang.case")
            .with_kind(SpanKind::Internal)
            .with_attributes([
                KeyValue::new("aislang.case",       case.id.clone()),
                KeyValue::new("aislang.case.index", idx as i64),
            ])
            .start(&tracer);
        let case_trace_id = case_span.span_context().trace_id();
        let case_span_id  = case_span.span_context().span_id();
        let case_url = if eval_trace_url.is_some() {
            Some(aislang_runtime::jaeger_url_with_span(
                case_trace_id, case_span_id))
        } else {
            None
        };
        let case_ctx = Context::current_with_span(case_span);
        let _case_guard = case_ctx.attach();

        let t0 = std::time::Instant::now();
        let turn_result = session.turn(&case.input).await;
        let latency_ms = t0.elapsed().as_millis() as u64;
        let (actual, ok) = match turn_result {
            Ok(s)  => (s, true),
            // Per-case turn failures (network, MCP, budget abort)
            // become empty-actual + score 0. The eval continues so
            // the user sees the full run, not just the first
            // failure.
            Err(e) => (format!("(turn errored: {e})"), false),
        };
        let score = aislang_runtime::metric_score(eval.metric, &case.expected, &actual)
            .map_err(|e| anyhow::anyhow!("metric scoring failed: {e}"))?;

        // Post-turn line — overwrite the trailing `…` placeholder
        // with the score + latency. Newline ends the case line.
        let mark = if !ok { "✗ err" }
                   else if score >= 0.999 { "✓ 1.00" }
                   else if score >= eval.threshold.ratio { "✓" }
                   else                                  { "✗" };
        eprintln!("{mark} score={score:.3} ({latency_ms}ms)");

        case_results.push(aislang_runtime::CaseResult {
            id:       case.id.clone(),
            input:    case.input.clone(),
            expected: case.expected.clone(),
            actual,
            score,
            latency_ms,
            trace_url: case_url,
        });
        // _case_guard drops here → case span ends.
    }

    // ── 8. aggregate + threshold compare ──
    let scores: Vec<f64> = case_results.iter().map(|c| c.score).collect();
    let aggregate_score = aislang_runtime::eval_aggregate(eval.aggregator, &scores);
    let pass = aggregate_score >= eval.threshold.ratio;

    // ── 9. clean shutdown of MCP children ──
    session.shutdown().await
        .map_err(|e| anyhow::anyhow!("shutdown MCP children: {e}"))?;

    // Serialize metric + aggregator through Serde so we get the
    // `#[serde(rename_all = "snake_case")]` form (e.g. `exact_match`,
    // not the Debug-lowercased `exactmatch`). The values are simple
    // enum variants → serde_json::to_value yields a JSON string.
    let metric_str = serde_json::to_value(&eval.metric).ok()
        .and_then(|j| j.as_str().map(str::to_owned))
        .unwrap_or_else(|| "(unserializable)".to_string());
    let aggregator_str = serde_json::to_value(&eval.aggregator).ok()
        .and_then(|j| j.as_str().map(str::to_owned))
        .unwrap_or_else(|| "(unserializable)".to_string());

    Ok(aislang_runtime::EvalReport {
        eval_label:      eval.label.clone(),
        dataset_path:    dataset_path.display().to_string(),
        metric:          metric_str,
        aggregator:      aggregator_str,
        threshold:       eval.threshold.ratio,
        aggregate_score,
        pass,
        trace_url:       eval_trace_url,
        cases:           case_results,
    })
}

// ============================================================================
// Tests (unit-level only — CLI integration lives in tests/cli_chat.rs)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- levenshtein ----

    #[test] fn lev_identical_strings_distance_zero() {
        assert_eq!(levenshtein("support", "support"), 0);
    }

    #[test] fn lev_single_edit_distance_one() {
        assert_eq!(levenshtein("support", "suport"),  1);  // deletion
        assert_eq!(levenshtein("support", "supports"), 1); // insertion
        assert_eq!(levenshtein("support", "suppart"), 1);  // substitution
    }

    #[test] fn lev_two_edits_distance_two() {
        // "support" → "sport"   : two deletes (u, p).
        assert_eq!(levenshtein("support", "sport"),  2);
        // "support" → "suppurts": one substitute (o→u) + one insert (s).
        assert_eq!(levenshtein("support", "suppurts"), 2);
    }

    #[test] fn lev_classic_kitten_sitting_distance_three() {
        // The textbook Wagner-Fischer example. Pins correctness of
        // the table-walking loop independent of our specific inputs.
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test] fn lev_empty_strings() {
        assert_eq!(levenshtein("",        ""),        0);
        assert_eq!(levenshtein("support", ""),        7);
        assert_eq!(levenshtein("",        "support"), 7);
    }

    // ---- nearest_agent_label ----

    #[test] fn nearest_picks_close_match_within_threshold() {
        let cands = ["support", "billing", "analytics"];
        assert_eq!(nearest_agent_label("suport",  &cands), Some("support"));
        assert_eq!(nearest_agent_label("billng",  &cands), Some("billing"));
    }

    #[test] fn nearest_returns_none_when_no_match_within_threshold() {
        let cands = ["support", "billing"];
        // 6+ edits from any candidate — well beyond the ≤2 cap.
        assert_eq!(nearest_agent_label("zzzzzzzzz", &cands), None);
    }

    #[test] fn nearest_returns_none_for_empty_candidates() {
        let cands: [&str; 0] = [];
        assert_eq!(nearest_agent_label("support", &cands), None);
    }

    // ---- wait_for_port ----

    #[test] fn wait_for_port_returns_quickly_on_immediate_connect() {
        // Bind an actual listener on an OS-assigned port, then probe
        // that exact port. Should connect on the first try — wall
        // time well under the 5s timeout we pass.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port     = listener.local_addr().unwrap().port();
        let started  = std::time::Instant::now();
        wait_for_port("127.0.0.1", port, std::time::Duration::from_secs(5))
            .expect("immediate connect");
        let elapsed = started.elapsed();
        assert!(elapsed < std::time::Duration::from_secs(2),
            "expected <2s; took {elapsed:?}");
        drop(listener);
    }

    #[test] fn wait_for_port_times_out_when_nothing_listens() {
        // Probe a port that nothing's bound to. Use a port number
        // unlikely to clash (49152+ is IANA's ephemeral range). The
        // timeout we pass is intentionally short so the test finishes
        // fast — we just need the timeout path to actually trip.
        let started = std::time::Instant::now();
        let err = wait_for_port("127.0.0.1", 65534,
            std::time::Duration::from_millis(800))
            .err().expect("must time out");
        let msg = err.to_string();
        assert!(msg.contains("timed out"),
            "expected timeout message; got: {msg}");
        let elapsed = started.elapsed();
        assert!(elapsed >= std::time::Duration::from_millis(700),
            "must have actually waited; only {elapsed:?} elapsed");
        assert!(elapsed < std::time::Duration::from_secs(5),
            "should not overshoot timeout by 4x+; got {elapsed:?}");
    }

    #[test] fn nearest_picks_minimum_distance_when_multiple_match() {
        // Both "support" and "supports" are within threshold of
        // "suporrt" (dist 2 and 2). Tie-broken by iteration order
        // (min_by_key keeps the first). Either is acceptable; pin
        // behaviour so a refactor doesn't silently flip it.
        let cands = ["billing", "support", "supports"];
        let pick = nearest_agent_label("suporrt", &cands);
        assert!(pick == Some("support") || pick == Some("supports"),
            "expected close-match; got {pick:?}");
    }

    // ---- resolve_mcp_secrets — scoping by construction (Decision 9) ----

    /// Build a synthetic `Project` with two mcp_servers and two
    /// secrets:
    ///
    ///   - `mcp_server.alpha`: declares `secrets = [a_only]`.
    ///   - `mcp_server.beta`:  declares `secrets = [b_only]`.
    ///   - `secret "a_only"`: scoped to `alpha` per source (informational
    ///     here — runtime relies on mcp.secrets, not secret.scope).
    ///   - `secret "b_only"`: scoped to `beta`.
    ///
    /// Both secrets resolve from `env://TEST_SCOPE_*` env vars
    /// set by the test.
    fn synthetic_two_mcp_two_secret() -> aislang_core::ast::Project {
        use aislang_core::ast::*;
        use aislang_core::types::OciImage;
        let mk_mcp = |label: &str, secret_label: &str| McpServer {
            label: label.to_string(),
            image: OciImage {
                registry:   "docker.io".into(),
                repository: "mcp/x".into(),
                tag:        Some("0".into()),
                digest:     None,
            },
            mount:   None,
            env:     None,
            secrets: Some(vec![Ref {
                kind:  RefKind::Secret,
                label: secret_label.to_string(),
                module: None,
            }]),
        };
        Project {
            name: "x".into(),
            mcp_servers: vec![
                mk_mcp("alpha", "a_only"),
                mk_mcp("beta",  "b_only"),
            ],
            secrets: vec![
                Secret {
                    label:  "a_only".into(),
                    source: "env://TEST_SCOPE_A_ONLY".into(),
                    scope:  vec![Ref { kind: RefKind::McpServer, label: "alpha".into(), module: None }],
                },
                Secret {
                    label:  "b_only".into(),
                    source: "env://TEST_SCOPE_B_ONLY".into(),
                    scope:  vec![Ref { kind: RefKind::McpServer, label: "beta".into(),  module: None }],
                },
            ],
            ..Project::default()
        }
    }

    #[test] fn scoping_alpha_secret_not_in_beta_env() {
        // Decision 9: by construction, only secrets declared in
        // `mcp_server.secrets = [...]` reach that container's env.
        // Pin: alpha's secret never appears in beta's resolved env,
        // and vice versa.
        std::env::set_var("TEST_SCOPE_A_ONLY", "alpha-val");
        std::env::set_var("TEST_SCOPE_B_ONLY", "beta-val");
        let project = synthetic_two_mcp_two_secret();
        let alpha = &project.mcp_servers[0];
        let beta  = &project.mcp_servers[1];

        let alpha_env = resolve_mcp_secrets(&project, alpha, Path::new("/tmp")).unwrap();
        let beta_env  = resolve_mcp_secrets(&project, beta,  Path::new("/tmp")).unwrap();

        // Alpha sees exactly its declared secret. Beta's name doesn't appear.
        let alpha_keys: Vec<&str> = alpha_env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(alpha_keys, vec!["a_only"],
            "alpha must see ONLY its declared secret; got: {alpha_keys:?}");
        assert!(alpha_env.iter().any(|(k, v)| k == "a_only" && v == "alpha-val"));

        // Beta sees exactly its declared secret.
        let beta_keys: Vec<&str> = beta_env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(beta_keys, vec!["b_only"],
            "beta must see ONLY its declared secret; got: {beta_keys:?}");

        // CROSS-CHECK: alpha's env has no b_only; beta's has no a_only.
        assert!(!alpha_env.iter().any(|(k, _)| k == "b_only"),
            "alpha must NOT see beta's secret; got: {alpha_env:?}");
        assert!(!beta_env .iter().any(|(k, _)| k == "a_only"),
            "beta must NOT see alpha's secret; got: {beta_env:?}");

        std::env::remove_var("TEST_SCOPE_A_ONLY");
        std::env::remove_var("TEST_SCOPE_B_ONLY");
    }

    #[test] fn mcp_with_no_secrets_field_yields_empty_env() {
        // No secrets declared → empty env (not an error).
        use aislang_core::ast::*;
        use aislang_core::types::OciImage;
        let project = Project {
            name: "x".into(),
            mcp_servers: vec![McpServer {
                label: "free".into(),
                image: OciImage {
                    registry: "x".into(), repository: "y".into(),
                    tag: None, digest: None,
                },
                mount: None, env: None,
                secrets: None,   // explicitly absent
            }],
            ..Project::default()
        };
        let env = resolve_mcp_secrets(&project, &project.mcp_servers[0],
                                       Path::new("/tmp")).unwrap();
        assert!(env.is_empty());
    }

    #[test] fn mcp_referencing_unknown_secret_errors_cleanly() {
        // Defense in depth: if a `mcp_server.secrets` ref points
        // at a non-existent secret block (which the checker should
        // already reject), we surface a clean error rather than
        // panic.
        use aislang_core::ast::*;
        use aislang_core::types::OciImage;
        let project = Project {
            name: "x".into(),
            mcp_servers: vec![McpServer {
                label: "borked".into(),
                image: OciImage {
                    registry: "x".into(), repository: "y".into(),
                    tag: None, digest: None,
                },
                mount: None, env: None,
                secrets: Some(vec![Ref {
                    kind:  RefKind::Secret,
                    label: "phantom".into(),
                    module: None,
                }]),
            }],
            secrets: vec![],   // phantom doesn't exist
            ..Project::default()
        };
        let err = resolve_mcp_secrets(&project, &project.mcp_servers[0],
                                       Path::new("/tmp")).err().unwrap();
        assert!(err.to_string().contains("phantom"),
            "error must name the missing secret; got: {err}");
    }

    // ── base64 decoder used by preflight non-ASCII probe (Thu) ──

    #[test] fn decode_base64_round_trips_ascii() {
        // "sk-test-key" — what a real API key body looks like.
        let s = "c2stdGVzdC1rZXk=";
        let decoded = decode_base64(s).expect("valid base64");
        assert_eq!(String::from_utf8_lossy(&decoded), "sk-test-key");
        assert!(decoded.iter().all(|&b| b <= 0x7f),
            "ASCII-only input must decode to all-ASCII bytes");
    }

    #[test] fn decode_base64_round_trips_non_ascii() {
        // Decodes "sk-ant-\u2026YOUR-KEY\u2026" — the bug-reproducing
        // case the preflight catches.  base64 of those 18 bytes:
        let raw = format!("sk-ant-{}YOUR-KEY{}", '\u{2026}', '\u{2026}');
        // Hand-roll base64 encode for the test (we only have decode).
        let encoded = simple_b64_encode(raw.as_bytes());
        let decoded = decode_base64(&encoded).expect("round-trip");
        assert_eq!(decoded, raw.as_bytes());
        assert!(decoded.iter().any(|&b| b > 0x7f),
            "non-ASCII unicode (`\u{2026}` = 3 bytes UTF-8) must surface as >0x7f bytes — \
             that's what triggers the preflight non-ASCII warning");
    }

    #[test] fn decode_base64_rejects_invalid_input() {
        assert!(decode_base64("not!valid").is_none(),
            "invalid base64 → None (preflight skips the non-ASCII probe rather \
             than false-positive)");
        assert!(decode_base64("abc").is_none(),
            "non-multiple-of-4 length → None");
    }

    #[test] fn decode_base64_handles_padding() {
        // "ab" = 2 bytes → base64 "YWI=" (1 pad)
        assert_eq!(decode_base64("YWI=").unwrap(), b"ab");
        // "a"  = 1 byte  → base64 "YQ==" (2 pad)
        assert_eq!(decode_base64("YQ==").unwrap(), b"a");
    }

    /// Tiny base64 encoder for the round-trip test above; not used
    /// in the production path (decode_base64 is the one in main.rs).
    fn simple_b64_encode(bytes: &[u8]) -> String {
        const ALPHA: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        let mut i = 0;
        while i + 3 <= bytes.len() {
            let n = ((bytes[i]   as u32) << 16)
                  | ((bytes[i+1] as u32) << 8)
                  | (bytes[i+2]  as u32);
            for shift in [18, 12, 6, 0] {
                out.push(ALPHA[((n >> shift) & 0x3f) as usize] as char);
            }
            i += 3;
        }
        match bytes.len() - i {
            1 => {
                let n = (bytes[i] as u32) << 16;
                out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
                out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
                out.push('=');
                out.push('=');
            }
            2 => {
                let n = ((bytes[i] as u32) << 16) | ((bytes[i+1] as u32) << 8);
                out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
                out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
                out.push(ALPHA[((n >>  6) & 0x3f) as usize] as char);
                out.push('=');
            }
            _ => {}
        }
        out
    }
}

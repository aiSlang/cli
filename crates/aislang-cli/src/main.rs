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
        /// v0.3 Week 20 Tue: pause between pipeline steps so the
        /// user can hand-edit the previous step's persisted output
        /// before the next step picks it up.  Every step's reply
        /// is ALWAYS persisted under `<project_root>/.aislang/
        /// pipeline/<pipeline_label>/step_<N>_<agent>.md`
        /// regardless of this flag — the flag controls whether the
        /// CLI pauses + re-reads the file as the next step's
        /// input.  Without `--interactive`, the orchestrator
        /// runs unchanged from the Mon/W17-Thu pipe-forward
        /// behaviour.  No-op for single-agent + router branches.
        #[arg(long, default_value_t = false)]
        interactive: bool,
        /// v0.4 Week 21 Mon: keep `<project>/.aislang/pipeline/
        /// <pipeline_label>/` from the previous chat session.
        /// Default behaviour wipes that dir at session start so
        /// each chat session leaves a clean trace (user-reported
        /// bug 4 — old + new chat artifacts mixed in the dir).
        /// `--keep-history` opts out.  No-op for single-agent +
        /// router paths (no persistence dir).
        #[arg(long, default_value_t = false)]
        keep_history: bool,
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
    /// Run a declared `skill { … }` block against the runtime.
    /// v0.3 Week 18 Thu follow-up — gives `.ais` authors a CLI
    /// for end-to-end skill testing without writing Rust glue.
    ///
    /// Shell-exec skills (`exec = "<cmd>"`) run directly through
    /// `aislang-runtime::skills::run_shell_skill` against the
    /// per-skill sandbox (per-skill cwd + scrubbed PATH +
    /// command denylist).
    ///
    /// MCP-delegate skills (`mcp = mcp_server.<s>.tool.<t>`)
    /// spawn the referenced `mcp_server` block's container via
    /// `docker run -i`, route the call through
    /// `run_mcp_skill`, then shut down the child cleanly.
    /// Requires Docker on PATH.
    ///
    /// Prints the typed [`SkillOutput`] as text on stdout
    /// (variant-specific formatting via `SkillOutput::to_text`).
    /// Exit 0 on success; exit 1 with the rendered error on
    /// any `SkillError` (denylist, non-zero exit, MCP failure,
    /// etc.).
    Skill {
        #[command(subcommand)]
        action: SkillCommand,
    },
    /// RAG runtime operations against declared `vector_store`
    /// blocks (Decision 44.6).  v0.3 Week 19 Wed follow-up —
    /// gives `.ais` authors a CLI for end-to-end RAG testing
    /// without writing Rust glue.
    Rag {
        #[command(subcommand)]
        action: RagCommand,
    },
    /// v0.4 Track C — `aislang-cloud` managed control plane
    /// (Decision 46.1).  Today's `serve` subcommand boots the REST
    /// endpoint surface against the in-memory project store; Tue's
    /// Postgres schema-per-tenant swap stays inside the cloud
    /// crate so this CLI surface is byte-stable across the
    /// W22 Phase 1 days.
    Cloud {
        #[command(subcommand)]
        action: CloudCommand,
    },
    /// v0.4 Week 25 Mon — delegate a CLI verb to a remote control
    /// plane (Decision 46.9, mission deliverable #2).  The same
    /// `aislang` binary self-hosted + cloud users already run; only
    /// `--target` differs.  Verbs operate on a project by its
    /// server-stamped id (create projects via the REST API or the
    /// signup flow first).
    ///
    /// ```sh
    /// export AISLANG_CLOUD_TOKEN=…           # or pass --token
    /// aislang remote --target=https://api.aislang.cloud \
    ///   --tenant=acme validate p-abc123
    /// aislang remote --target=… --tenant=acme plan    p-abc123
    /// aislang remote --target=… --tenant=acme apply   p-abc123
    /// aislang remote --target=… --tenant=acme destroy p-abc123
    /// aislang remote --target=… --tenant=acme status  p-abc123
    /// ```
    Remote {
        /// Control-plane base URL, e.g. `https://api.aislang.cloud`
        /// (trailing slash optional).
        #[arg(long, value_name = "URL")]
        target: String,
        /// Tenant slug sent as `X-Aislang-Tenant` (Decision 46.5).
        /// Defaults to the `AISLANG_TENANT` env var, else `default`
        /// (the self-hosted single-tenant convention).
        #[arg(long, value_name = "SLUG")]
        tenant: Option<String>,
        /// Bearer token.  Defaults to `AISLANG_CLOUD_TOKEN` (loaded
        /// from the nearest `.env.cloud`, same as `aislang cloud
        /// serve`).
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
        #[command(subcommand)]
        action: RemoteCommand,
    },
}

/// v0.4 Week 25 Mon — `aislang remote` verbs.  Each delegates to the
/// control plane over the Decision-46.1 REST surface; see the
/// per-verb endpoint map in `aislang_cloud::remote`.
#[derive(Subcommand, Debug)]
enum RemoteCommand {
    /// Parse + typecheck + resolve a project server-side.  FREE — not
    /// metered, not rate-limited (Decision 46.10).  Exit 0 if valid,
    /// 2 if the source fails to load.
    Validate {
        /// Server-stamped project id (e.g. `p-abc123`).
        id: String,
    },
    /// Plan a project server-side + print the generated Compose YAML
    /// to stdout.  Billed 1 SP-equivalent (Decision 46.3 / 46.10).
    Plan { id: String },
    /// Apply a project server-side — streams progress events.  Billed
    /// 1 SP-equivalent.  Exit 0 on `done`, 1 on an `error` event.
    Apply { id: String },
    /// Tear down a project's applied stack — streams progress events.
    /// Billed 1 SP-equivalent + trails a `project.destroy` audit row.
    Destroy { id: String },
    /// Print a project's stored record (id / name / version /
    /// timestamps).
    Status { id: String },
}

#[derive(Subcommand, Debug)]
enum SkillCommand {
    /// Run one skill block end-to-end.
    Run {
        /// Skill label as declared in the .ais file
        /// (e.g. `list_files`).
        label: String,
        /// Path to the .ais project file (single-file) OR the
        /// project root dir (multi-module).  Same dispatch rules
        /// as `aislang chat`.
        project: PathBuf,
        /// JSON args for MCP-delegate skills.  Required when the
        /// referenced MCP tool needs structured input (e.g.
        /// `{"path":"/data/hello.txt"}`); ignored for shell-exec
        /// skills.  Defaults to `{}`.
        #[arg(long, value_name = "JSON", default_value = "{}")]
        args: String,
    },
}

#[derive(Subcommand, Debug)]
enum RagCommand {
    /// Build the RAG index for one `vector_store` block from its
    /// declared `source` URI, embed every chunk via the
    /// referenced `embedding` block (POSTs through LiteLLM at
    /// `--litellm-url`), and persist to
    /// `<project>/.aislang/vector/<label>.bin`.  Subsequent
    /// `rag query` calls use the cached index.
    Build {
        /// `vector_store` label as declared in the .ais file.
        label: String,
        /// Path to the .ais project file or project root dir.
        project: PathBuf,
        /// LiteLLM proxy base URL (default: localhost:4000 — the
        /// `aislang apply`-spun Compose stack default).
        #[arg(long, value_name = "URL", default_value = "http://localhost:4000")]
        litellm_url: String,
    },
    /// Embed `<query>` via the vector_store's embedding model + return
    /// the top-K matching chunks (default 3) by cosine similarity.
    /// Builds the index on first call if `<project>/.aislang/vector/
    /// <label>.bin` doesn't exist yet.
    Query {
        /// `vector_store` label.
        label: String,
        /// Path to the .ais project file or project root dir.
        project: PathBuf,
        /// Query text.
        query: String,
        /// Top-K results to return (default 3).
        #[arg(long, value_name = "N", default_value_t = 3)]
        top_k: usize,
        /// LiteLLM proxy base URL (default: localhost:4000).
        #[arg(long, value_name = "URL", default_value = "http://localhost:4000")]
        litellm_url: String,
    },
    /// v0.4 W21 Tue — scan every `vector_store` declared in a
    /// project + report chunk counts per persisted index.  Exits
    /// non-zero when any index is empty (0 chunks) or missing
    /// from disk; exits 0 when every declared store has a healthy
    /// index.  Closes the W20 Thu dogfood-finding gap (silent
    /// empty-RAG indexes masked the Coder hallucination root
    /// cause); pairs with the chat-time empty-RAG warning that
    /// fires when a user starts a chat against an agent whose
    /// `knowledge` refs a store with 0 chunks.
    Verify {
        /// Path to the .ais project file or project root dir.
        project: PathBuf,
        /// Treat a missing index file the same as a 0-chunk index
        /// (both exit non-zero).  Default: missing = non-zero,
        /// empty = non-zero — `--strict` is a no-op today but
        /// reserved for future per-store severity levels.
        #[arg(long, default_value_t = false)]
        strict: bool,
    },
    /// v0.4 W26 — build EVERY declared `vector_store`'s index in one go,
    /// tolerating per-store failures (logs them, exits 0 so a stack still
    /// comes up).  Indexes land in `AISLANG_VECTOR_DIR` if set, else
    /// `<project>/.aislang/vector/`.  Used by the managed-cloud agent-runtime
    /// entrypoint to build `git:` knowledge corpora in-pod before serving.
    BuildAll {
        /// Path to the .ais project file or project root dir.
        project: PathBuf,
        /// LiteLLM proxy base URL (default: localhost:4000).
        #[arg(long, value_name = "URL", default_value = "http://localhost:4000")]
        litellm_url: String,
    },
}

#[derive(Subcommand, Debug)]
enum CloudCommand {
    /// Boot the REST endpoint surface — `axum::serve` against the
    /// configured port.  Reads the bearer token from
    /// `AISLANG_CLOUD_TOKEN` and refuses to start with an empty
    /// token (server would otherwise accept unauthenticated
    /// requests).
    ///
    /// Quick smoke test:
    ///
    /// ```sh
    /// AISLANG_CLOUD_TOKEN=hunter2 aislang cloud serve &
    /// curl -H 'Authorization: Bearer hunter2' \
    ///      -H 'X-Aislang-Tenant: alpha' \
    ///      http://127.0.0.1:4001/v1/projects
    /// # → {"projects":[]}
    /// ```
    Serve {
        /// Bind host (default 127.0.0.1 — does NOT bind 0.0.0.0
        /// by default to avoid accidentally exposing an
        /// unhardened control plane to the network).
        #[arg(long, value_name = "HOST", default_value = "127.0.0.1")]
        host: String,
        /// Bind port (default 4001 from
        /// `aislang_cloud::DEFAULT_PORT`).
        #[arg(long, value_name = "PORT",
              default_value_t = aislang_cloud::DEFAULT_PORT)]
        port: u16,
    },
    /// v0.4 W22 Tue follow-up — provision a tenant's Postgres
    /// schema + tenant-migrations.  Idempotent (re-running against
    /// an already-provisioned tenant is a no-op).
    ///
    /// Without this step, `GET /v1/projects` against an
    /// un-provisioned tenant returns 500 (`relation "projects"
    /// does not exist`) because the per-tenant schema hasn't been
    /// created yet.  Future Phase 2 Week 23 signup flow will call
    /// this automatically on first signup; for now, operators run
    /// it explicitly per tenant (mirrors the `aislang apply` posture
    /// where operator action provisions infrastructure).
    ///
    /// Requires `AISLANG_CLOUD_DB_URL` (the in-memory store has no
    /// schema to provision; running this against the fallback store
    /// surfaces a clear error).
    ///
    /// ```sh
    /// AISLANG_CLOUD_DB_URL=postgres://... \
    ///   aislang cloud provision alpha
    /// ```
    Provision {
        /// Tenant slug to provision.  Must match
        /// `^[a-z0-9][a-z0-9-]{0,63}$` (validated against the same
        /// regex `public.tenants.slug`'s CHECK constraint enforces
        /// at the DB layer).
        tenant: String,
    },
    /// v0.4 W25 — fully tear down a tenant + ALL of its data: drop
    /// the tenant schema (projects / applied_stacks / idempotency_keys
    /// / audit_log) + delete its shared-schema rows (`public.tenants`
    /// / `tenant_members` / `project_repos` / `signups`).
    ///
    /// The inverse of `provision`.  **Destructive + irreversible** —
    /// use for offboarding a customer or cleaning up test tenants.
    /// Idempotent: deprovisioning an unknown tenant is a no-op.
    ///
    /// Requires `AISLANG_CLOUD_DB_URL`.
    ///
    /// ```sh
    /// AISLANG_CLOUD_DB_URL=postgres://... \
    ///   aislang cloud deprovision acme
    /// ```
    Deprovision {
        /// Tenant slug to tear down.
        tenant: String,
    },
    /// v0.4 W23 Mon — Stripe metering API spike (Decision 46.3,
    /// risk-3 mitigation per §"Three v0.4 risks worth pre-
    /// mitigating").  Submits a single meter event to the Stripe
    /// Billing meter-events API + prints the server-echoed
    /// response.
    ///
    /// Two meter kinds per Decision 46.3:
    ///   - `tokens`  per-1k-tokens for inference (mirrors v0.1
    ///               `spend.json` micro-units field shape)
    ///   - `sp`      per-SP-equivalent for control-plane operations
    ///               (`apply`/`destroy`/`plan` = 1 SP-equivalent
    ///               for billing)
    ///
    /// Both meter names must be pre-created in your Stripe
    /// dashboard before submitting events — see
    /// `docs/stripe-integration-notes.md` § "Meter must exist
    /// before events".
    ///
    /// Requires `STRIPE_SECRET_KEY` env var (use `sk_test_...` for
    /// sandbox; `sk_live_...` for production).  Optional
    /// `STRIPE_API_BASE` override for sandbox-mocks +
    /// stripe-mock locally.
    ///
    /// ```sh
    /// STRIPE_SECRET_KEY=sk_test_... \
    ///   aislang cloud meter-event \
    ///     --customer cus_xxx \
    ///     --kind tokens \
    ///     --quantity 1000
    /// ```
    MeterEvent {
        /// Stripe customer id (`cus_...`) to charge for this event.
        /// In production this comes from the per-tenant Stripe
        /// customer record persisted at signup; the spike CLI
        /// takes it as a flag.
        #[arg(long, value_name = "CUSTOMER_ID")]
        customer: String,
        /// Meter kind: `tokens` (per-1k-tokens inference) OR `sp`
        /// (per-SP-equivalent control-plane operation).  See
        /// Decision 46.3.
        #[arg(long, value_name = "KIND")]
        kind: String,
        /// Quantity to record.  Stripe accumulates these across
        /// events for the billing period.
        #[arg(long, value_name = "N")]
        quantity: u64,
        /// Override the Stripe `identifier` field for idempotency
        /// testing.  Defaults to a server-generated unique value.
        /// Re-submitting the same identifier + same payload is a
        /// no-op on Stripe's side (idempotent retry).
        #[arg(long, value_name = "ID")]
        identifier: Option<String>,
    },
    /// v0.4 W23 Mon follow-up — write `.env.cloud.example` to cwd.
    ///
    /// The template lists every operator-facing env var the
    /// `aislang-cloud` crate reads at startup (control-plane
    /// bearer auth, Postgres, Stripe Billing).  Copy to
    /// `.env.cloud`, fill in the values, and re-run any
    /// `aislang cloud <subcommand>` — each subcommand auto-loads
    /// the nearest `.env.cloud` from cwd / parent dirs BEFORE
    /// reading env vars.
    ///
    /// This template is SEPARATE from the per-project
    /// `.env.example` that `aislang plan` emits — those cover the
    /// project's provider credentials (consumed by docker
    /// compose); this one covers the cloud OPERATOR's
    /// control-plane secrets (consumed by the aislang binary
    /// directly).
    ///
    /// ```sh
    /// aislang cloud init                  # writes .env.cloud.example to cwd
    /// cp .env.cloud.example .env.cloud
    /// # edit .env.cloud and fill in AISLANG_CLOUD_TOKEN etc.
    /// echo .env.cloud >> .gitignore
    /// aislang cloud serve                 # auto-loads .env.cloud
    /// ```
    Init {
        /// Overwrite an existing `.env.cloud.example` if present.
        /// Default: refuse + print a clear message so we don't
        /// stomp operator edits.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// v0.4 W23 Tue — map a tenant slug to its Stripe customer
    /// id (Decision 46.3 metering pipeline).
    ///
    /// Phase 2 Thu W23 signup flow will set this automatically
    /// after a successful Stripe Checkout; today the operator
    /// runs it explicitly per tenant (mirrors `aislang cloud
    /// provision`'s explicit-action posture).
    ///
    /// Tenants without a Stripe customer mapping accumulate
    /// meter events in the in-process queue but the flusher
    /// drops them (with a logged warning) on each tick — no
    /// silent revenue loss, but no Stripe-side accounting
    /// either.  Re-running this subcommand after the fact does
    /// NOT replay dropped events; v0.5+ may add explicit
    /// backfill.
    ///
    /// Requires `AISLANG_CLOUD_DB_URL`.  Customer ids must
    /// match `^cus_[A-Za-z0-9]{1,60}$` (matches Stripe's actual
    /// shape + the DB-layer CHECK constraint at
    /// `migrations/20260609000000_tenants_stripe_customer.sql`).
    SetCustomer {
        /// Tenant slug (must already be provisioned).
        tenant: String,
        /// Stripe customer id, e.g. `cus_OrK5jdN5VbcM9G`.
        stripe_customer_id: String,
    },
    /// v0.4 W23 Wed — set a tenant's billing plan (Decision 46.3
    /// free-tier rate-limiting).
    ///
    /// `free` (default for new tenants) rate-limits control-plane
    /// SP-equivalent operations (`apply` etc.) at 50/day; `paid`
    /// is unrate-limited.  Phase 2 Thu W23 signup flow flips a
    /// tenant to `paid` automatically after payment requisites
    /// are confirmed; today the operator runs it explicitly.
    ///
    /// Requires `AISLANG_CLOUD_DB_URL` (the plan must persist
    /// across `aislang cloud serve` restarts).
    SetPlan {
        /// Tenant slug (must already be provisioned).
        tenant: String,
        /// Plan: `free` (rate-limited 50 SP-equivalents/day) or
        /// `paid` (unlimited).
        plan: String,
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
        Command::Chat { agent, file, interactive, keep_history } =>
            run_chat(agent, file, interactive, keep_history).map(|_| ExitCode::SUCCESS)?,
        Command::McpServe { stdio, http } =>
            run_mcp_serve(stdio, http).map(|_| ExitCode::SUCCESS)?,
        Command::Skill { action } => match action {
            SkillCommand::Run { label, project, args } =>
                run_skill(label, project, args).map(|_| ExitCode::SUCCESS)?,
        },
        Command::Rag { action } => match action {
            RagCommand::Build { label, project, litellm_url } =>
                run_rag_build(label, project, litellm_url).map(|_| ExitCode::SUCCESS)?,
            RagCommand::Query { label, project, query, top_k, litellm_url } =>
                run_rag_query(label, project, query, top_k, litellm_url)
                    .map(|_| ExitCode::SUCCESS)?,
            RagCommand::Verify { project, strict } =>
                run_rag_verify(project, strict)?,
            RagCommand::BuildAll { project, litellm_url } =>
                run_rag_build_all(project, litellm_url).map(|_| ExitCode::SUCCESS)?,
        },
        Command::Cloud { action } => match action {
            CloudCommand::Serve { host, port } =>
                run_cloud_serve(host, port).map(|_| ExitCode::SUCCESS)?,
            CloudCommand::Provision { tenant } =>
                run_cloud_provision(tenant).map(|_| ExitCode::SUCCESS)?,
            CloudCommand::Deprovision { tenant } =>
                run_cloud_deprovision(tenant).map(|_| ExitCode::SUCCESS)?,
            CloudCommand::MeterEvent { customer, kind, quantity, identifier } =>
                run_cloud_meter_event(customer, kind, quantity, identifier)
                    .map(|_| ExitCode::SUCCESS)?,
            CloudCommand::Init { force } =>
                run_cloud_init(force).map(|_| ExitCode::SUCCESS)?,
            CloudCommand::SetCustomer { tenant, stripe_customer_id } =>
                run_cloud_set_customer(tenant, stripe_customer_id)
                    .map(|_| ExitCode::SUCCESS)?,
            CloudCommand::SetPlan { tenant, plan } =>
                run_cloud_set_plan(tenant, plan).map(|_| ExitCode::SUCCESS)?,
        },
        Command::Remote { target, tenant, token, action } =>
            run_remote(target, tenant, token, action)?,
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

    // v0.3 Week 18 Mon follow-up: install the OTel tracer BEFORE the
    // runtime starts.  With the new `ThreadedSpanProcessor` (see
    // `aislang-runtime::tracing` docs), span export happens on a
    // dedicated OS thread + can't conflict with the tokio chat-turn
    // lifecycle.  This re-enables aislang-side tracing for the chat
    // flow — each chat_turn now produces a `aislang.turn` span +
    // child `tool_use_iteration[N]` spans + `chat <model>` spans
    // with the OTel-GenAI semantic attrs (gen_ai.system,
    // gen_ai.request.model, gen_ai.usage.input_tokens, etc.) defined
    // in `litellm.rs::gen_ai_*`.  Service name in Jaeger:
    // `aislang-mcp-serve`.  Fail-soft (same shape as `run_eval`):
    // exporter build failure logs + continues without tracing.
    if let Err(e) = aislang_runtime::init_tracer("aislang-mcp-serve") {
        eprintln!("aislang mcp-serve: tracing disabled: {e}");
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;

    rt.block_on(async {
        aislang_runtime::mcp_serve_stdio().await
            .map_err(|e| anyhow::anyhow!("mcp-serve: {e}"))
    })?;

    // Flush + drain the threaded exporter before exit so the last
    // in-flight batch lands in Jaeger.  Bounded by the processor's
    // 5s shutdown timeout — never blocks indefinitely.
    aislang_runtime::shutdown_tracer();

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

    // Mirror the stdio path: install the threaded-processor tracer
    // before the runtime starts.  See `run_mcp_serve` for the full
    // rationale.
    if let Err(e) = aislang_runtime::init_tracer("aislang-mcp-serve") {
        eprintln!("aislang mcp-serve: tracing disabled: {e}");
    }

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

    aislang_runtime::shutdown_tracer();

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
//
// v0.3 Week 17 Tue: the core loader hoisted to `aislang-runtime` so
// `aislang mcp-serve`'s `aislang_chat_turn` can share the same
// dispatcher.  The CLI keeps a thin wrapper that adds the K8s
// `ProjectSources` bag (which lives in `aislang-k8s` — we don't
// want the runtime crate to drag k8s deps just to satisfy the K8s
// emit path).
// ============================================================================

/// CLI-side wrapper around `aislang_runtime::LoadedProject` — adds
/// the K8s `ProjectSources` bag (built from the loader's per-module
/// data) so the K8s emitter call sites compile unchanged.
struct LoadedProject {
    inner:   aislang_runtime::LoadedProject,
    sources: aislang_k8s::ProjectSources,
}

impl std::ops::Deref for LoadedProject {
    type Target = aislang_runtime::LoadedProject;
    fn deref(&self) -> &Self::Target { &self.inner }
}

/// Load a project from a CLI positional path argument.  Wraps
/// [`aislang_runtime::load_project_input`] + builds the K8s
/// `ProjectSources` bag.  Source-aware parse-error rendering for
/// the user is preserved by matching on the structured
/// [`aislang_runtime::ProjectLoadError`] variants.
fn load_project_input(input: &Path) -> Result<LoadedProject> {
    let inner = aislang_runtime::load_project_input(input).map_err(|err| {
        match &err {
            aislang_runtime::ProjectLoadError::ParseSingle { source, errors } => {
                let verr = aislang_core::ValidateError::Parse(errors.clone());
                render_validate_error(source, &verr);
                anyhow::anyhow!("(see error above)")
            }
            aislang_runtime::ProjectLoadError::ParseModule {
                module, source, errors,
            } => {
                eprintln!("module `{module}`:");
                let verr = aislang_core::ValidateError::Parse(errors.clone());
                render_validate_error(source, &verr);
                anyhow::anyhow!("(see error above)")
            }
            _ => anyhow::anyhow!("{err}"),
        }
    })?;
    // Build the K8s emitter's ProjectSources bag from the loaded
    // per-module data.  Same shape as v0.3 Week 16 Thu.
    let sources = aislang_k8s::ProjectSources {
        ais_source:    inner.single_file_source.clone(),
        prompt_bodies: inner.prompt_bodies.clone(),
        modules:       inner.modules.clone(),
        manifest:      inner.manifest.clone(),
        ..Default::default()
    };
    Ok(LoadedProject { inner, sources })
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
    // W20 Wed follow-up: walk up from the project dir looking for
    // a `.env` so users can keep a single workspace-root `.env`
    // for many sub-projects.  Docker Compose only auto-loads `.env`
    // next to the compose file; when we find one further up we
    // pass it explicitly via `--env-file`.  No-op if a `.env`
    // already sits next to stack.yml (Compose handles that natively).
    let discovered_env = find_dotenv_walking_up(&prep.output_dir);
    if let Some(p) = discovered_env.as_ref() {
        eprintln!("  env file:         {} (picked up from a parent dir)",
            p.display());
    }
    eprintln!("bringing stack up: docker compose -f {} up -d", stack_path.display());
    let mut cmd = std::process::Command::new("docker");
    cmd.args(["compose", "-f"]).arg(&stack_path);
    if let Some(p) = discovered_env.as_ref() {
        cmd.args(["--env-file"]).arg(p);
    }
    cmd.args(["up", "-d"]);
    let status = cmd.status()
        .map_err(|e| anyhow::anyhow!("invoke docker compose: {e}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "docker compose up -d exited {:?}", status.code()));
    }

    // v0.3 Week 19 Wed follow-up: `docker compose up -d` returns
    // as soon as each container has STARTED — but LiteLLM's
    // python process inside takes ~5-10s MORE to bind :4000 +
    // load model_list + accept HTTP.  Running `aislang chat` /
    // `aislang rag build` immediately after apply surfaced as
    // "error sending request" (connection refused) — the user
    // had to retry to make their next command work.
    //
    // Reuse the smoke-test path's `wait_for_litellm_ready` helper
    // (polls /health/liveness via curl; 200-on-fully-up).  Apply
    // here is fail-soft on timeout: log + continue.  Forcing apply
    // to fail would be worse UX than letting the user retry their
    // next command if LiteLLM is genuinely slow to come up.
    match wait_for_litellm_ready(
        "http://127.0.0.1:4000",
        std::time::Duration::from_secs(30),
    ) {
        Ok(()) => eprintln!("  litellm ready:    ✓ /health/liveness 200"),
        Err(e) => eprintln!(
            "  warning: LiteLLM not ready within 30s ({e}); \
             retry your next command in a few seconds"),
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

    // v0.3 Week 19 Thu: auto-build RAG indexes for every declared
    // `vector_store` block.  Skipped silently when the project has
    // no vector_stores (the common single-agent v0.2 case).  Per-
    // store failures log + continue (don't abort apply — the
    // stack IS up; user can rebuild missing indexes manually via
    // `aislang rag build <label> <project>`).
    auto_build_rag_indexes(&file);

    eprintln!();
    eprintln!("stack is up. next steps:");
    if let Some(agent) = first_agent_label_in_file(&file) {
        eprintln!("  chat:      aislang chat {agent} {}", file.display());
    }
    eprintln!("  teardown:  aislang destroy {}", file.display());
    Ok(())
}

/// Walk up from `start` looking for a `.env` file.  Returns
/// `Some(<absolute path>)` for the first match, or `None` if the
/// search walks all the way to the filesystem root without
/// finding one.  Skips `.env.example` (template, not the user's
/// secrets file).
///
/// v0.3 Week 20 Wed.  Users keep a single workspace-root `.env`
/// for many sub-projects (the repo pattern: one `.env` next to
/// `Cargo.toml`, many `examples/agents/*/` sub-projects).  Docker
/// Compose only auto-loads `.env` next to the compose file —
/// pass it explicitly via `--env-file` when we find it further up.
fn find_dotenv_walking_up(start: &Path) -> Option<PathBuf> {
    let mut cur: PathBuf = start.to_path_buf();
    // Skip the start dir itself — Docker Compose handles that
    // case natively, no `--env-file` flag needed.
    while let Some(parent) = cur.parent() {
        let candidate = parent.join(".env");
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = parent.to_path_buf();
    }
    None
}

#[cfg(test)]
mod dotenv_walk_tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "aislang-cli-dotenv-walk-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(
                std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn find_dotenv_walking_up_returns_parent_match() {
        // `.env` at `<root>/.env`; start = `<root>/sub/project/`;
        // expect to find the root `.env` two levels up.
        let root = tempdir();
        std::fs::write(root.join(".env"), b"OLLAMA_API_BASE=...").unwrap();
        let nested = root.join("sub").join("project");
        std::fs::create_dir_all(&nested).unwrap();
        let found = find_dotenv_walking_up(&nested)
            .expect("should walk up to root and find .env");
        assert_eq!(found, root.join(".env"));
    }

    #[test]
    fn find_dotenv_walking_up_skips_start_dir() {
        // Docker Compose already handles `.env` next to the stack
        // — `find_dotenv_walking_up` is for the PARENT-dir case
        // ONLY.  A `.env` IN `start` returns `None` so the caller
        // doesn't pass redundant `--env-file`.
        let root = tempdir();
        std::fs::write(root.join(".env"), b"X=1").unwrap();
        assert_eq!(find_dotenv_walking_up(&root), None,
            "start dir match must not be returned — that's Compose's default behaviour");
    }

    #[test]
    fn find_dotenv_walking_up_returns_none_when_no_env_in_ancestry() {
        let root = tempdir();
        let nested = root.join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        // No `.env` anywhere — must walk to fs root + return None
        // without panicking.
        assert_eq!(find_dotenv_walking_up(&nested), None);
    }

    #[test]
    fn find_dotenv_walking_up_picks_closest_match() {
        // Closer .env wins over farther one (mirrors expected
        // human intuition + matches how Compose ascent-search
        // behaves elsewhere).
        let root = tempdir();
        std::fs::write(root.join(".env"), b"FAR=1").unwrap();
        let mid  = root.join("workspace");
        std::fs::create_dir_all(&mid).unwrap();
        std::fs::write(mid.join(".env"), b"CLOSE=1").unwrap();
        let nested = mid.join("project");
        std::fs::create_dir_all(&nested).unwrap();
        let found = find_dotenv_walking_up(&nested).expect("walk");
        assert_eq!(found, mid.join(".env"));
    }
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
    // v0.3 Week 20 Wed follow-up: dir-vs-file dispatch via
    // load_project_input (same pattern as the W20 Mon `aislang
    // status` dir-input fix).  `aislang cost <multi-module-dir>/`
    // was failing with `Is a directory (os error 21)` because
    // this fn still used the v0.2 direct `read_to_string` path.
    let loaded = load_project_input(&file)?;
    let project = &loaded.resolved.project;

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
    // v0.3 Week 20 Mon follow-up: dir-vs-file dispatch via
    // load_project_input.  Single-file input → source_dir = file
    // parent; multi-module dir input → source_dir = the dir itself.
    // Status got left out of the Week 17 Tue dir-input wave because
    // it was rarely exercised; the W20 multi-module orchestrator
    // surfaced the gap when `aislang status <project-dir>/` failed
    // with `Is a directory (os error 21)`.
    let loaded = load_project_input(&file)?;
    let project = &loaded.resolved.project;

    let source_dir = if file.is_dir() {
        file.as_path()
    } else {
        file.parent().unwrap_or_else(|| Path::new("."))
    };
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
/// v0.3 Week 17 Fri — router-pattern dispatch state.
///
/// Built once at `run_chat` entry when the user typed a branch label;
/// each user message in the chat loop runs the classifier → matches a
/// case key → dispatches the matched case's pipeline as an
/// orchestrator run.  Resolving steps upfront keeps the per-turn hot
/// path simple (no AST lookups during the loop).
///
/// **v0.3 classifier-agent convention**: the project's FIRST declared
/// agent (in source order) is used to run the classifier turn.  This
/// avoids adding a new grammar field to `branch { … }` for v0.3 Fri;
/// v0.5+ may add an explicit `classifier = agent.<label>` field.
/// Demos that exercise the router pattern should declare a toolless
/// classifier-style agent FIRST.
struct RouterDispatch {
    /// User-facing label (banner) — the qualified branch label.
    branch_label:           String,
    /// Qualified agent label used for the classifier turn.
    classifier_agent_label: String,
    /// `(case-key, qualified-step-labels)` pairs, source order
    /// preserved so the banner can show the route table deterministically.
    case_keys_and_steps:    Vec<(String, Vec<String>)>,
    /// Qualified step labels for the branch's `default` pipeline.
    /// Falls back to this when no case key matches the classifier reply.
    default_steps:          Vec<String>,
}

impl RouterDispatch {
    fn build(
        loaded: &aislang_runtime::LoadedProject,
        branch: &aislang_core::ast::Branch,
    ) -> Result<Self> {
        let project = &loaded.resolved.project;
        // Classifier: first agent in source order (v0.3 convention).
        // If the project has no agents at all, fail with a clear hint.
        let classifier_agent = project.agents.first().ok_or_else(|| {
            anyhow::anyhow!(
                "branch `{}` needs at least one agent in the project to act as \
                 the router classifier (v0.3 uses the first declared agent)",
                branch.label)
        })?;
        let classifier_agent_label = classifier_agent.label.clone();

        // Each case-pipeline must resolve to a non-empty step list.
        // Cases preserve source order (BTreeMap iteration is
        // deterministic by key — good enough for v0.3).
        let mut case_keys_and_steps = Vec::with_capacity(branch.cases.len());
        for (case_key, case_ref) in &branch.cases {
            let pipeline = project.pipelines.iter()
                .find(|p| {
                    // Match by qualified label OR bare label given the
                    // case ref's optional module field.
                    match (&case_ref.module, p.label.split_once('.')) {
                        (Some(m), Some((pm, pl))) => m == pm && pl == case_ref.label,
                        _ => p.label == case_ref.label
                            || p.label.ends_with(&format!(".{}", case_ref.label)),
                    }
                })
                .ok_or_else(|| anyhow::anyhow!(
                    "branch `{}` case `{case_key}` references unknown pipeline `{}`",
                    branch.label, case_ref.label))?;
            if pipeline.steps.is_empty() {
                return Err(anyhow::anyhow!(
                    "branch `{}` case `{case_key}` → pipeline `{}` has no steps",
                    branch.label, pipeline.label));
            }
            case_keys_and_steps.push((case_key.clone(),
                qualify_pipeline_steps(pipeline)));
        }

        let default_pipeline = project.pipelines.iter()
            .find(|p| {
                match (&branch.default.module, p.label.split_once('.')) {
                    (Some(m), Some((pm, pl))) => m == pm && pl == branch.default.label,
                    _ => p.label == branch.default.label
                        || p.label.ends_with(&format!(".{}", branch.default.label)),
                }
            })
            .ok_or_else(|| anyhow::anyhow!(
                "branch `{}` default → unknown pipeline `{}`",
                branch.label, branch.default.label))?;
        if default_pipeline.steps.is_empty() {
            return Err(anyhow::anyhow!(
                "branch `{}` default pipeline `{}` has no steps",
                branch.label, default_pipeline.label));
        }
        let default_steps = qualify_pipeline_steps(default_pipeline);

        Ok(Self {
            branch_label: branch.label.clone(),
            classifier_agent_label,
            case_keys_and_steps,
            default_steps,
        })
    }
}

/// CLI-side classifier prompt (mirrors `patterns::router::
/// build_classifier_prompt` — duplicated here to avoid exposing that
/// private helper).  Asks the classifier agent to emit ONE of the
/// case keys for the supplied user input.
fn build_router_classifier_prompt(
    case_keys_and_steps: &[(String, Vec<String>)],
    user_input:          &str,
) -> String {
    let case_list = case_keys_and_steps.iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "You are a routing classifier.  Given the user input below, \
         reply with ONE of the case keys: {case_list}.  Output ONLY \
         the case key, no other text, no punctuation, no quotes.\n\
         \nUser input: {user_input}",
    )
}

/// Shared step-qualifier — folds a pipeline's `Vec<Ref>` to qualified
/// agent labels.  Used by both orchestrator-mode and router-mode
/// dispatch resolution; identical rules as `patterns::orchestrator::
/// qualified_step_labels` (kept here as a CLI helper to avoid making
/// that fn public).
fn qualify_pipeline_steps(
    pipeline: &aislang_core::ast::Pipeline,
) -> Vec<String> {
    let containing = pipeline.label.split_once('.').map(|(m, _)| m);
    pipeline.steps.iter().map(|r| match (&r.module, containing) {
        (Some(m), _)    => format!("{m}.{}", r.label),
        (None, Some(c)) => format!("{c}.{}", r.label),
        (None, None)    => r.label.clone(),
    }).collect()
}

/// `aislang skill run <label> <project> [--args '<json>']` —
/// v0.3 Week 18 Thu follow-up.  Resolves the project, finds the
/// skill block by label, dispatches to the appropriate runtime
/// entry point (`run_shell_skill` for `exec = "<cmd>"`,
/// `run_mcp_skill` for `mcp = mcp_server.<s>.tool.<t>`), prints
/// the typed output as text on stdout.
///
/// For MCP-delegate skills the referenced `mcp_server` block is
/// spawned via `docker run -i` (same StdioChild path agents use
/// for their `tools = [mcp_server.<x>]` list), the tool is
/// called with the supplied JSON args, and the child is shut
/// down cleanly before returning.  Requires Docker on PATH.
/// `aislang rag build <label> <project> [--litellm-url …]` —
/// v0.3 Week 19 Wed follow-up.  Chunks the vector_store's source
/// content, embeds every chunk via the referenced embedding
/// block (POSTs to LiteLLM at `--litellm-url`), persists to
/// `<project>/.aislang/vector/<label>.bin`.
/// v0.3 Week 19 Thu: auto-build RAG indexes at `aislang apply`
/// time.  Iterates every declared `vector_store` block + builds +
/// persists its index to `<project>/.aislang/vector/<label>.bin`.
/// Logs per-store progress to stderr; per-store failures DON'T
/// abort apply (stack is up either way + user can rebuild via
/// `aislang rag build <label> <project>` manually).
fn auto_build_rag_indexes(project: &Path) {
    // apply runs the local Compose stack → LiteLLM at localhost:4000.
    let _ = run_rag_build_all(project.to_path_buf(), "http://localhost:4000".to_string());
}

/// `aislang rag build-all <project> [--litellm-url …]` (W26) — build EVERY
/// declared vector_store's index, tolerating per-store failures (logged, not
/// fatal, so a stack still comes up).  Shared by `apply`'s auto-build + the
/// managed-cloud agent-runtime entrypoint (which builds `git:` knowledge
/// corpora in-pod before serving).  Indexes land per `vector_index_dir`
/// (honours `AISLANG_VECTOR_DIR`).
fn run_rag_build_all(project: PathBuf, litellm_url: String) -> Result<()> {
    let loaded = match load_project_input(&project) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("aislang rag build-all: load failed (skipping): {e}");
            return Ok(());
        }
    };
    let proj = &loaded.resolved.project;
    if proj.vector_stores.is_empty() {
        eprintln!("aislang rag build-all: no vector_store blocks — nothing to build");
        return Ok(());
    }
    eprintln!();
    eprintln!("building RAG indexes for {} declared vector_store(s) (litellm={litellm_url}):",
        proj.vector_stores.len());
    for vs in &proj.vector_stores {
        eprint!("  - `{}` … ", vs.label);
        match build_rag_index_for(&vs.label, &project, &litellm_url) {
            Ok((built, idx_path)) => match built.save(&idx_path) {
                Ok(bytes) => eprintln!("{} chunks ({} bytes) → {}",
                    built.chunks.len(), bytes, idx_path.display()),
                Err(e) => eprintln!("FAILED at save: {e}"),
            },
            Err(e) => eprintln!("FAILED at build: {e}"),
        }
    }
    Ok(())
}

fn run_rag_build(
    vs_label:    String,
    project:     PathBuf,
    litellm_url: String,
) -> Result<()> {
    let (built, idx_path) = build_rag_index_for(&vs_label, &project, &litellm_url)?;
    let written = built.save(&idx_path)
        .map_err(|e| anyhow::anyhow!("save index: {e}"))?;
    eprintln!("aislang rag: built `{vs_label}` index with {} chunks ({written} bytes) at {}",
        built.chunks.len(), idx_path.display());
    Ok(())
}

/// v0.4 W21 Tue — `aislang rag verify <project>` scan-all-stores
/// health check.
///
/// Walks every `vector_store` declared in the (merged, multi-
/// module-aware) project + reports the on-disk chunk count for
/// each.  Closes the W20 Thu dogfood-finding gap: the multi-
/// module orchestrator silently built EMPTY (50-byte / 0-chunk)
/// indexes for every sub-agent's corpus because the per-store
/// `source` was resolving against the wrong dir.  The Coder
/// hallucinated `hcl::Block` against an empty corpus index +
/// nothing surfaced the empty state until manual JSON-inspection
/// of the `.bin` files.
///
/// Exit semantics:
/// - **Exit 0** — every declared store has a healthy index
///   (>0 chunks).
/// - **Exit non-zero** — at least one store either has 0 chunks
///   OR its `.bin` file is missing from disk.  The
///   `aislang::v0_4::RagVerifyFailed` `anyhow` error contains
///   a per-store summary so the user sees which stores are
///   broken.
fn run_rag_verify(project: PathBuf, _strict: bool) -> Result<ExitCode> {
    let loaded = load_project_input(&project)?;
    let proj = &loaded.resolved.project;
    let project_root = std::fs::canonicalize(&loaded.project_root)
        .map_err(|e| anyhow::anyhow!("project root not readable: {e}"))?;

    if proj.vector_stores.is_empty() {
        eprintln!("aislang rag verify: no vector_store blocks declared in {} \
                   — nothing to verify",
            project.display());
        return Ok(ExitCode::SUCCESS);
    }

    eprintln!("aislang rag verify: scanning {} declared vector_store(s) in {}",
        proj.vector_stores.len(), project.display());

    let mut unhealthy = 0u32;
    let mut healthy   = 0u32;
    for vs in &proj.vector_stores {
        let idx_path = aislang_runtime::rag::vector_index_dir(&project_root)
            .join(format!("{}.bin", vs.label));
        let report = inspect_rag_index(&idx_path);
        match report {
            RagIndexReport::Healthy { chunks, bytes } => {
                eprintln!("  ✓ `{}`: {chunks} chunks ({bytes} bytes) — {}",
                    vs.label, idx_path.display());
                healthy += 1;
            }
            RagIndexReport::Empty { bytes } => {
                eprintln!("  ✗ `{}`: EMPTY (0 chunks, {bytes} bytes) — {}",
                    vs.label, idx_path.display());
                eprintln!("    hint: run `aislang apply {}` to rebuild, or",
                    project.display());
                eprintln!("          `aislang rag build {} {}` to build just this store",
                    vs.label, project.display());
                unhealthy += 1;
            }
            RagIndexReport::Missing => {
                eprintln!("  ✗ `{}`: MISSING — {}",
                    vs.label, idx_path.display());
                eprintln!("    hint: run `aislang apply {}` to build the indexes",
                    project.display());
                unhealthy += 1;
            }
            RagIndexReport::Corrupt { cause } => {
                eprintln!("  ✗ `{}`: CORRUPT — {}: {cause}",
                    vs.label, idx_path.display());
                unhealthy += 1;
            }
        }
    }

    eprintln!();
    if unhealthy == 0 {
        eprintln!("aislang rag verify: OK — {healthy} healthy store(s)");
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("aislang rag verify: FAILED — {healthy} healthy, {unhealthy} unhealthy");
        Ok(ExitCode::from(2))
    }
}

/// v0.4 W23 Mon follow-up — auto-load `.env.cloud` from the
/// nearest parent dir BEFORE any cloud subcommand reads env vars.
/// Prints a one-line banner so the operator sees which file (if
/// any) populated their env.  Process env vars set in the shell
/// keep precedence over file values (standard dotenv semantics).
fn load_cloud_dotenv_with_banner() {
    use aislang_cloud::env_template::{load_cloud_dotenv, DotenvLoadResult};
    match load_cloud_dotenv() {
        DotenvLoadResult::Loaded(path) => {
            eprintln!("aislang cloud: loaded env from {}",
                path.display());
        }
        DotenvLoadResult::NoFile => {
            // Quiet by default — operator may legitimately be
            // running via shell-exported vars (CI, `cargo test`,
            // production systemd unit).
        }
        DotenvLoadResult::LoadError(msg) => {
            // Non-fatal — env vars may still be set via shell.
            // Surface as a warning so operator can fix the file.
            eprintln!("aislang cloud: warning: dotenv load failed: {msg}");
        }
    }
}

/// v0.4 W22 Mon follow-up — `aislang cloud serve` thin wrapper
/// around `aislang_cloud::serve`.  Reads the bearer token from
/// `AISLANG_CLOUD_TOKEN`, binds a TcpListener on
/// `<host>:<port>`, calls `aislang_cloud::serve`.  Refuses to
/// start without a token (would otherwise accept every
/// unauthenticated request + leak the store).
fn run_cloud_serve(host: String, port: u16) -> Result<()> {
    load_cloud_dotenv_with_banner();
    let config = aislang_cloud::ServerConfig::from_env().ok_or_else(|| {
        anyhow::anyhow!(
            "AISLANG_CLOUD_TOKEN is unset or empty — refusing to start the \
             control plane without a bearer token (Decision 46.1).  Set it \
             before launching:\n  \
             AISLANG_CLOUD_TOKEN=<token> aislang cloud serve")
    })?;
    // v0.4 W22 Tue: AppState now carries `Arc<dyn Store>`; the
    // in-memory ProjectStore is the W22-Mon scaffold default.
    // PgStore swap lives behind `AISLANG_CLOUD_DB_URL` — when set,
    // the CLI binds the Postgres pool; absent → fall back to the
    // in-memory store so `aislang cloud serve` still works for
    // local-dev smoke tests without a Postgres.
    let db_url = std::env::var("AISLANG_CLOUD_DB_URL").ok()
        .filter(|s| !s.is_empty());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;

    rt.block_on(async move {
        let public_base_url = config.public_base_url.clone();
        let mut state = if let Some(url) = db_url {
            eprintln!("aislang cloud: connecting to Postgres at AISLANG_CLOUD_DB_URL");
            let pg = aislang_cloud::db::PgStore::connect(&url).await
                .map_err(|e| anyhow::anyhow!("PgStore connect: {e}"))?;
            // v0.4 W25 — reconcile every existing tenant schema to the
            // current per-tenant DDL on startup (idempotent ALTER ...
            // ADD COLUMN IF NOT EXISTS / CREATE TABLE IF NOT EXISTS), so
            // a schema change shipped in this build (e.g. the `repo`
            // column / `audit_log` table) backfills onto pre-existing
            // tenants WITHOUT a manual per-tenant re-provision.
            match pg.reconcile_all_tenant_schemas().await {
                Ok((ok, failed)) => eprintln!(
                    "aislang cloud: reconciled {ok} tenant schema(s) to current \
                     DDL ({failed} failed)"),
                Err(e) => eprintln!(
                    "aislang cloud: WARN tenant-schema reconcile skipped: {e}"),
            }
            aislang_cloud::AppState::new(config, std::sync::Arc::new(pg))
        } else {
            eprintln!("aislang cloud: AISLANG_CLOUD_DB_URL unset \
                       — using in-memory store (local dev only; \
                       no data persists across restarts)");
            aislang_cloud::AppState::with_in_memory_store(config)
        };

        // v0.4 W23 Tue (Decision 46.3) — spawn the metering
        // background flusher.  Only fires when STRIPE_SECRET_KEY
        // is present; otherwise the queue still accepts events
        // (so handlers don't need to branch) but they just sit
        // there until the operator wires Stripe up + restarts.
        match aislang_cloud::billing::StripeClient::from_env() {
            Ok(stripe) => {
                let interval = std::time::Duration::from_secs(
                    aislang_cloud::billing::DEFAULT_FLUSH_INTERVAL_SECS);
                state.meter.spawn_flusher(
                    stripe, state.store.clone(), interval);
                eprintln!("aislang cloud: metering pipeline enabled \
                           (flush every {}s)",
                    aislang_cloud::billing::DEFAULT_FLUSH_INTERVAL_SECS);
            }
            Err(_) => {
                eprintln!("aislang cloud: STRIPE_SECRET_KEY unset — \
                           metering pipeline DISABLED (events still \
                           enqueued + buffered, but never flushed to \
                           Stripe until you set STRIPE_SECRET_KEY + \
                           restart)");
            }
        }

        // v0.4 W23 Thu follow-up — swap in the REAL Stripe Checkout
        // Session creator when STRIPE_PRICE_ID is set; otherwise the
        // signup flow uses MockCheckoutCreator (whose checkout_url
        // is a non-payable mock that 404s on Stripe).
        match aislang_cloud::signup::StripeCheckoutCreator::from_env(&public_base_url) {
            Ok(Some(creator)) => {
                state.checkout_creator = std::sync::Arc::new(creator);
                eprintln!("aislang cloud: live Stripe Checkout enabled \
                           (STRIPE_PRICE_ID set) — signup checkout_url is \
                           a real payable session");
            }
            Ok(None) => {
                eprintln!("aislang cloud: STRIPE_PRICE_ID unset — signup \
                           uses MOCK checkout (checkout_url is NOT payable; \
                           set STRIPE_PRICE_ID to a recurring price to \
                           enable real Stripe Checkout)");
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Stripe Checkout misconfigured: {e}"));
            }
        }

        // v0.4 W24 Tue — swap in the REAL GitHub PR commenter when
        // GITHUB_TOKEN is set; otherwise the GitOps flow logs the
        // plan comment instead of posting it.
        match aislang_cloud::gitops::GithubPrCommenter::from_env() {
            Ok(Some(commenter)) => {
                state.pr_commenter = std::sync::Arc::new(commenter);
                eprintln!("aislang cloud: GitOps PR comments ENABLED \
                           (GITHUB_TOKEN set) — plan results post to PRs");
            }
            Ok(None) => {
                eprintln!("aislang cloud: GITHUB_TOKEN unset — GitOps plan \
                           comments are LOGGED only (not posted to GitHub; \
                           set GITHUB_TOKEN to a PAT/installation token to \
                           enable PR comments)");
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "GitHub PR commenter misconfigured: {e}"));
            }
        }

        // v0.4 W25 Tue follow-up — swap in the REAL AWS SES email
        // sender when AISLANG_EMAIL_FROM is set; otherwise signup
        // verify-emails are LOGGED only (LoggingEmailSender default).
        match aislang_cloud::signup::SesEmailSender::from_env() {
            Ok(Some(sender)) => {
                state.email_sender = std::sync::Arc::new(sender);
                eprintln!("aislang cloud: live email ENABLED via AWS SES \
                           (AISLANG_EMAIL_FROM set) — signup verify links \
                           are emailed to users");
            }
            Ok(None) => {
                eprintln!("aislang cloud: AISLANG_EMAIL_FROM unset — signup \
                           verify emails are LOGGED only (not sent; set \
                           AISLANG_EMAIL_FROM + AISLANG_SES_REGION + \
                           AISLANG_SES_SMTP_USER + AISLANG_SES_SMTP_PASSWORD \
                           to enable AWS SES)");
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "SES email sender misconfigured: {e}"));
            }
        }

        let addr = format!("{host}:{port}");
        let listener = tokio::net::TcpListener::bind(&addr).await
            .map_err(|e| anyhow::anyhow!("bind {addr}: {e}"))?;
        eprintln!("aislang cloud: listening on http://{addr}");
        eprintln!("aislang cloud: bearer auth enabled (AISLANG_CLOUD_TOKEN)");
        eprintln!("aislang cloud: smoke test:");
        eprintln!("  curl -H 'Authorization: Bearer <token>' \\");
        eprintln!("       -H 'X-Aislang-Tenant: alpha' \\");
        eprintln!("       http://{addr}/v1/projects");
        aislang_cloud::serve(listener, state).await
            .map_err(|e| anyhow::anyhow!("axum serve: {e}"))
    })?;
    Ok(())
}

/// v0.4 W22 Tue follow-up — `aislang cloud provision <tenant>`.
///
/// Closes the UX gap surfaced by the live-Postgres e2e smoke: hitting
/// `GET /v1/projects` against an un-provisioned tenant returned 500
/// (`relation "projects" does not exist`) because the per-tenant
/// schema hadn't been created.  This subcommand calls
/// `PgStore::ensure_tenant_schema` to create the schema + replay the
/// per-tenant migrations, so the next API call against that tenant
/// hits a populated schema.
///
/// Idempotent — running it twice against the same tenant is a
/// no-op (Postgres `CREATE SCHEMA IF NOT EXISTS` + `INSERT ... ON
/// CONFLICT DO NOTHING` on the tenants registry).
///
/// Requires `AISLANG_CLOUD_DB_URL`.  The in-memory fallback has no
/// schema to provision; running against it would silently succeed
/// without doing useful work, so we refuse explicitly instead.
fn run_cloud_provision(tenant: String) -> Result<()> {
    load_cloud_dotenv_with_banner();
    let db_url = std::env::var("AISLANG_CLOUD_DB_URL").ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!(
            "AISLANG_CLOUD_DB_URL is unset or empty — `aislang cloud \
             provision` only makes sense against Postgres.  Set it before \
             running:\n  \
             AISLANG_CLOUD_DB_URL=postgres://... aislang cloud provision <tenant>"))?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;

    // Validate the slug client-side BEFORE opening the Postgres
    // connection so a clear "malformed slug" error doesn't get
    // preceded by misleading "provisioning schema `t_BAD;DROP`"
    // banner output.  Mirrors the `public.tenants.slug` CHECK
    // constraint at the DB layer (belt-and-suspenders).
    if !aislang_cloud::db::is_valid_slug(&tenant) {
        anyhow::bail!(
            "tenant slug `{tenant}` is malformed — must match \
             `^[a-z0-9][a-z0-9-]{{0,63}}$` (lowercase alnum + hyphens, \
             starts with alnum, ≤64 chars)");
    }

    rt.block_on(async move {
        use aislang_cloud::Store;
        eprintln!("aislang cloud: connecting to Postgres at AISLANG_CLOUD_DB_URL");
        let pg = aislang_cloud::db::PgStore::connect(&db_url).await
            .map_err(|e| anyhow::anyhow!("PgStore connect: {e}"))?;
        eprintln!("aislang cloud: provisioning tenant `{tenant}` \
                   (schema `t_{tenant}` + per-tenant migrations)");
        pg.ensure_tenant_schema(&tenant).await
            .map_err(|e| anyhow::anyhow!("ensure_tenant_schema: {e}"))?;
        eprintln!("aislang cloud: tenant `{tenant}` provisioned ✓");
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

/// v0.4 W25 — `aislang cloud deprovision <tenant>`.  The inverse of
/// `provision`: drops the tenant schema + deletes all of its
/// shared-schema rows.  Destructive + irreversible.  Idempotent — a
/// no-op (with a clear message) when the tenant doesn't exist.
fn run_cloud_deprovision(tenant: String) -> Result<()> {
    load_cloud_dotenv_with_banner();
    let db_url = std::env::var("AISLANG_CLOUD_DB_URL").ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!(
            "AISLANG_CLOUD_DB_URL is unset or empty — `aislang cloud \
             deprovision` only makes sense against Postgres.  Set it before \
             running:\n  \
             AISLANG_CLOUD_DB_URL=postgres://... aislang cloud deprovision <tenant>"))?;

    if !aislang_cloud::db::is_valid_slug(&tenant) {
        anyhow::bail!(
            "tenant slug `{tenant}` is malformed — must match \
             `^[a-z0-9][a-z0-9-]{{0,63}}$`");
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;

    rt.block_on(async move {
        use aislang_cloud::Store;
        let pg = aislang_cloud::db::PgStore::connect(&db_url).await
            .map_err(|e| anyhow::anyhow!("PgStore connect: {e}"))?;
        eprintln!("aislang cloud: deprovisioning tenant `{tenant}` \
                   (DROP SCHEMA t_{tenant} CASCADE + delete shared rows)");
        let existed = pg.deprovision_tenant(&tenant).await
            .map_err(|e| anyhow::anyhow!("deprovision_tenant: {e}"))?;
        if existed {
            eprintln!("aislang cloud: tenant `{tenant}` deprovisioned ✓");
        } else {
            eprintln!("aislang cloud: tenant `{tenant}` did not exist — nothing \
                       to do");
        }
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

/// v0.4 W23 Mon — `aislang cloud meter-event` Stripe metering spike.
///
/// Submits one meter event to the Stripe Billing meter-events API
/// + prints the server-echoed response.  Demonstrates the full
/// wire path the Phase 2 Tue automated emission will use (token
/// + SP-equivalent meter events fire on every chat-turn + every
/// `apply`/`destroy`/`plan`).
///
/// Reads `STRIPE_SECRET_KEY` (required) + optionally
/// `STRIPE_API_BASE` (defaults to `https://api.stripe.com`) from
/// env.  Refuses to start without a secret key — mirrors the
/// `AISLANG_CLOUD_TOKEN` empty-check posture from W22 Mon.
fn run_cloud_meter_event(
    customer:   String,
    kind:       String,
    quantity:   u64,
    identifier: Option<String>,
) -> Result<()> {
    load_cloud_dotenv_with_banner();
    use aislang_cloud::billing::{
        MeterEvent, MeterEventKind, StripeClient, generate_smoke_identifier,
        PINNED_API_VERSION,
    };

    let kind = MeterEventKind::parse_cli_value(&kind)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let identifier = identifier.unwrap_or_else(generate_smoke_identifier);
    let event = MeterEvent {
        kind,
        stripe_customer_id: customer.clone(),
        quantity,
        identifier: identifier.clone(),
        timestamp_secs: None,
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;

    rt.block_on(async move {
        eprintln!("aislang cloud: building Stripe client (API version: {PINNED_API_VERSION})");
        let client = StripeClient::from_env()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        eprintln!("aislang cloud: submitting meter event \
                   customer={customer} kind={} quantity={quantity} identifier={identifier}",
            kind.as_event_name());
        let resp = client.submit_meter_event(&event).await
            .map_err(|e| anyhow::anyhow!("submit_meter_event: {e}"))?;
        eprintln!("aislang cloud: Stripe accepted the event ✓");
        eprintln!("  event_name = {}", resp.event_name);
        eprintln!("  identifier = {}", resp.identifier);
        if let Some(ts) = resp.timestamp {
            eprintln!("  timestamp  = {ts}");
        }
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

/// v0.4 W23 Mon follow-up — `aislang cloud init`.  Writes
/// `.env.cloud.example` to cwd listing every operator env var the
/// `aislang-cloud` crate reads (see
/// `aislang_cloud::env_template::CLOUD_ENV_VARS` for the
/// canonical registry).
///
/// Refuses to overwrite an existing file unless `--force` is
/// passed, so we don't stomp operator edits when re-run during
/// an upgrade.
fn run_cloud_init(force: bool) -> Result<()> {
    use aislang_cloud::env_template::{
        generate_cloud_env_example, CLOUD_ENV_EXAMPLE_FILENAME,
    };
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("cwd: {e}"))?;
    let path = cwd.join(CLOUD_ENV_EXAMPLE_FILENAME);
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists — pass --force to overwrite",
            path.display());
    }
    std::fs::write(&path, generate_cloud_env_example())
        .map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
    eprintln!("aislang cloud: wrote {} ✓", path.display());
    eprintln!("  next steps:");
    eprintln!("    cp {} .env.cloud", path.display());
    eprintln!("    # edit .env.cloud + fill in AISLANG_CLOUD_TOKEN etc.");
    eprintln!("    echo .env.cloud >> .gitignore");
    eprintln!("    aislang cloud serve   # auto-loads .env.cloud");
    Ok(())
}

/// v0.4 W23 Tue — `aislang cloud set-customer <tenant>
/// <stripe_customer_id>`.
///
/// Maps a provisioned tenant slug to its Stripe customer record
/// so the metering pipeline (Decision 46.3) can submit meter
/// events against the right billable account.  Requires
/// `AISLANG_CLOUD_DB_URL` — the in-memory fallback store doesn't
/// persist across restarts so a mapping set there would
/// disappear when `aislang cloud serve` cycles.
///
/// Customer-id validation:
/// - Client-side: `^cus_[A-Za-z0-9]{1,60}$` regex (matches
///   Stripe's actual format + the DB CHECK constraint).
/// - DB-side: same regex enforced at insert.
/// - Failure mode: malformed input fails fast BEFORE opening
///   the Postgres connection (clear error, no transient DB
///   noise in logs).
fn run_cloud_set_customer(
    tenant:             String,
    stripe_customer_id: String,
) -> Result<()> {
    load_cloud_dotenv_with_banner();
    // Client-side slug + customer-id validation.  Slug
    // validation is reused from db.rs; the customer-id regex
    // is duplicated here so the error fires before we open a
    // Postgres connection.
    if !aislang_cloud::db::is_valid_slug(&tenant) {
        anyhow::bail!("tenant slug `{tenant}` is malformed — must match \
                       `^[a-z0-9][a-z0-9-]{{0,63}}$`");
    }
    if !is_valid_stripe_customer_id(&stripe_customer_id) {
        anyhow::bail!("stripe customer id `{stripe_customer_id}` is malformed \
                       — must match `^cus_[A-Za-z0-9]{{1,60}}$`");
    }

    let db_url = std::env::var("AISLANG_CLOUD_DB_URL").ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!(
            "AISLANG_CLOUD_DB_URL is unset or empty — `aislang cloud \
             set-customer` only makes sense against Postgres (the \
             in-memory fallback doesn't persist).  Set it before \
             running:\n  \
             AISLANG_CLOUD_DB_URL=postgres://... aislang cloud \
             set-customer <tenant> <cus_id>"))?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;

    rt.block_on(async move {
        use aislang_cloud::Store;
        eprintln!("aislang cloud: connecting to Postgres at AISLANG_CLOUD_DB_URL");
        let pg = aislang_cloud::db::PgStore::connect(&db_url).await
            .map_err(|e| anyhow::anyhow!("PgStore connect: {e}"))?;
        eprintln!("aislang cloud: mapping tenant `{tenant}` → \
                   stripe_customer_id `{stripe_customer_id}`");
        pg.set_tenant_stripe_customer(&tenant, &stripe_customer_id).await
            .map_err(|e| anyhow::anyhow!("set_tenant_stripe_customer: {e}"))?;
        eprintln!("aislang cloud: customer mapping persisted ✓");
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

/// Client-side Stripe customer-id validator — mirrors the
/// `tenants_stripe_customer_id_format` CHECK constraint defined
/// in `migrations/20260609000000_tenants_stripe_customer.sql`.
/// Belt-and-suspenders with the DB constraint so malformed ids
/// fail fast with a clear error before any SQL fires.
fn is_valid_stripe_customer_id(s: &str) -> bool {
    if !s.starts_with("cus_") { return false; }
    let rest = &s[4..];
    if rest.is_empty() || rest.len() > 60 { return false; }
    rest.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// v0.4 W23 Wed — `aislang cloud set-plan <tenant> <free|paid>`.
///
/// Sets a tenant's billing plan (Decision 46.3 free-tier rate-
/// limiting).  `free` rate-limits SP-equivalent operations at
/// 50/day; `paid` is unlimited.  Requires `AISLANG_CLOUD_DB_URL`
/// — the plan must persist across `aislang cloud serve` restarts
/// (the in-process rate-limit COUNTER resets on restart, but the
/// PLAN config is durable).
fn run_cloud_set_plan(tenant: String, plan: String) -> Result<()> {
    load_cloud_dotenv_with_banner();
    if !aislang_cloud::db::is_valid_slug(&tenant) {
        anyhow::bail!("tenant slug `{tenant}` is malformed — must match \
                       `^[a-z0-9][a-z0-9-]{{0,63}}$`");
    }
    let parsed_plan = aislang_cloud::rate_limit::TenantPlan::parse_cli_value(&plan)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let db_url = std::env::var("AISLANG_CLOUD_DB_URL").ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!(
            "AISLANG_CLOUD_DB_URL is unset or empty — `aislang cloud \
             set-plan` only makes sense against Postgres (the in-memory \
             fallback doesn't persist).  Set it before running:\n  \
             AISLANG_CLOUD_DB_URL=postgres://... aislang cloud set-plan \
             <tenant> <free|paid>"))?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;

    rt.block_on(async move {
        use aislang_cloud::Store;
        eprintln!("aislang cloud: connecting to Postgres at AISLANG_CLOUD_DB_URL");
        let pg = aislang_cloud::db::PgStore::connect(&db_url).await
            .map_err(|e| anyhow::anyhow!("PgStore connect: {e}"))?;
        eprintln!("aislang cloud: setting tenant `{tenant}` plan → `{}`",
            parsed_plan.as_str());
        pg.set_tenant_plan(&tenant, parsed_plan).await
            .map_err(|e| anyhow::anyhow!("set_tenant_plan: {e}"))?;
        eprintln!("aislang cloud: plan persisted ✓");
        if parsed_plan == aislang_cloud::rate_limit::TenantPlan::Free {
            eprintln!("  note: free tier rate-limited at {} SP-equivalents/day",
                aislang_cloud::rate_limit::FREE_TIER_DAILY_SP_CAP);
        }
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

/// v0.4 W25 Mon — `aislang remote --target` (Decision 46.9, mission
/// deliverable #2).  Resolve the bearer token + tenant, build a
/// [`aislang_cloud::remote::RemoteClient`], and delegate the chosen
/// verb to the control plane.  Returns the process exit code:
///   - validate: 0 valid / 2 invalid source / (errors propagate)
///   - plan / status: 0 on success
///   - apply / destroy: 0 on a `done` event, 1 on an `error` event
///     or an incomplete stream
fn run_remote(
    target: String,
    tenant: Option<String>,
    token:  Option<String>,
    action: RemoteCommand,
) -> Result<ExitCode> {
    // `--target` is the control-plane BASE URL (where `aislang cloud
    // serve` is reachable) — e.g. `https://api.aislang.cloud` or a
    // port-forward `http://127.0.0.1:4001`.  A common mix-up is passing
    // the executor mode (`kubernetes`) here; catch that with a clear
    // message rather than reqwest's cryptic "relative URL without a base".
    if !target.starts_with("http://") && !target.starts_with("https://") {
        anyhow::bail!(
            "--target must be the control plane's URL (e.g. \
             https://api.aislang.cloud, or a port-forward \
             http://127.0.0.1:4001), not `{target}`.\n\
             (The executor mode `kubernetes` is a SERVER-side setting — \
             you set it on the control plane via the Helm chart's \
             `executor` value, not on `aislang remote`.)");
    }

    // Auto-load `.env.cloud` so `AISLANG_CLOUD_TOKEN` / `AISLANG_TENANT`
    // resolve the same way the `aislang cloud serve` operator surface
    // already does.
    load_cloud_dotenv_with_banner();

    let token = token
        .or_else(|| std::env::var("AISLANG_CLOUD_TOKEN").ok())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!(
            "no bearer token — pass --token or set AISLANG_CLOUD_TOKEN \
             (e.g. in .env.cloud).  This is the same token the control \
             plane was started with."))?;
    let tenant = tenant
        .or_else(|| std::env::var("AISLANG_TENANT").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".to_string());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;

    rt.block_on(async move {
        use aislang_cloud::remote::{RemoteClient, RemoteError, SseTerminal};
        let client = RemoteClient::new(&target, &token, &tenant);
        eprintln!("aislang remote: target={target} tenant={tenant}");

        // Print one SSE event as a progress line on stderr (results
        // and artifacts go to stdout; progress narration to stderr so
        // pipelines can capture clean output).
        let print_event = |name: &str, data: &serde_json::Value| {
            match name {
                "progress" => eprintln!("  · {}: {}",
                    data["phase"].as_str().unwrap_or("?"),
                    data["detail"].as_str().unwrap_or("")),
                "done" => eprintln!("  ✓ done"),
                "error" => eprintln!("  ✗ error in {}: {}",
                    data["phase"].as_str().unwrap_or("?"),
                    data["error"].as_str().unwrap_or("")),
                _ => {}
            }
        };

        // Map a streaming outcome → exit code + a billing summary line.
        let summarise_stream = |verb: &str, outcome: aislang_cloud::remote::SseOutcome| {
            match outcome.terminal {
                SseTerminal::Done(d) => {
                    if let Some(q) = d["billing"]["quantity"].as_u64() {
                        eprintln!("aislang remote: {verb} ok — billed {q} \
                                   SP-equivalent");
                    }
                    ExitCode::SUCCESS
                }
                SseTerminal::Error(_) => {
                    eprintln!("aislang remote: {verb} failed (see error above)");
                    ExitCode::from(1)
                }
                SseTerminal::Incomplete => {
                    eprintln!("aislang remote: {verb} stream ended without a \
                               terminal event — treating as failure");
                    ExitCode::from(1)
                }
            }
        };

        match action {
            RemoteCommand::Status { id } => {
                let row = client.status(&id).await?;
                println!("{}", serde_json::to_string_pretty(&row)?);
                Ok(ExitCode::SUCCESS)
            }
            RemoteCommand::Validate { id } => {
                match client.validate(&id).await {
                    Ok(v) => {
                        eprintln!("aislang remote: ✓ valid — project `{}`",
                            v["project_name"].as_str().unwrap_or("?"));
                        Ok(ExitCode::SUCCESS)
                    }
                    // 422 = the source failed to load.  Surface it as a
                    // clean non-zero exit (2, mirroring local validate)
                    // rather than an anyhow error trace.
                    Err(RemoteError::Status { status: 422, body }) => {
                        eprintln!("aislang remote: ✗ invalid — {body}");
                        Ok(ExitCode::from(2))
                    }
                    Err(e) => Err(e.into()),
                }
            }
            RemoteCommand::Plan { id } => {
                let v = client.plan(&id).await?;
                if let Some(compose) = v["artifacts"]["docker_compose"].as_str() {
                    // Compose YAML → stdout (the actual plan output).
                    println!("{compose}");
                }
                eprintln!("aislang remote: ✓ planned `{}` — billed 1 \
                           SP-equivalent",
                    v["project_name"].as_str().unwrap_or("?"));
                Ok(ExitCode::SUCCESS)
            }
            RemoteCommand::Apply { id } => {
                let outcome = client.apply(&id, print_event).await?;
                Ok(summarise_stream("apply", outcome))
            }
            RemoteCommand::Destroy { id } => {
                let outcome = client.destroy(&id, print_event).await?;
                Ok(summarise_stream("destroy", outcome))
            }
        }
    })
}

/// Per-store health classification used by `aislang rag verify`
/// + by the chat-time empty-RAG warning at `aislang chat`
/// session start.  Single source of truth so both surfaces use
/// the same definitions.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RagIndexReport {
    Healthy { chunks: usize, bytes: u64 },
    Empty   { bytes: u64 },
    Missing,
    Corrupt { cause: String },
}

/// Inspect a single on-disk RAG index file + classify it.  Pure
/// function over the filesystem (no network, no embedder
/// instantiation) so it's cheap enough to call at every
/// `aislang chat` session start without delaying the chat loop.
pub(crate) fn inspect_rag_index(idx_path: &Path) -> RagIndexReport {
    if !idx_path.exists() {
        return RagIndexReport::Missing;
    }
    let bytes = std::fs::metadata(idx_path)
        .map(|m| m.len()).unwrap_or(0);
    match aislang_runtime::rag::Index::load(idx_path) {
        Ok(idx) => {
            let n = idx.chunks.len();
            if n == 0 {
                RagIndexReport::Empty { bytes }
            } else {
                RagIndexReport::Healthy { chunks: n, bytes }
            }
        }
        Err(e) => RagIndexReport::Corrupt { cause: format!("{e:?}") },
    }
}

/// `aislang rag query <label> <project> "<text>" [--top-k N] [--litellm-url …]` —
/// v0.3 Week 19 Wed follow-up.  Loads the cached index (building
/// it on-demand if missing), embeds the query text via the same
/// embedding model the index was built with, returns the top-K
/// most-similar chunks ordered by cosine score.
fn run_rag_query(
    vs_label:    String,
    project:     PathBuf,
    query:       String,
    top_k:       usize,
    litellm_url: String,
) -> Result<()> {
    use aislang_runtime::rag::{Index, LiteLlmEmbedder, Embedder};

    let loaded = load_project_input(&project)?;
    let proj = &loaded.resolved.project;
    let vs = proj.vector_stores.iter()
        .find(|v| v.label == vs_label)
        .ok_or_else(|| anyhow::anyhow!(
            "no vector_store named `{vs_label}` in {}", project.display()))?;

    let project_root = std::fs::canonicalize(&loaded.project_root)
        .map_err(|e| anyhow::anyhow!("project root not readable: {e}"))?;
    let idx_path = aislang_runtime::rag::vector_index_dir(&project_root)
        .join(format!("{vs_label}.bin"));

    // Build on first call if missing — same posture as
    // `aislang skill run` (no separate "prepare" step needed for
    // the demo path).
    let index = if idx_path.exists() {
        eprintln!("aislang rag: using cached index at {}", idx_path.display());
        Index::load(&idx_path)
            .map_err(|e| anyhow::anyhow!("load index: {e}"))?
    } else {
        eprintln!("aislang rag: index missing — building now");
        let (built, _) = build_rag_index_for(&vs_label, &project, &litellm_url)?;
        built.save(&idx_path)
            .map_err(|e| anyhow::anyhow!("save index: {e}"))?;
        built
    };

    // Look up the embedding-block referenced by the vector_store
    // so we can stand up a LiteLlmEmbedder with matching dims.
    // Multi-module discipline: `vs.embedding` is a parser-produced
    // ref (cross-module or same-module-bare); resolve to the merged-
    // project lookup key via Ref::qualified_in against vs.label.
    let emb_key = vs.embedding.qualified_in(&vs.label);
    let emb_block = proj.embeddings.iter()
        .find(|e| e.label == emb_key)
        .ok_or_else(|| anyhow::anyhow!(
            "vector_store `{vs_label}` references embedding `{emb_key}` \
             which doesn't exist in the project"))?;
    let catalog = aislang_catalog::bundled()
        .map_err(|e| anyhow::anyhow!("load catalog: {e}"))?;
    let catalog_entry = catalog
        .find_embedding(&emb_block.provider, &emb_block.model)
        .ok_or_else(|| anyhow::anyhow!(
            "embedding `{}` ({} / {}) not in bundled catalog",
            emb_block.label, emb_block.provider, emb_block.model))?;
    let embedder = LiteLlmEmbedder::new(
        &litellm_url, &emb_block.label,
        catalog_entry.dimensions as usize);

    let query_vec = embedder.embed(&query)
        .map_err(|e| anyhow::anyhow!("embed query: {e}"))?;
    let results = index.query(&query_vec, top_k)
        .map_err(|e| anyhow::anyhow!("query: {e}"))?;

    eprintln!("aislang rag: top {} results for `{}`:", results.len(), query);
    for (i, (text, score)) in results.iter().enumerate() {
        // Stdout: machine-readable shape `[<idx>] <score> | <text>`.
        let preview = if text.len() > 200 { &text[..200] } else { text.as_str() };
        println!("[{i}] score={:.4} | {}", score,
            preview.replace('\n', " ⏎ "));
    }
    Ok(())
}

/// Shared helper: resolve project → find vector_store →
/// read+chunk source → embed via LiteLLM → return the built
/// `Index` + its on-disk path.  Used by both `rag build` (which
/// just saves) and `rag query`'s on-first-call build path.
fn build_rag_index_for(
    vs_label:    &str,
    project:     &Path,
    litellm_url: &str,
) -> Result<(aislang_runtime::rag::Index, PathBuf)> {
    use aislang_core::ast::SourceUri;
    use aislang_runtime::rag::{build_index, LiteLlmEmbedder};

    let loaded = load_project_input(project)?;
    let proj = &loaded.resolved.project;
    let vs = proj.vector_stores.iter()
        .find(|v| v.label == vs_label)
        .ok_or_else(|| anyhow::anyhow!(
            "no vector_store named `{vs_label}` in {}", project.display()))?;
    let project_root = std::fs::canonicalize(&loaded.project_root)
        .map_err(|e| anyhow::anyhow!("project root not readable: {e}"))?;
    // v0.3 Week 20 Thu (dogfood finding): multi-module projects
    // resolve a vector_store's relative `source` path against the
    // CONTAINING MODULE's dir, NOT the project root.  The W19 Wed
    // wiring used `project_root.join(path)` which works for
    // single-file projects (where module_dir == project_root) but
    // silently produced empty indexes for multi-module orchestrator
    // sub-modules — discovered when the dogfood Coder hallucinated
    // `hcl::Block` against an EMPTY (50-byte / 0-chunk) corpus
    // index instead of the W20 Wed rewritten 217-KB chunked corpus.
    // `vs_label` is module-qualified (e.g. `coder.coder_kb`); split
    // on the first `.` to recover the module name + look up its
    // canonical dir via `LoadedProject.module_dirs`.
    let resolve_root: PathBuf = match vs_label.split_once('.') {
        Some((module, _)) => loaded.module_dirs.get(module)
            .cloned()
            .unwrap_or_else(|| project_root.clone()),
        None => project_root.clone(),
    };

    // v0.3: only the Path source variant is wired in the CLI build.
    // Url / Crawl variants would need an HTTP fetcher (+ for crawl
    // a recursive walker); both deferred to v0.5+.
    let source_text = match &vs.source {
        SourceUri::Path { path } => {
            let abs = if path.is_absolute() { path.clone() }
                      else { resolve_root.join(path) };
            // Single file → read directly; directory → concatenate
            // every readable file under it (lexicographic order)
            // with a separator marker so chunks don't span file
            // boundaries opaquely.
            if abs.is_file() {
                std::fs::read_to_string(&abs)
                    .map_err(|e| anyhow::anyhow!("read {}: {e}", abs.display()))?
            } else if abs.is_dir() {
                let mut entries: Vec<_> = std::fs::read_dir(&abs)
                    .map_err(|e| anyhow::anyhow!("read dir {}: {e}", abs.display()))?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.is_file())
                    .collect();
                entries.sort();
                let mut combined = String::new();
                for entry in entries {
                    if let Ok(body) = std::fs::read_to_string(&entry) {
                        if !combined.is_empty() {
                            combined.push_str(&format!("\n\n--- {} ---\n\n",
                                entry.file_name().unwrap_or_default().to_string_lossy()));
                        }
                        combined.push_str(&body);
                    }
                }
                combined
            } else {
                return Err(anyhow::anyhow!(
                    "vector_store `{vs_label}` source `{}` is neither file nor directory",
                    abs.display()));
            }
        }
        SourceUri::Git { repo, path } => {
            // W26 — clone an external GitHub repo (shallow, default branch)
            // into a temp dir, then recursively concatenate the readable text
            // files under the optional `#subpath`.  Private repos auth via
            // GITHUB_TOKEN (same scheme as the cloud's project clone).  In the
            // managed cloud this runs IN the agent-runtime pod (which has git +
            // the token), building the index into the pod's vector dir.
            let tmp = RagTmpDir::new("aislang-rag")
                .map_err(|e| anyhow::anyhow!("temp dir for `git:{repo}`: {e}"))?;
            clone_github_repo_shallow(repo, &tmp.0)
                .map_err(|e| anyhow::anyhow!("clone `git:{repo}`: {e}"))?;
            let root = match path {
                Some(p) => tmp.0.join(p),
                None    => tmp.0.clone(),
            };
            if !root.exists() {
                return Err(anyhow::anyhow!(
                    "vector_store `{vs_label}` source `git:{repo}#{}` not found \
                     in the cloned repo", path.as_deref().unwrap_or("")));
            }
            read_source_tree(&root)
                .map_err(|e| anyhow::anyhow!("read `git:{repo}` source: {e}"))?
        }
        SourceUri::Url { .. } | SourceUri::Crawl { .. } => {
            return Err(anyhow::anyhow!(
                "`rag build` supports `Path` + `git:owner/repo` source URIs; \
                 url/crawl variants need an HTTP fetcher (deferred to v0.5+)"));
        }
    };

    let emb_key = vs.embedding.qualified_in(&vs.label);
    let emb_block = proj.embeddings.iter()
        .find(|e| e.label == emb_key)
        .ok_or_else(|| anyhow::anyhow!(
            "vector_store `{vs_label}` references embedding `{emb_key}` \
             which doesn't exist in the project"))?;
    let catalog = aislang_catalog::bundled()
        .map_err(|e| anyhow::anyhow!("load catalog: {e}"))?;
    let catalog_entry = catalog
        .find_embedding(&emb_block.provider, &emb_block.model)
        .ok_or_else(|| anyhow::anyhow!(
            "embedding `{}` ({} / {}) not in bundled catalog",
            emb_block.label, emb_block.provider, emb_block.model))?;
    let embedder = LiteLlmEmbedder::new(
        litellm_url, &emb_block.label,
        catalog_entry.dimensions as usize);

    eprintln!("aislang rag: building `{vs_label}` index ({} chars source, \
               chunk_size={} chunk_overlap={}, embedder=`{}` dims={})",
        source_text.len(), vs.chunk_size.units, vs.chunk_overlap.units,
        emb_block.label, catalog_entry.dimensions);

    let index = build_index(
        vs_label.to_string(),
        &source_text,
        vs.chunk_size.units    as usize,
        vs.chunk_overlap.units as usize,
        &embedder,
    ).map_err(|e| anyhow::anyhow!("build index: {e}"))?;

    let idx_path = aislang_runtime::rag::vector_index_dir(&project_root)
        .join(format!("{vs_label}.bin"));
    Ok((index, idx_path))
}

// ── W26: `git:` vector_store source helpers ──────────────────────────────

/// RAII temp directory — removed on drop.  Holds a shallow repo clone for
/// the duration of a `git:` index build.
struct RagTmpDir(PathBuf);
impl RagTmpDir {
    fn new(prefix: &str) -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos()).unwrap_or(0);
        let p = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p)?;
        Ok(RagTmpDir(p))
    }
}
impl Drop for RagTmpDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

/// Shallow-clone a GitHub `owner/repo` into `dest` (default branch).  When
/// `GITHUB_TOKEN` is set it authenticates a private clone; the token is
/// redacted from any error surfaced.
fn clone_github_repo_shallow(repo: &str, dest: &Path) -> Result<()> {
    let token = std::env::var("GITHUB_TOKEN").ok()
        .map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let url = match &token {
        Some(t) => format!("https://x-access-token:{t}@github.com/{repo}.git"),
        None    => format!("https://github.com/{repo}.git"),
    };
    let out = std::process::Command::new("git")
        .args(["clone", "--depth", "1", "--quiet", &url])
        .arg(dest)
        .output()
        .map_err(|e| anyhow::anyhow!("run `git clone` (is git installed?): {e}"))?;
    if !out.status.success() {
        let mut err = String::from_utf8_lossy(&out.stderr).to_string();
        if let Some(t) = &token { err = err.replace(t, "***"); }
        return Err(anyhow::anyhow!("git clone `{repo}` failed: {}", err.trim()));
    }
    Ok(())
}

/// Per-file size cap when reading a `git:` source tree — skip anything
/// bigger so a stray lockfile/binary doesn't dominate the corpus.
const RAG_MAX_FILE_BYTES: u64 = 256 * 1024;

/// Read a single file, or recursively concatenate every readable UTF-8 text
/// file under a directory (skipping `.git` / build / dependency dirs and
/// binary/oversize files), each prefixed with a repo-relative separator.
fn read_source_tree(root: &Path) -> Result<String> {
    if root.is_file() {
        return std::fs::read_to_string(root)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", root.display()));
    }
    let mut files = Vec::new();
    collect_text_files(root, &mut files);
    files.sort();
    let mut combined = String::new();
    for f in &files {
        let Ok(body) = std::fs::read_to_string(f) else { continue }; // skip binary
        let rel = f.strip_prefix(root).unwrap_or(f);
        if !combined.is_empty() { combined.push_str("\n\n"); }
        combined.push_str(&format!("--- {} ---\n\n", rel.display()));
        combined.push_str(&body);
    }
    Ok(combined)
}

fn collect_text_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            if matches!(name.as_ref(),
                ".git" | "target" | "node_modules" | ".aislang" | "dist" | "build") {
                continue;
            }
            collect_text_files(&p, out);
        } else if p.is_file()
            && entry.metadata().map(|m| m.len()).unwrap_or(u64::MAX) <= RAG_MAX_FILE_BYTES
        {
            out.push(p);
        }
    }
}

fn run_skill(
    skill_label: String,
    project:     PathBuf,
    args_json:   String,
) -> Result<()> {
    use aislang_core::ast::SkillExec;
    use aislang_runtime::skills::{run_shell_skill, run_mcp_skill, SkillOutput};

    // 1. Load + parse the project (same dir-vs-file dispatch as chat).
    let loaded = load_project_input(&project)?;
    let proj = &loaded.resolved.project;

    // 2. Find the skill block by label.
    let skill = proj.skills.iter()
        .find(|s| s.label == skill_label)
        .ok_or_else(|| {
            let available: Vec<&str> = proj.skills.iter()
                .map(|s| s.label.as_str()).collect();
            let avail_line = if available.is_empty() {
                "  (no skill blocks declared in this project)".to_string()
            } else {
                format!("  available skills: {}", available.join(", "))
            };
            anyhow::anyhow!(
                "no skill named `{skill_label}` in {}\n{avail_line}",
                project.display())
        })?;

    // 3. Project root for the per-skill cwd (`<root>/.aislang/
    //    skills/<label>/`).
    let project_root = std::fs::canonicalize(&loaded.project_root)
        .map_err(|e| anyhow::anyhow!(
            "project root `{}` not readable: {e}",
            loaded.project_root.display()))?;

    // 4. Dispatch on exec variant.
    let outcome: Result<SkillOutput> = match &skill.exec {
        SkillExec::Shell { command } => {
            eprintln!("aislang skill: running shell-exec skill `{skill_label}` \
                       in sandbox `{}/.aislang/skills/{skill_label}/`",
                project_root.display());
            run_shell_skill(
                &skill.label, command, &project_root, skill.output_type,
            ).map_err(|e| anyhow::anyhow!("{e}"))
        }
        SkillExec::Mcp { server, tool } => {
            // Look up the referenced mcp_server block to get image
            // + mount.  Build a StdioChild via the same machinery
            // agents use for tool fleets.
            let server_block = proj.mcp_servers.iter()
                .find(|m| m.label == server.label)
                .ok_or_else(|| anyhow::anyhow!(
                    "skill `{skill_label}` references unknown mcp_server \
                     `{}`", server.label))?;
            let args_value: serde_json::Value = serde_json::from_str(&args_json)
                .map_err(|e| anyhow::anyhow!(
                    "--args is not valid JSON: {e}"))?;
            eprintln!("aislang skill: spawning mcp_server `{}` \
                       (image={}/{}) to delegate skill \
                       `{skill_label}`.tool=`{tool}`",
                server.label,
                server_block.image.registry, server_block.image.repository);
            // tokio runtime + StdioChild spawn — same shape as
            // chat_turn's MCP path; teardown on Drop.
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;
            let label_ref  = skill.label.clone();
            let tool_ref   = tool.clone();
            let out_type   = skill.output_type;
            let mount_abs  = server_block.mount.as_ref()
                .map(|m| if m.is_absolute() { m.clone() }
                         else { project_root.join(m) });
            // Stringify the OciImage same way mcp_server's
            // `format_image` does (registry/repository[:tag][@digest]).
            let img = &server_block.image;
            let image = match (&img.tag, &img.digest) {
                (Some(t), Some(d)) => format!("{}/{}:{t}@{d}",
                    img.registry, img.repository),
                (Some(t), None)    => format!("{}/{}:{t}",
                    img.registry, img.repository),
                (None,    Some(d)) => format!("{}/{}@{d}",
                    img.registry, img.repository),
                (None,    None)    => format!("{}/{}",
                    img.registry, img.repository),
            };
            rt.block_on(async move {
                use aislang_runtime::mcp::{config, StdioChild};
                let cfg = config(&image, &[],
                    mount_abs.as_deref());
                let child = StdioChild::spawn(&cfg).await
                    .map_err(|e| anyhow::anyhow!("spawn mcp_server: {e}"))?;
                let result = run_mcp_skill(
                    &label_ref, &child, &tool_ref, args_value, out_type,
                ).await.map_err(|e| anyhow::anyhow!("{e}"));
                child.shutdown().await.ok();
                result
            })
        }
    };

    // 5. Print the typed output to stdout (pipe-clean) + summary
    //    to stderr.
    let output = outcome?;
    eprintln!("aislang skill: completed `{skill_label}` (output_type = {:?})",
        skill.output_type);
    println!("{}", output.to_text());
    Ok(())
}

fn run_chat(agent_label: String, file: PathBuf, interactive: bool, keep_history: bool) -> Result<()> {
    // ── 1. read + validate source (single-file OR multi-module dir) ──
    // v0.3 Week 17 Tue: dir-vs-file dispatch via load_project_input.
    // The loader handles both shapes; we drive the same downstream
    // chat-via-mcp-serve plumbing with the resolved (project_root,
    // qualified_agent_label) pair.
    let loaded = load_project_input(&file)?;
    let project = &loaded.resolved.project;

    // ── 2. find dispatch target.  v0.3 Week 17 Thu: try agent
    //       first; if no match, try pipeline (orchestrator pattern).
    //       v0.3 Week 17 Fri: branch label → router pattern (this commit).
    let qualified_agent_label: String;
    let model_label:           String;
    let orchestrator_steps: Option<Vec<String>>;
    let router_dispatch:    Option<RouterDispatch>;
    // v0.4 Week 21 Mon follow-up — loop-label dispatch.  When
    // Some(N), the chat loop re-runs `orchestrator_steps` until
    // the final reply starts with `ACCEPT` or N attempts elapse.
    // None = single pipeline pass (the v0.3 behaviour).
    let loop_max_iter: Option<u32>;

    if let Some(resolved) = loaded.find_agent_label(&agent_label) {
        let agent = project.agents.iter()
            .find(|a| a.label == resolved)
            .expect("find_agent_label returned a label that doesn't exist");
        qualified_agent_label = agent.label.clone();
        let containing = qualified_agent_label.split_once('.').map(|(m, _)| m);
        model_label = match (&agent.model.module, containing) {
            (Some(m), _)    => format!("{m}.{}", agent.model.label),
            (None, Some(c)) => format!("{c}.{}", agent.model.label),
            (None, None)    => agent.model.label.clone(),
        };
        orchestrator_steps = None;
        router_dispatch    = None;
        loop_max_iter      = None;
    } else if let Some(resolved) = loaded.find_loop_label(&agent_label) {
        // v0.4 Week 21 Mon follow-up: loop-label dispatch
        // (W20 Mon scope-deferred carry-over; user-reported
        // bug 1).  The loop's body MUST resolve to a pipeline;
        // we drive that pipeline's steps through orchestrator
        // dispatch + re-run until ACCEPT or max_iter cap.
        let loop_block = project.loops.iter()
            .find(|l| l.label == resolved)
            .expect("find_loop_label returned a label that doesn't exist");
        let body_module = resolved.split_once('.').map(|(m, _)| m);
        let body_label = match (&loop_block.body.module, body_module) {
            (Some(m), _)    => format!("{m}.{}", loop_block.body.label),
            (None, Some(c)) => format!("{c}.{}", loop_block.body.label),
            (None, None)    => loop_block.body.label.clone(),
        };
        let body_pipeline = project.pipelines.iter()
            .find(|p| p.label == body_label)
            .ok_or_else(|| anyhow::anyhow!(
                "loop `{resolved}` body references pipeline `{body_label}` \
                 which does not exist"))?;
        if body_pipeline.steps.is_empty() {
            return Err(anyhow::anyhow!(
                "loop `{resolved}` body pipeline `{body_label}` has no steps"));
        }
        orchestrator_steps = Some(qualify_pipeline_steps(body_pipeline));
        qualified_agent_label = loop_block.label.clone();
        model_label = format!("(loop, max_iter={})", loop_block.max_iter);
        router_dispatch = None;
        loop_max_iter = Some(loop_block.max_iter);
    } else if let Some(resolved) = loaded.find_pipeline_label(&agent_label) {
        // v0.3 Week 17 Thu: pipeline-label dispatch → orchestrator
        // pattern.  Each user message triggers a full pipeline run;
        // we resolve each step's qualified agent label upfront so
        // the chat loop can drive mcp-serve sequentially.
        let pipeline = project.pipelines.iter()
            .find(|p| p.label == resolved)
            .expect("find_pipeline_label returned a label that doesn't exist");
        if pipeline.steps.is_empty() {
            return Err(anyhow::anyhow!(
                "pipeline `{resolved}` has no steps to dispatch"));
        }
        orchestrator_steps = Some(qualify_pipeline_steps(pipeline));
        qualified_agent_label = pipeline.label.clone();
        model_label = "(orchestrator)".to_string();   // banner-only
        router_dispatch = None;
        loop_max_iter = None;
    } else if let Some(resolved) = loaded.find_branch_label(&agent_label) {
        // v0.3 Week 17 Fri: branch-label dispatch → router pattern.
        // Per turn: classifier picks a case key from user input →
        // matched case's pipeline runs via orchestrator.  Cases
        // and default are resolved upfront to qualified step
        // labels; the per-turn classifier callback runs against
        // the project's FIRST declared agent (v0.3 convention —
        // see `RouterDispatch` docstring).
        let branch = project.branches.iter()
            .find(|b| b.label == resolved)
            .expect("find_branch_label returned a label that doesn't exist");
        router_dispatch = Some(RouterDispatch::build(&loaded, branch)?);
        qualified_agent_label = branch.label.clone();
        model_label = "(router)".to_string();   // banner-only
        orchestrator_steps = None;
        loop_max_iter = None;
    } else {
        // No exact match + no unique suffix match in any of agent /
        // pipeline / branch.  v0.3 Week 20 Mon follow-up: distinguish
        // the AMBIGUOUS case (multiple modules declare the same bare
        // label) from the truly UNKNOWN case (no module has it) — the
        // user-reported full_design collision (architect.full_design
        // vs orchestrator.full_design under multi-module) surfaced
        // here as a confusing "no agent named" message when the real
        // issue was ambiguity, not absence.
        let suffix = format!(".{agent_label}");
        let mut ambiguous: Vec<String> = Vec::new();
        for a in &project.agents {
            if a.label.ends_with(&suffix) {
                ambiguous.push(a.label.clone());
            }
        }
        for p in &project.pipelines {
            if p.label.ends_with(&suffix) {
                ambiguous.push(format!("{} (pipeline)", p.label));
            }
        }
        for b in &project.branches {
            if b.label.ends_with(&suffix) {
                ambiguous.push(format!("{} (branch)", b.label));
            }
        }
        for l in &project.loops {
            if l.label.ends_with(&suffix) {
                ambiguous.push(format!("{} (loop)", l.label));
            }
        }
        if ambiguous.len() >= 2 {
            ambiguous.sort();
            return Err(anyhow::anyhow!(
                "`{agent_label}` is ambiguous in {} — multiple modules declare it.\n  \
                 candidates: {}\n  \
                 disambiguate by typing the qualified `<module>.{agent_label}` form.",
                file.display(),
                ambiguous.join(", ")));
        }
        // Genuinely unknown — fall through to the v0.2 "no match"
        // diagnostic with the full available list + nearest-neighbour
        // suggestion.
        let mut available: Vec<String> = project.agents.iter()
            .map(|a| a.label.clone()).collect();
        available.extend(project.pipelines.iter()
            .map(|p| format!("{} (pipeline)", p.label)));
        available.extend(project.branches.iter()
            .map(|b| format!("{} (branch)", b.label)));
        available.extend(project.loops.iter()
            .map(|l| format!("{} (loop)", l.label)));
        available.sort();
        let agent_labels: Vec<&str> = project.agents.iter()
            .map(|a| a.label.as_str()).collect();
        let hint = nearest_agent_label(&agent_label, &agent_labels);
        let hint_line = match hint {
            Some(h) => format!(" — did you mean `{h}`?"),
            None    => String::new(),
        };
        let avail_line = if available.is_empty() {
            "  (no agents or pipelines declared in this project)".to_string()
        } else {
            format!("  available: {}", available.join(", "))
        };
        return Err(anyhow::anyhow!(
            "no agent, pipeline, or branch named `{agent_label}` in {}{hint_line}\n{avail_line}",
            file.display()
        ));
    }

    // Canonical source_dir for mcp-serve.  For dir input this IS
    // the project root; for file input it's the file's parent dir.
    let source_dir = std::fs::canonicalize(&loaded.project_root)
        .map_err(|e| anyhow::anyhow!(
            "source dir `{}` not readable: {e}",
            loaded.project_root.display()))?;

    // Canonical absolute path passed to mcp-serve.  For multi-
    // module: the project dir (mcp-serve detects via aislang.toml
    // and routes through build_session_for_agent_multi_module).
    // For single-file: the .ais file itself.
    let source_path = std::fs::canonicalize(&file)
        .map_err(|e| anyhow::anyhow!(
            "source file `{}` not readable: {e}", file.display()))?;

    // v0.4 W21 Mon follow-up — clean pipeline history at session
    // start unless `--keep-history`.  Only meaningful for multi-
    // step dispatches (orchestrator / loop) where the persistence
    // dir actually gets written; single-agent + router paths
    // never created it.
    if !keep_history && orchestrator_steps.is_some() {
        clean_pipeline_history(&source_dir, &qualified_agent_label);
    }

    // v0.4 W21 Tue — empty-RAG warning at chat-session start.
    // Scan every agent that's about to dispatch (the qualified
    // step labels for orchestrator + loop, the single agent for
    // direct chat) + warn on stderr if any of its `knowledge`
    // refs points at an empty / missing / corrupt RAG index.
    // Closes the W20 Thu dogfood-finding gap (empty indexes
    // silently masked the Coder hallucination root cause).  Fail-
    // soft: warning only, never aborts the chat.
    warn_on_empty_rag_indexes(
        &loaded, &source_dir,
        &qualified_agent_label, orchestrator_steps.as_deref(),
        router_dispatch.as_ref(),
    );

    // ── 3. spin tokio runtime + drive the MCP-proxied chat loop ──
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;
    let result = rt.block_on(chat_via_mcp_serve(
        source_path, source_dir,
        qualified_agent_label, model_label,
        orchestrator_steps, router_dispatch,
        interactive, loop_max_iter,
    ));
    drop(rt);
    result
}

/// Persist a single pipeline step's reply under `<root>/turn_<T>_
/// step_<N>_<agent>.md` and return the absolute path.  Best-effort:
/// IO errors print a warning + return the would-be path so the
/// caller can still surface "step happened" to the user.
///
/// v0.3 Week 20 Tue.  Hoisted to a top-level fn so unit tests can
/// pin the filename shape + the parent-dir creation + the
/// roundtrip-after-edit contract that `--interactive` relies on
/// without driving the full chat loop.
// v0.3 W20 Tue: single-iteration persistence helper.  Superseded by
// `persist_step_reply_iter` at v0.4 W21 Mon (the live caller uses
// the iter-aware variant); kept for the existing
// `chat_persistence_tests` filename-shape contract + as a thin
// fallback for any future single-pass dispatch that doesn't want
// the `iter_<I>` segment.
#[allow(dead_code)]
fn persist_step_reply(
    root:       &Path,
    turn:       u64,
    step:       usize,
    step_label: &str,
    reply:      &str,
) -> PathBuf {
    let fname = format!(
        "turn_{turn:03}_step_{step:02}_{step_label}.md");
    let path = root.join(fname);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, reply) {
        eprintln!("\x1b[2;3m  · warn: failed to persist step output to {}: {e}\x1b[0m",
            path.display());
    }
    path
}

/// v0.4 Week 21 Mon follow-up — iter-aware variant for loop
/// dispatch.  Filename: `turn_<T>_iter_<I>_step_<N>_<agent>.md`
/// so multi-iteration runs don't overwrite their own history
/// (user-reported bug 3 — pipeline iteration # visible in
/// artifacts as well as progress output).
fn persist_step_reply_iter(
    root:       &Path,
    turn:       u64,
    iter:       u32,
    step:       usize,
    step_label: &str,
    reply:      &str,
) -> PathBuf {
    let fname = format!(
        "turn_{turn:03}_iter_{iter:02}_step_{step:02}_{step_label}.md");
    let path = root.join(fname);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, reply) {
        eprintln!("\x1b[2;3m  · warn: failed to persist step output to {}: {e}\x1b[0m",
            path.display());
    }
    path
}

/// v0.4 Week 21 Mon follow-up — hard-error-signal detector for
/// the orchestrator's per-step abort path (user-reported bug 2).
/// Any agent that prefixes its reply with `## ERROR:` (after
/// trim, case-insensitive) signals "the upstream input is
/// unusable — stop the pipeline".  Single source of truth so
/// the orchestrator + future tests agree on the wire shape.
///
/// v0.4 Week 21 Mon follow-up #2: extended to tolerate the
/// common model-output shape variations that don't change
/// the user-facing meaning — extra leading hashes, bold markdown
/// `**ERROR:**`, the colon as part of a heading title, etc.  The
/// `## ERROR:` form remains the documented canonical, but the
/// detector is forgiving of header-level (`#` count) +
/// emphasis-marker drift so a model that slips out of the strict
/// `## ERROR:` shape doesn't escape the abort path.
fn is_hard_error_signal(reply: &str) -> bool {
    // v0.4 W21 Fri (Decision 46.6 finalize): bilingual error
    // detector — markdown shape (v0.3 W21 Mon canonical
    // `## ERROR:` + drift tolerances) OR JSON error envelope
    // (W21 Thu `{"error": {"code":..., "message":...}}`).  The
    // runtime trips on EITHER so mixed-mode chains (one agent
    // emits JSON tool-call, another still emits markdown
    // fallback) get a unified abort path.
    if aislang_runtime::schemas::looks_like_error_envelope(reply) {
        return true;
    }
    let t = reply.trim_start();
    // Strip leading markdown header / emphasis tokens so we
    // recognise `## ERROR:`, `### ERROR:`, `**ERROR:**`,
    // `ERROR:` bare, etc.  Anchored to the START of the reply
    // + REQUIRE the colon (or bracket-wrapped abort marker) so
    // a body sentence like "Error elsewhere in the patch" doesn't
    // trip the detector.
    let stripped = t
        .trim_start_matches('#')
        .trim_start_matches('*')
        .trim_start();
    let lc = stripped.to_lowercase();
    lc.starts_with("error:") ||
        lc.starts_with("[pipeline_abort]") ||
        lc.starts_with("pipeline_abort:")
}

/// v0.4 Week 21 Mon follow-up — local ACCEPT-verdict detector
/// matching `patterns::reflector::is_accept_verdict` semantics
/// (trim + lowercase + starts-with `accept`).  Inlined here so
/// the loop dispatch doesn't have to depend on the runtime
/// patterns crate's private fn.
///
/// v0.4 W21 Fri (Decision 46.6 finalize): bilingual — also
/// recognises the JSON tool-call shape `{"verdict": "ACCEPT"}`
/// that Reviewer + Tester emit under the JSON contract.  Same
/// detector for both wire formats so mixed-mode chains converge.
fn is_accept_verdict_local(reply: &str) -> bool {
    let trimmed = reply.trim();
    // Fast path — v0.3 markdown shape.
    if trimmed.to_lowercase().starts_with("accept") {
        return true;
    }
    // v0.4 JSON shape — `{"verdict": "ACCEPT", ...}` (any field
    // order, optional whitespace).  Parse the candidate as JSON
    // + check the top-level `verdict` field.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(verdict) = v.get("verdict").and_then(|x| x.as_str()) {
            return verdict.eq_ignore_ascii_case("accept");
        }
    }
    false
}

/// v0.4 Week 21 Mon follow-up — pipeline-history cleanup at
/// chat-session start (user-reported bug 4 — old + new chat
/// artifacts mixing in `.aislang/pipeline/<label>/`).  Wipes
/// the per-pipeline subdir before the first turn so each chat
/// session starts with a clean trace.  Best-effort: errors
/// print a warning + continue.  `--keep-history` (lifted to
/// the CLI) skips this.
fn clean_pipeline_history(source_dir: &Path, pipeline_label: &str) {
    let p = source_dir.join(".aislang").join("pipeline").join(pipeline_label);
    if p.exists() {
        if let Err(e) = std::fs::remove_dir_all(&p) {
            eprintln!("\x1b[2;3m  · warn: failed to clean {}: {e}\x1b[0m",
                p.display());
        }
    }
}

/// v0.4 W21 Tue — chat-time empty-RAG warning.  Scans every
/// agent that's about to dispatch + warns on stderr if any of
/// its `knowledge` refs points at an empty / missing / corrupt
/// RAG index.  Fail-soft — only emits warnings, never aborts
/// the chat session.
///
/// Resolution rules:
/// - **Direct agent chat** (`orchestrator_steps == None &&
///   router_dispatch == None`) — scan the single `agent_label`'s
///   knowledge.
/// - **Orchestrator / loop** (`orchestrator_steps == Some(_)`) —
///   scan every qualified step label.
/// - **Router branch** — scan every unique agent across all
///   case-pipelines + the default-pipeline.
fn warn_on_empty_rag_indexes(
    loaded:             &LoadedProject,
    source_dir:         &Path,
    agent_label:        &str,
    orchestrator_steps: Option<&[String]>,
    router_dispatch:    Option<&RouterDispatch>,
) {
    let project = &loaded.resolved.project;
    // Build the set of qualified agent labels to scan.
    let mut to_scan: Vec<String> = Vec::new();
    if let Some(steps) = orchestrator_steps {
        to_scan.extend(steps.iter().cloned());
    } else if let Some(r) = router_dispatch {
        to_scan.push(r.classifier_agent_label.clone());
        for (_, steps) in &r.case_keys_and_steps {
            to_scan.extend(steps.iter().cloned());
        }
        to_scan.extend(r.default_steps.iter().cloned());
    } else {
        to_scan.push(agent_label.to_string());
    }
    // De-dup while preserving order — `agent_label` may repeat
    // across cases.
    to_scan.sort();
    to_scan.dedup();

    // For each agent, look up its `knowledge` refs + check each
    // store's on-disk index.
    let project_root = std::fs::canonicalize(source_dir)
        .unwrap_or_else(|_| source_dir.to_path_buf());
    for agent_lbl in &to_scan {
        let agent = match project.agents.iter().find(|a| a.label == *agent_lbl) {
            Some(a) => a,
            None    => continue,    // already warned at resolution time
        };
        let knowledge = match &agent.knowledge {
            Some(k) if !k.is_empty() => k,
            _ => continue,
        };
        for vs_ref in knowledge {
            let vs_key = vs_ref.qualified_in(&agent.label);
            let vs = match project.vector_stores.iter()
                .find(|v| v.label == vs_key)
            {
                Some(v) => v,
                None    => continue,    // checker should have caught this
            };
            let idx_path = aislang_runtime::rag::vector_index_dir(&project_root)
                .join(format!("{}.bin", vs.label));
            match inspect_rag_index(&idx_path) {
                RagIndexReport::Healthy { .. } => { /* quiet on healthy */ }
                RagIndexReport::Empty { .. } => {
                    eprintln!(
                        "\x1b[2;3m  · warn: agent `{}`'s knowledge store `{}` has 0 chunks — agent will run UNGROUNDED.  Run `aislang rag build {} <project>` to fix.\x1b[0m",
                        agent_lbl, vs.label, vs.label);
                }
                RagIndexReport::Missing => {
                    eprintln!(
                        "\x1b[2;3m  · warn: agent `{}`'s knowledge store `{}` index file is MISSING — agent will run UNGROUNDED.  Run `aislang apply <project>` to build the indexes.\x1b[0m",
                        agent_lbl, vs.label);
                }
                RagIndexReport::Corrupt { cause } => {
                    eprintln!(
                        "\x1b[2;3m  · warn: agent `{}`'s knowledge store `{}` index file is CORRUPT ({cause}) — agent will run UNGROUNDED.\x1b[0m",
                        agent_lbl, vs.label);
                }
            }
        }
    }
}

#[cfg(test)]
mod chat_persistence_tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "aislang-cli-chat-persist-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(
                std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn persist_step_reply_writes_with_zero_padded_turn_and_step() {
        // Pins the filename shape that `--interactive` relies on
        // (the user's hand-edit roundtrips through the SAME path).
        let root = tempdir();
        let p = persist_step_reply(&root, 1, 2, "coder.coder",
            "## Patch\nfoo\n");
        assert_eq!(p.file_name().unwrap(),
            "turn_001_step_02_coder.coder.md");
        let read = std::fs::read_to_string(&p).unwrap();
        assert_eq!(read, "## Patch\nfoo\n");
    }

    #[test]
    fn persist_step_reply_creates_missing_parent_dirs() {
        // `<source_dir>/.aislang/pipeline/<pipeline_label>/`
        // doesn't exist on a fresh project.  The helper MUST
        // create it (recursively) or the first orchestrator
        // run loses its trace.
        let root = tempdir().join("nested").join("doesnt").join("exist");
        assert!(!root.exists());
        let p = persist_step_reply(&root, 1, 1, "architect.architect",
            "design body");
        assert!(p.exists());
        assert!(root.exists());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "design body");
    }

    #[test]
    fn persist_step_reply_overwrites_same_step_across_calls() {
        // Reflector / loop dispatch re-runs the same (pipeline,
        // step, agent) within a single turn → must overwrite,
        // not collide.  The turn counter increments per
        // top-level orchestrator run so cross-turn writes get
        // distinct files automatically.
        let root = tempdir();
        let p1 = persist_step_reply(&root, 5, 3, "coder.coder",
            "v1 patch");
        let p2 = persist_step_reply(&root, 5, 3, "coder.coder",
            "v2 patch");
        assert_eq!(p1, p2);
        assert_eq!(std::fs::read_to_string(&p2).unwrap(), "v2 patch");
    }

    #[test]
    fn persist_step_reply_round_trip_supports_hand_edit() {
        // The `--interactive` UX is: persist → user edits the
        // file → CLI re-reads.  Test the file-IO contract — a
        // post-write modification round-trips on re-read.
        let root = tempdir();
        let p = persist_step_reply(&root, 1, 1, "architect.architect",
            "first draft");
        std::fs::write(&p, "user-edited body").unwrap();
        let read = std::fs::read_to_string(&p).unwrap();
        assert_eq!(read, "user-edited body",
            "user hand-edit must round-trip through the same path");
    }

    // ── v0.4 Week 21 Mon follow-up tests ──────────────────────

    #[test]
    fn persist_step_reply_iter_filename_has_iter_segment() {
        // Loop-dispatch filename shape must include iter_<I>
        // so multi-iteration runs don't overwrite each other
        // (user-reported bug 3).
        let root = tempdir();
        let p = persist_step_reply_iter(&root, 1, 2, 3, "coder.coder",
            "## Patch v2\n");
        assert_eq!(p.file_name().unwrap(),
            "turn_001_iter_02_step_03_coder.coder.md");
        let read = std::fs::read_to_string(&p).unwrap();
        assert_eq!(read, "## Patch v2\n");
    }

    #[test]
    fn persist_step_reply_iter_does_not_overwrite_other_iters() {
        // Same (turn, step, agent) at different iter values must
        // land at DISTINCT paths.  Pin the loop-dispatch
        // non-overwrite invariant.
        let root = tempdir();
        let p1 = persist_step_reply_iter(&root, 1, 1, 2, "coder.coder", "v1");
        let p2 = persist_step_reply_iter(&root, 1, 2, 2, "coder.coder", "v2");
        let p3 = persist_step_reply_iter(&root, 1, 3, 2, "coder.coder", "v3");
        assert_ne!(p1, p2);
        assert_ne!(p2, p3);
        assert_eq!(std::fs::read_to_string(&p1).unwrap(), "v1");
        assert_eq!(std::fs::read_to_string(&p2).unwrap(), "v2");
        assert_eq!(std::fs::read_to_string(&p3).unwrap(), "v3");
    }

    #[test]
    fn is_hard_error_signal_detects_md_header_prefix() {
        // Canonical shape from the agent prompts: `## ERROR:`
        assert!(is_hard_error_signal("## ERROR: upstream produced garbage"));
        assert!(is_hard_error_signal("## error: lowercase still counts"));
        assert!(is_hard_error_signal("  ## ERROR: leading whitespace ok"));
        // Different header levels — common model drift.
        assert!(is_hard_error_signal("### ERROR: deeper heading"));
        assert!(is_hard_error_signal("# ERROR: top-level heading"));
        // Bold-emphasis shapes — model drift toward different markdown.
        assert!(is_hard_error_signal("**ERROR:** bold emphasis form"));
        assert!(is_hard_error_signal("**error:** lowercase bold"));
        // Bare prefix without markdown markers.
        assert!(is_hard_error_signal("ERROR: bare prefix"));
        assert!(is_hard_error_signal("error: lowercase bare"));
        // Alternative abort marker.
        assert!(is_hard_error_signal("[PIPELINE_ABORT] alternative marker"));
        assert!(is_hard_error_signal("PIPELINE_ABORT: also accepted"));

        // Non-error replies don't trip it.
        assert!(!is_hard_error_signal("## Patch\n```rust\n..."));
        assert!(!is_hard_error_signal("ACCEPT\nlooks good"));
        assert!(!is_hard_error_signal("error elsewhere in the body"));
        // CRITICAL anti-false-positive: prose body sentences that
        // happen to start with the word "Error" must NOT trip
        // (caught the v0.4 W21 Mon detector-strengthen pass).
        assert!(!is_hard_error_signal("Error analyses found three issues"));
        assert!(!is_hard_error_signal("Errors are expected — see Test coverage"));
        assert!(!is_hard_error_signal(""));
    }

    #[test]
    fn is_accept_verdict_local_matches_runtime_semantics() {
        // Mirror patterns::reflector::is_accept_verdict — trim +
        // lowercase + starts-with `accept`.  The CLI loop dispatch
        // uses this verdict to terminate the outer loop early.
        assert!(is_accept_verdict_local("ACCEPT"));
        assert!(is_accept_verdict_local("Accept\nwith rationale"));
        assert!(is_accept_verdict_local("  accept  "));
        assert!(!is_accept_verdict_local("FAIL: missing test"));
        assert!(!is_accept_verdict_local("rejects with concerns"));
        assert!(!is_accept_verdict_local(""));
    }

    #[test]
    fn is_accept_verdict_local_recognises_json_verdict() {
        // v0.4 W21 Fri (Decision 46.6): bilingual detector also
        // matches `{"verdict": "ACCEPT"}` JSON shape that Reviewer
        // + Tester emit under the JSON contract.
        assert!(is_accept_verdict_local(r#"{"verdict": "ACCEPT"}"#));
        assert!(is_accept_verdict_local(
            r#"{"verdict": "ACCEPT", "rationale": "byte-stable"}"#));
        assert!(is_accept_verdict_local(
            r#"{"verdict": "accept", "rationale": "lowercase ok"}"#));
        // FAIL verdict → false.
        assert!(!is_accept_verdict_local(
            r#"{"verdict": "FAIL", "rationale": "hcl::Block used"}"#));
        // Error envelope → false (the abort signal goes through
        // is_hard_error_signal, NOT through ACCEPT detection).
        assert!(!is_accept_verdict_local(
            r#"{"error": {"code": "input_malformed", "message": "..."}}"#));
        // Bare empty object → false.
        assert!(!is_accept_verdict_local(r#"{}"#));
    }

    #[test]
    fn is_hard_error_signal_recognises_json_error_envelope() {
        // v0.4 W21 Fri (Decision 46.6): bilingual error detector
        // — markdown shape OR JSON envelope.  Closes the abort
        // path under mixed-mode chains.
        assert!(is_hard_error_signal(
            r#"{"error": {"code": "input_malformed", "message": "Architect output missing block_name"}}"#));
        assert!(is_hard_error_signal(
            r#"{"error": {"code": "downstream_propagate", "message": "upstream forwarded"}}"#));
        // Reviewer ACCEPT JSON → false (not an error envelope).
        assert!(!is_hard_error_signal(
            r#"{"verdict": "ACCEPT", "rationale": "good"}"#));
        // Tester FAIL JSON → false (gaps[] is non-ACCEPT but not
        // an abort — reflector loop iterates).
        assert!(!is_hard_error_signal(
            r#"{"verdict": "FAIL", "rationale": "...", "gaps": []}"#));
        // Architect happy-path JSON → false.
        assert!(!is_hard_error_signal(
            r#"{"block_name": "dataset", "purpose": "...", "fields": [], "grammar_example": "", "language_spec_snippet": "", "decision_implications": {"new_refkind": null, "checker_invariants": [], "decision_conflicts": [], "byte_stable_v0_2_roundtrip": true}}"#));
    }

    #[test]
    fn clean_pipeline_history_wipes_existing_subdir() {
        // Pin the bug-4 cleanup contract: chat-session start
        // wipes `.aislang/pipeline/<label>/`.  Best-effort —
        // missing dir is a no-op.
        let dir = tempdir();
        let pipeline = "orchestrator.full_design";
        let subdir = dir.join(".aislang").join("pipeline").join(pipeline);
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("turn_001_step_01_x.md"), "stale\n").unwrap();
        assert!(subdir.exists());

        clean_pipeline_history(&dir, pipeline);
        assert!(!subdir.exists(),
            "subdir must be removed after clean_pipeline_history");

        // Re-cleaning a non-existent dir is a silent no-op.
        clean_pipeline_history(&dir, pipeline);
    }

    #[test]
    fn clean_pipeline_history_leaves_other_pipelines_untouched() {
        // Per-pipeline scoping: cleaning `full_design` doesn't
        // touch `review_cycle`.
        let dir = tempdir();
        let kept_subdir = dir.join(".aislang").join("pipeline")
            .join("orchestrator.review_cycle");
        std::fs::create_dir_all(&kept_subdir).unwrap();
        std::fs::write(kept_subdir.join("turn_001_step_01_x.md"),
            "kept\n").unwrap();

        clean_pipeline_history(&dir, "orchestrator.full_design");

        assert!(kept_subdir.exists(),
            "sibling pipeline subdir must NOT be touched");
        assert_eq!(std::fs::read_to_string(
            kept_subdir.join("turn_001_step_01_x.md")).unwrap(),
            "kept\n");
    }

    // ── v0.4 Week 21 Tue: rag verify / chat-time warning ──

    /// Synthesize a minimal Index .bin file on disk with the
    /// given chunk count.  The runtime's `Index::save` is the
    /// "right" way but adds bge-m3 dim noise; for the verifier's
    /// pure classification tests, this hand-rolled JSON shape
    /// matches `Index`'s serde wire format (label/dims/chunks).
    fn write_index_bin(path: &std::path::Path, chunks: usize) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let chunks_json: Vec<String> = (0..chunks)
            .map(|i| format!(
                "{{\"text\":\"chunk {i}\",\"vector\":[0.0]}}"))
            .collect();
        let body = format!(
            "{{\"label\":\"test_kb\",\"dims\":1,\"chunks\":[{}]}}",
            chunks_json.join(","));
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn inspect_rag_index_classifies_healthy_when_chunks_present() {
        let dir = tempdir();
        let p = dir.join("healthy.bin");
        write_index_bin(&p, 5);
        match inspect_rag_index(&p) {
            RagIndexReport::Healthy { chunks, bytes } => {
                assert_eq!(chunks, 5);
                assert!(bytes > 0);
            }
            other => panic!("expected Healthy, got {other:?}"),
        }
    }

    #[test]
    fn inspect_rag_index_classifies_empty_when_zero_chunks() {
        // The W20 Thu dogfood-finding shape: 50-byte file with
        // an empty chunks array.  The verifier MUST flag this
        // as Empty (not Healthy) so the user sees the gap before
        // the agent runs ungrounded.
        let dir = tempdir();
        let p = dir.join("empty.bin");
        write_index_bin(&p, 0);
        match inspect_rag_index(&p) {
            RagIndexReport::Empty { bytes } => {
                assert!(bytes > 0,
                    "Empty != Missing — file exists but has 0 chunks");
            }
            other => panic!("expected Empty, got {other:?}"),
        }
    }

    #[test]
    fn inspect_rag_index_classifies_missing_when_file_absent() {
        let dir = tempdir();
        let p = dir.join("nonexistent.bin");
        assert!(!p.exists());
        match inspect_rag_index(&p) {
            RagIndexReport::Missing => { /* expected */ }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn inspect_rag_index_classifies_corrupt_when_json_invalid() {
        // Malformed payload — neither a valid Index nor an empty
        // file.  Verifier must report Corrupt with the cause,
        // not silently treat as missing.
        let dir = tempdir();
        let p = dir.join("corrupt.bin");
        std::fs::write(&p, b"this is not JSON at all").unwrap();
        match inspect_rag_index(&p) {
            RagIndexReport::Corrupt { cause } => {
                assert!(!cause.is_empty());
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }
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

/// v0.3 Week 17 Thu: `orchestrator_steps` — when `Some(steps)`,
/// each user input triggers an orchestrator-pattern dispatch:
/// N sequential `aislang_chat_turn` calls threading each step's
/// reply as the next step's `user_input`.  When `None`, the legacy
/// single-agent-per-turn shape (v0.2 Week 13 Wed) runs.
///
/// v0.3 Week 17 Fri: `router_dispatch` — when `Some(state)`, each
/// user input runs a classifier turn first (against the project's
/// first declared agent), matches the reply against the branch's
/// case keys, then dispatches the matched case-pipeline as an
/// orchestrator run.  Mutually exclusive with `orchestrator_steps`
/// (we'd never have both — `run_chat` picks one or the other).
async fn chat_via_mcp_serve(
    source_path: PathBuf,
    source_dir:  PathBuf,
    agent_label: String,
    model_label: String,
    orchestrator_steps: Option<Vec<String>>,
    router_dispatch:    Option<RouterDispatch>,
    interactive:        bool,
    loop_max_iter:      Option<u32>,
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
    //
    // v0.3 Week 17 Thu: orchestrator-pattern dispatch banner names
    // the pipeline + the step count.  v0.3 Week 17 Fri: router-pattern
    // banner names the branch + lists the case keys.  Single-agent
    // path keeps the v0.2 wording byte-stable for the existing
    // chat-banner tests.
    if let Some(steps) = orchestrator_steps.as_ref() {
        if let Some(mx) = loop_max_iter {
            eprintln!("aislang chat — loop `{agent_label}` (orchestrator pattern, \
                       {} step{}, max_iter={mx}) via aislang mcp-serve --stdio",
                steps.len(), if steps.len() == 1 { "" } else { "s" });
        } else {
            eprintln!("aislang chat — pipeline `{agent_label}` (orchestrator pattern, \
                       {} steps) via aislang mcp-serve --stdio", steps.len());
        }
        eprintln!("steps: {}", steps.join(" → "));
    } else if let Some(r) = router_dispatch.as_ref() {
        let case_keys: Vec<&str> = r.case_keys_and_steps.iter()
            .map(|(k, _)| k.as_str()).collect();
        eprintln!("aislang chat — branch `{}` (router pattern, {} cases) \
                   via aislang mcp-serve --stdio",
            r.branch_label, case_keys.len());
        eprintln!("cases: {} (default: {} step{})",
            case_keys.join(" | "),
            r.default_steps.len(),
            if r.default_steps.len() == 1 { "" } else { "s" });
        eprintln!("classifier agent: `{}`", r.classifier_agent_label);
    } else {
        eprintln!("aislang chat — agent `{agent_label}` (model alias `{model_label}`) \
                   via aislang mcp-serve --stdio");
    }
    eprintln!("connected to LiteLLM via mcp-serve's Session pool.");
    eprintln!("type your message and press Enter; `/exit` or Ctrl-D ends the session.");
    eprintln!("for multi-line input (paste a markdown design etc.), open with `/paste`");
    eprintln!("then end with `/end` on its own line.");
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

        let first_line = match reader.next_line().await {
            Ok(Some(l)) => l,
            Ok(None)    => break "(end of input)",
            Err(e)      => {
                client.cancel().await.ok();
                return Err(anyhow::anyhow!("stdin read error: {e}"));
            }
        };
        let first_trimmed = first_line.trim();
        if first_trimmed.is_empty() { continue; }
        if first_trimmed == "/exit" { break "(/exit — session ended)"; }

        // v0.3 Week 20 Mon follow-up #2: multi-line paste mode.
        // Pasting a multi-line markdown (e.g. the Architect's
        // 5-section design into the Coder) under line-buffered
        // stdin sends each line as a separate turn — the receiving
        // agent sees a stream of one-line fragments and keeps
        // asking for "the full input".  `/paste` opens an
        // accumulating buffer + `/end` on its own line sends the
        // joined content as ONE user message.  `/exit` inside the
        // buffer aborts the session (same precedence as outside).
        let line: String = if first_trimmed == "/paste" {
            let mut buf = String::new();
            loop {
                let next = match reader.next_line().await {
                    Ok(Some(l)) => l,
                    Ok(None)    => break,            // EOF closes the buffer
                    Err(e) => {
                        client.cancel().await.ok();
                        return Err(anyhow::anyhow!("stdin read error in /paste: {e}"));
                    }
                };
                if next.trim() == "/end"  { break; }
                if next.trim() == "/exit" {
                    // Empty out the buffer + propagate the exit at
                    // the outer loop's next iteration.  Simpler than
                    // smuggling a flag out of the inner block.
                    buf.clear();
                    buf.push_str("/exit");
                    break;
                }
                if !buf.is_empty() { buf.push('\n'); }
                buf.push_str(&next);
            }
            if buf == "/exit" { break "(/exit — session ended)"; }
            if buf.is_empty() { continue; }
            buf
        } else {
            first_trimmed.to_string()
        };
        let line = line.as_str();

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

        // v0.3 Week 17 Thu: orchestrator-pattern dispatch threads
        // each step's reply as the next step's user_input.  Single-
        // agent (v0.2) shape is the steps.len() == 1 case where
        // `agent_label` IS the only step.
        //
        // v0.3 Week 17 Fri: router-pattern dispatch runs an extra
        // classifier turn BEFORE the orchestrator loop, against the
        // project's first declared agent.  The classifier reply
        // picks a case-key (substring + case-insensitive match,
        // same logic as `patterns::router::route` — kept in sync,
        // inlined here for closure-lifetime simplicity).  If no
        // case matches → fall through to the `default` steps.
        let steps: Vec<String> = if let Some(r) = router_dispatch.as_ref() {
            let prompt = build_router_classifier_prompt(
                &r.case_keys_and_steps, line);
            eprintln!("\x1b[2;3m  · router classifying via agent \
                       `{}` against cases [{}]\x1b[0m",
                r.classifier_agent_label,
                r.case_keys_and_steps.iter()
                    .map(|(k, _)| k.as_str())
                    .collect::<Vec<_>>().join(", "));
            let mut cls_params = CallToolRequestParams::new("aislang_chat_turn")
                .with_arguments(serde_json::json!({
                    "source_path": source_path.to_string_lossy(),
                    "source_dir":  source_dir.to_string_lossy(),
                    "agent_label": &r.classifier_agent_label,
                    "user_input":  prompt,
                }).as_object().unwrap().clone());
            cls_params.set_progress_token(progress_token.clone());
            match client.peer().call_tool(cls_params).await {
                Ok(result) => {
                    let reply = result.content.first()
                        .and_then(|c| match &c.raw {
                            RawContent::Text(t) => Some(t.text.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    let normalized = reply.trim().to_lowercase();
                    let picked = r.case_keys_and_steps.iter()
                        .find(|(k, _)| {
                            let lk = k.to_lowercase();
                            lk == normalized || normalized.contains(&lk)
                        });
                    match picked {
                        Some((k, s)) => {
                            eprintln!("\x1b[2;3m  · router → case `{k}` ({} steps)\x1b[0m",
                                s.len());
                            s.clone()
                        }
                        None => {
                            eprintln!("\x1b[2;3m  · router → no case match \
                                       (reply: {:?}) → default ({} steps)\x1b[0m",
                                reply.trim(), r.default_steps.len());
                            r.default_steps.clone()
                        }
                    }
                }
                Err(e) => {
                    eprintln!("aislang chat: router classifier turn failed: {e}");
                    // Fall through to default — degraded but
                    // continues the chat session rather than killing it.
                    eprintln!("\x1b[2;3m  · falling back to default ({} steps)\x1b[0m",
                        r.default_steps.len());
                    r.default_steps.clone()
                }
            }
        } else {
            match orchestrator_steps.as_ref() {
                Some(s) => s.clone(),
                None    => vec![agent_label.clone()],
            }
        };
        // v0.3 Week 20 Tue: persist each step's reply under
        // `<source_dir>/.aislang/pipeline/<pipeline_label>/turn_<T>_
        // iter_<I>_step_<N>_<agent>.md` so the user can compare /
        // diff between runs.  v0.4 W21 Mon follow-up: filename gains
        // an `iter_<I>` segment when running under a loop
        // dispatch (user-reported bug 3 — pipeline iteration #
        // visible in persisted artifacts + progress output).
        // Single-agent + router paths skip persistence (no
        // step-by-step surface to inspect).
        let persist_root = if steps.len() > 1 {
            Some(source_dir.join(".aislang").join("pipeline").join(&agent_label))
        } else {
            None
        };

        // v0.4 W21 Mon follow-up — outer loop wrapping the
        // orchestrator step dispatch.  When `loop_max_iter` is
        // Some(N), re-run the inner step loop until the final
        // step's reply starts with `ACCEPT` or N iterations
        // elapse.  When None, run exactly once (the v0.3
        // behaviour).  Matches `patterns::reflector::is_accept
        // _verdict` semantics on the final-step reply.
        let max_iter = loop_max_iter.unwrap_or(0);     // 0 = single pass
        let mut final_reply: Option<String> = None;
        let mut outer_dispatch_failed = false;
        let mut outer_error_signal = false;
        'outer: for iter in 0..=max_iter {
            let mut current_input = if iter == 0 {
                line.to_string()
            } else {
                // Subsequent iterations feed the PRIOR FULL-PIPELINE
                // reply back as the first step's input.  This is
                // the simplest loop semantics: pipeline body keeps
                // chewing on the cumulative output until the
                // critic-equivalent (final step) ACCEPTs.
                final_reply.clone().unwrap_or_else(|| line.to_string())
            };
            let mut last_reply: Option<String> = None;
            let mut dispatch_failed = false;
            let mut error_signal    = false;
            for (step_idx, step_label) in steps.iter().enumerate() {
                if steps.len() > 1 {
                    // v0.4 W21 Mon follow-up #2: UNIFIED iter counter
                    // format across pipeline + loop dispatch (user-
                    // reported issue 5).  Single-pass pipelines show
                    // `iter 1/1` so the format is shape-stable across
                    // the dispatch surfaces.
                    eprintln!(
                        "\x1b[2;3m  · iter {}/{} · step {}/{} → agent `{step_label}`\x1b[0m",
                        iter + 1, max_iter + 1, step_idx + 1, steps.len());
                }
                let mut params = CallToolRequestParams::new("aislang_chat_turn")
                    .with_arguments(serde_json::json!({
                        "source_path": source_path.to_string_lossy(),
                        "source_dir":  source_dir.to_string_lossy(),
                        "agent_label": step_label,
                        "user_input":  current_input,
                    }).as_object().unwrap().clone());
                params.set_progress_token(progress_token.clone());
                match client.peer().call_tool(params).await {
                    Ok(result) => {
                        let reply = result.content.first()
                            .and_then(|c| match &c.raw {
                                RawContent::Text(t) => Some(t.text.clone()),
                                _ => None,
                            })
                            .unwrap_or_else(|| "(empty response)".to_string());
                        // Persist with iter-aware filename so
                        // loop runs don't overwrite their own
                        // history within the turn.  Persist ALWAYS
                        // — even on the error path, so the user
                        // can inspect the erroring reply later.
                        let persisted_path: Option<PathBuf> = persist_root.as_ref()
                            .map(|root| {
                                persist_step_reply_iter(
                                    root, turn_counter, iter + 1,
                                    step_idx + 1, step_label, &reply)
                            });
                        // v0.4 W21 Mon follow-up #2: detect hard
                        // error FIRST (user-reported issue 4 — the
                        // erroring reply was being printed as
                        // intermediate-step indented output AND as
                        // the final stdout reply, total 4-5 mentions).
                        // Single eprintln on the abort path; the
                        // final `pipeline terminated on hard error`
                        // line + stdout reply are still emitted in
                        // the post-loop block.  This single-print
                        // path also unifies pipeline + loop abort
                        // behaviour (user-reported issue 1 — review_
                        // cycle "appeared to continue" because the
                        // same erroring reply was printed 5x).
                        if is_hard_error_signal(&reply) {
                            eprintln!(
                                "\x1b[2;3m  · hard error signalled by `{step_label}`; aborting pipeline\x1b[0m");
                            last_reply = Some(reply);
                            error_signal = true;
                            break;
                        }
                        // No error → safe to print the intermediate
                        // indented reply (skipped on the final step
                        // since the post-loop block prints it to
                        // stdout).
                        if steps.len() > 1 && step_idx + 1 < steps.len() {
                            let indented = reply.lines()
                                .map(|l| format!("    \x1b[2;3m{l}\x1b[0m"))
                                .collect::<Vec<_>>().join("\n");
                            eprintln!("{indented}");
                        }
                        if interactive
                            && steps.len() > 1
                            && step_idx + 1 < steps.len()
                            && persisted_path.is_some()
                        {
                            let p = persisted_path.as_ref().unwrap();
                            eprintln!("\x1b[2;3m  · --interactive: edit {} if you want; press Enter to continue\x1b[0m",
                                p.display());
                            let _ = reader.next_line().await;
                            match std::fs::read_to_string(p) {
                                Ok(edited) => {
                                    if edited != reply {
                                        eprintln!("\x1b[2;3m  · picked up hand-edit ({} -> {} bytes)\x1b[0m",
                                            reply.len(), edited.len());
                                    }
                                    current_input = edited;
                                }
                                Err(e) => {
                                    eprintln!("\x1b[2;3m  · warn: re-read of {} failed: {e}; falling back to in-memory reply\x1b[0m",
                                        p.display());
                                    current_input = reply.clone();
                                }
                            }
                        } else {
                            current_input = reply.clone();
                        }
                        last_reply = Some(reply);
                    }
                    Err(e) => {
                        eprintln!("aislang chat: orchestrator step {}/{} (`{step_label}`) failed: {e}",
                            step_idx + 1, steps.len());
                        dispatch_failed = true;
                        break;
                    }
                }
            }
            if dispatch_failed {
                outer_dispatch_failed = true;
                break 'outer;
            }
            if error_signal {
                outer_error_signal = true;
                final_reply = last_reply;
                break 'outer;
            }
            final_reply = last_reply.clone();
            // Loop termination: ACCEPT on final step's reply,
            // OR max_iter hit.  When `loop_max_iter` is None
            // (max_iter == 0), the for-loop range 0..=0 runs
            // exactly once + drops out — v0.3 single-pass
            // behaviour preserved.
            if loop_max_iter.is_some() {
                if let Some(r) = &last_reply {
                    if is_accept_verdict_local(r) {
                        eprintln!(
                            "\x1b[2;3m  · loop terminated at iter {}/{} on ACCEPT\x1b[0m",
                            iter + 1, max_iter + 1);
                        break 'outer;
                    }
                }
                if iter == max_iter {
                    eprintln!(
                        "\x1b[2;3m  · loop hit max_iter cap ({}); no ACCEPT — surfacing last reply\x1b[0m",
                        max_iter);
                }
            }
        }
        if !outer_dispatch_failed {
            if let Some(reply) = final_reply {
                // v0.4 W21 Mon follow-up #2: the per-step
                // "hard error signalled by `X`; aborting pipeline"
                // line already covers the abort summary — DON'T
                // double-print a redundant "pipeline terminated"
                // marker here.  Just emit the final reply (which
                // IS the erroring `## ERROR:` text on stdout, so
                // pipes / tee'd logs still see the actual error)
                // and let the per-step abort line speak for itself.
                let _ = outer_error_signal;
                println!("{reply}");
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

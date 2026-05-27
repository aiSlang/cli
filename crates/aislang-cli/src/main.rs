//! `aislang` — declarative AI infrastructure CLI.
//!
//! v0.1 subcommand surface (most are stubs until their scheduled week).
//! See PLAN-90-days.md for the implementation schedule.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "aislang",
    version,
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
    Destroy {
        file: PathBuf,
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
    /// Show projected and actual cost for the project
    Cost {
        file: PathBuf,
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
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
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

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate { file, verbose }            => run_validate(file, verbose),
        Command::Plan { file, verbose, no_lock, output_dir, smoke_test, smoke_test_live } =>
            run_plan(file, verbose, no_lock, output_dir, smoke_test, smoke_test_live),
        Command::Apply { file }      => run_apply(file),
        Command::Destroy { file }    => run_destroy(file),
        Command::Eval { eval_label, file } => run_eval(eval_label, file),
        Command::Cost { file: _ }    => stub("cost",    7),
        Command::Chat { agent, file } => run_chat(agent, file),
    }
}

fn run_validate(file: PathBuf, verbose: u8) -> Result<()> {
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
    let source = std::fs::read_to_string(&file)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    let source_dir = file.parent().unwrap_or_else(|| std::path::Path::new("."));

    // Resolve the output dir: explicit --output-dir wins; otherwise
    // default to the source file's directory. Canonicalised to an
    // absolute path so the emitted Compose YAML embeds an unambiguous
    // host-side mount path (works regardless of `docker compose`'s CWD).
    let output_dir: PathBuf = match output_dir {
        Some(p) => p,
        None    => source_dir.to_path_buf(),
    };
    let output_dir = std::fs::canonicalize(&output_dir).map_err(|e| {
        anyhow::anyhow!("output dir `{}` doesn't exist or unreadable: {e}",
            output_dir.display())
    })?;

    let t_total_start = std::time::Instant::now();

    // ── Stage 1: validate (parse + checker) ──
    let t = std::time::Instant::now();
    let validate_result = aislang_core::validate(&source);
    let t_validate = t.elapsed();
    let (project, mut warnings) = match validate_result {
        Ok(v) => v,
        Err(err) => {
            render_validate_error(&source, &err);
            return Err(anyhow::anyhow!("(see error above)"));
        }
    };

    // ── Stage 2: catalog ID validation ──
    let t = std::time::Instant::now();
    let cat_result = aislang_resolver::check_against_bundled_catalog(&project);
    let t_catalog = t.elapsed();
    match cat_result {
        Ok(cat_warnings) => warnings.extend(cat_warnings),
        Err(e) => return Err(anyhow::anyhow!("catalog error: {e}")),
    }

    // ── Stage 3: resolve (model selection + prompt hashing) ──
    let t = std::time::Instant::now();
    let resolved = aislang_resolver::resolve_against_bundled_catalog(&project, source_dir)
        .map_err(|e| anyhow::anyhow!("resolve error: {e}"))?;
    let t_resolve = t.elapsed();

    // ── Stage 4: lockfile reconcile (unless --no-lock) ──
    // Lockfile and litellm config both live under `output_dir`.
    let t = std::time::Instant::now();
    let lock_path = output_dir.join("aislang.lock");
    let lockfile_status: &str = if no_lock {
        "skipped (--no-lock)"
    } else {
        let existing = aislang_resolver::lockfile::read(&lock_path)
            .map_err(|e| anyhow::anyhow!("lockfile read error: {e}"))?;
        let diff = aislang_resolver::lockfile::reconcile(
            &source,
            existing.as_ref(),
            &resolved.catalog_version,
            &resolved.prompt_hashes,
        );
        match diff {
            aislang_resolver::LockfileDiff::Match => {
                "match — reused cached bindings"
            }
            aislang_resolver::LockfileDiff::Fresh => {
                let new_lock = aislang_resolver::Lockfile::build(&source, &resolved);
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
                let new_lock = aislang_resolver::Lockfile::build(&source, &resolved);
                aislang_resolver::lockfile::write(&lock_path, &new_lock)
                    .map_err(|e| anyhow::anyhow!("lockfile write error: {e}"))?;
                "stale — rewrote lockfile"
            }
        }
    };
    let t_lock = t.elapsed();

    // ── Stage 5: emit Compose + LiteLLM config; write the latter ──
    let t = std::time::Instant::now();
    let catalog = aislang_catalog::bundled()
        .map_err(|e| anyhow::anyhow!("catalog load error: {e}"))?;
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
    let t_emit = t.elapsed();

    let t_total = t_total_start.elapsed();

    // ── Output: Compose YAML on stdout (pipeable into `docker compose -f - up`) ──
    print!("{}", artifacts.docker_compose);

    // ── Generated-file paths on stderr (so stdout stays pipe-clean) ──
    eprintln!();
    eprintln!("generated:");
    eprintln!("  litellm config:  {}", litellm_config_path.display());
    eprintln!("  env example:     {}", env_example_path.display());
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
        let graph = aislang_core::checker::graph_summary(&project).ok();
        let combined = serde_json::json!({
            "ast":        &project,
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
    let source = std::fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    let source_dir = file.parent().unwrap_or_else(|| Path::new("."));

    let output_dir_buf: PathBuf = match output_dir {
        Some(p) => p.to_path_buf(),
        None    => source_dir.to_path_buf(),
    };
    let output_dir = std::fs::canonicalize(&output_dir_buf).map_err(|e| {
        anyhow::anyhow!("output dir `{}` doesn't exist or unreadable: {e}",
            output_dir_buf.display())
    })?;

    // Stage 1: validate.
    let (project, mut warnings) = match aislang_core::validate(&source) {
        Ok(v) => v,
        Err(err) => {
            render_validate_error(&source, &err);
            return Err(anyhow::anyhow!("(see error above)"));
        }
    };
    // Stage 2: catalog ID validation.
    match aislang_resolver::check_against_bundled_catalog(&project) {
        Ok(cat_warnings) => warnings.extend(cat_warnings),
        Err(e) => return Err(anyhow::anyhow!("catalog error: {e}")),
    }
    // Stage 3: resolve.
    let resolved = aislang_resolver::resolve_against_bundled_catalog(&project, source_dir)
        .map_err(|e| anyhow::anyhow!("resolve error: {e}"))?;
    // Stage 4: lockfile.
    let lock_path = output_dir.join("aislang.lock");
    let lock_path_out: Option<PathBuf> = if no_lock {
        None
    } else {
        let existing = aislang_resolver::lockfile::read(&lock_path)
            .map_err(|e| anyhow::anyhow!("lockfile read error: {e}"))?;
        let diff = aislang_resolver::lockfile::reconcile(
            &source, existing.as_ref(), &resolved.catalog_version,
            &resolved.prompt_hashes,
        );
        match diff {
            aislang_resolver::LockfileDiff::Match => {}
            aislang_resolver::LockfileDiff::Fresh
            | aislang_resolver::LockfileDiff::Stale { .. } => {
                let new_lock = aislang_resolver::Lockfile::build(&source, &resolved);
                aislang_resolver::lockfile::write(&lock_path, &new_lock)
                    .map_err(|e| anyhow::anyhow!("lockfile write error: {e}"))?;
            }
        }
        Some(lock_path)
    };
    // Stage 5: emit + write supporting files.
    let catalog = aislang_catalog::bundled()
        .map_err(|e| anyhow::anyhow!("catalog load error: {e}"))?;
    let artifacts = aislang_compose::emit(&resolved, &catalog, &output_dir)
        .map_err(|e| anyhow::anyhow!("compose emit error: {e}"))?;
    let litellm_config_path = output_dir.join(aislang_compose::LITELLM_CONFIG_FILENAME);
    std::fs::write(&litellm_config_path, &artifacts.litellm_config)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", litellm_config_path.display()))?;
    let env_example_path = output_dir.join(aislang_compose::ENV_EXAMPLE_FILENAME);
    std::fs::write(&env_example_path, &artifacts.env_example)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", env_example_path.display()))?;

    if verbose >= 1 && !warnings.is_empty() {
        eprintln!("warnings:");
        for w in &warnings {
            eprintln!("  - {}", w.message);
        }
    }

    Ok(PreparedStack {
        artifacts,
        output_dir,
        litellm_config_path,
        env_example_path,
        lock_path: lock_path_out,
    })
}

/// `aislang apply <file>` — plan, then `docker compose up -d`.
fn run_apply(file: PathBuf) -> Result<()> {
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
    eprintln!();
    eprintln!("stack is up. next steps:");
    if let Some(agent) = first_agent_label_in_file(&file) {
        eprintln!("  chat:      aislang chat {agent} {}", file.display());
    }
    eprintln!("  teardown:  aislang destroy {}", file.display());
    Ok(())
}

/// `aislang destroy <file>` — locate the stack.yml emitted by apply
/// and run `docker compose down -v`.
fn run_destroy(file: PathBuf) -> Result<()> {
    let source_dir = file.parent().unwrap_or_else(|| Path::new("."));
    let output_dir = std::fs::canonicalize(source_dir).map_err(|e| {
        anyhow::anyhow!("source directory `{}` not readable: {e}",
            source_dir.display())
    })?;
    let stack_path = output_dir.join(aislang_compose::STACK_YAML_FILENAME);
    if !stack_path.exists() {
        return Err(anyhow::anyhow!(
            "no stack.yml at `{}` — run `aislang apply {}` first \
             (or the stack was already destroyed)",
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
    eprintln!();
    eprintln!("stack torn down — containers, networks, and volumes removed.");
    eprintln!("the stack.yml file is preserved; remove it manually if desired:");
    eprintln!("  rm {}", stack_path.display());
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
// `aislang chat` — Monday of week 6 (stub) → Wednesday (live LiteLLM loop)
// ============================================================================

/// Open an interactive chat session against a declared agent.
///
/// Flow:
///   1. Read + validate the project source. Surface parse / type-check
///      errors via the source-aware diagnostic pipeline so the UX
///      matches `aislang validate`.
///   2. Find the agent by label (with Levenshtein "did you mean…?"
///      suggestions on misses — same UX as cross-resource refs).
///   3. Resolve the agent's `model.label` reference and `prompt.label`
///      reference into the concrete bindings the runtime needs.
///   4. Load the system-prompt file's contents. If `aislang.lock` is
///      present in the source directory and carries a hash for this
///      prompt, verify it — otherwise read without verification (the
///      user may have skipped lockfile creation via `--no-lock` at
///      plan time).
///   5. Open a `Session` against the compose-default LiteLLM URL.
///   6. Loop: read one line from stdin, run a turn, print the
///      assistant message. `/exit` or EOF cleanly closes.
///
/// The interactive loop is async (LiteLLM is HTTP-backed via reqwest).
/// We spin up a single-thread tokio runtime on entry and `block_on`
/// the whole session — keeps the rest of the CLI sync.
fn run_chat(agent_label: String, file: PathBuf) -> Result<()> {
    // ── 1. validate ──
    let source = std::fs::read_to_string(&file)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    let (project, _warnings) = match aislang_core::validate(&source) {
        Ok(v) => v,
        Err(err) => {
            render_validate_error(&source, &err);
            return Err(anyhow::anyhow!("(see error above)"));
        }
    };

    // ── 2. find agent ──
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

    // ── 3. resolve model + prompt refs ──
    let model_label  = agent.model.label.clone();
    let prompt_label = agent.system_prompt.label.clone();
    let prompt_block = project.prompts.iter()
        .find(|p| p.label == prompt_label)
        .ok_or_else(|| anyhow::anyhow!(
            "agent `{agent_label}` references prompt `{prompt_label}` \
             which doesn't exist in {}", file.display()))?;

    // Prompt file path is recorded relative to the source file's dir.
    let source_dir   = file.parent().unwrap_or_else(|| std::path::Path::new("."));
    let prompt_path  = source_dir.join(&prompt_block.file);

    // ── 4. load + verify prompt ──
    // If aislang.lock exists in source_dir and carries a hash for
    // this prompt label, verify it. Missing lockfile = no verification.
    let lock_path = source_dir.join("aislang.lock");
    let expected_hash: Option<String> =
        match aislang_resolver::lockfile::read(&lock_path) {
            Ok(Some(lock)) => lock.prompt_hashes.get(&prompt_label).cloned(),
            _              => None,
        };
    let system_prompt = aislang_runtime::load_system_prompt(
        &prompt_path,
        expected_hash.as_deref(),
    ).map_err(|e| anyhow::anyhow!("{e}"))?;

    // ── 5. collect MCP server specs the CLI must spawn for this agent ──
    // For each ref in `agent.tools[]`, find the matching mcp_server
    // block, capture its image + mount, AND resolve its declared
    // secrets into env pairs (week 7 Fri, Decisions 8/9). Resolution
    // failures here surface BEFORE we spin up the tokio runtime so
    // the error path is sync.
    use aislang_runtime::SpawnConfig;
    let mut mcp_specs: Vec<(String, SpawnConfig)> = Vec::new();
    for tool_ref in &agent.tools {
        // v0.1 only supports `mcp_server.X` tools (per spec §4.6).
        if tool_ref.kind != aislang_core::ast::RefKind::McpServer {
            return Err(anyhow::anyhow!(
                "agent `{agent_label}.tools` ref `{}` has kind `{:?}` — \
                 v0.1 only supports `mcp_server` tool refs",
                tool_ref.label, tool_ref.kind));
        }
        let mcp = project.mcp_servers.iter()
            .find(|m| m.label == tool_ref.label)
            .ok_or_else(|| anyhow::anyhow!(
                "agent `{agent_label}` references mcp_server `{}` \
                 which doesn't exist in {}",
                tool_ref.label, file.display()))?;
        // Canonicalise the mount path (StdioChild's docker -v needs absolute).
        let mount = mcp.mount.as_ref().map(|p| {
            // Resolve relative to the source file's directory, matching
            // how aislang plan handles paths.
            let resolved = source_dir.join(p);
            std::fs::canonicalize(&resolved).unwrap_or(resolved)
        });
        // Resolve mcp.secrets[] into env pairs (Decision 9: by
        // construction — only secrets the user EXPLICITLY listed in
        // this server's `secrets = [...]` field reach this child's
        // env. No global env leakage; no other secret blocks visible).
        let env = resolve_mcp_secrets(&project, mcp, source_dir)
            .map_err(|e| anyhow::anyhow!(
                "secret resolution failed for mcp_server `{}`: {e}",
                mcp.label))?;
        mcp_specs.push((mcp.label.clone(), SpawnConfig {
            image: format_image(&mcp.image),
            args:  Vec::new(),
            mount,
            env,
        }));
    }

    // ── 6. resolve model + build per-run budget config (week 7 Wed) ──
    // The session needs catalog-derived per-1k-token prices to compute
    // running cost. We run the resolver inline (same logic as `plan`)
    // to find the primary `ModelId` for this agent's model, then look
    // up its catalog entry. Resolution failures bubble — same UX as
    // `aislang plan` on the same source.
    use aislang_runtime::BudgetConfig;
    let budget_cfg: Option<BudgetConfig> = match project.budget.as_ref() {
        Some(b) => {
            let resolved = aislang_resolver::resolve_against_bundled_catalog(
                &project, source_dir,
            ).map_err(|e| anyhow::anyhow!("resolve error: {e}"))?;
            let model_id = resolved.model_bindings.get(&model_label)
                .ok_or_else(|| anyhow::anyhow!(
                    "model `{model_label}` has no resolved binding — \
                     resolver returned an empty set?"))?;
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
            })
        }
        None => None,
    };

    // ── 7. open per-day spend tracker (week 7 Thu) ─────────────
    // Only attach a tracker when a budget block is present — there's
    // no point persisting spend for projects that don't declare
    // caps. Failures to open the tracker (missing $HOME, corrupt
    // file, future schema_version) bubble; the CLI surfaces the
    // error and the user fixes the file.
    let tracker = match budget_cfg.as_ref() {
        Some(_) => Some(aislang_runtime::SpendTracker::open_default(&project.name)
            .map_err(|e| anyhow::anyhow!("open spend tracker: {e}"))?),
        None => None,
    };

    // ── 8. open session, run interactive loop ──
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;
    rt.block_on(chat_interactive(
        &agent_label,
        &model_label,
        system_prompt,
        mcp_specs,
        budget_cfg,
        tracker,
    ))
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

/// The async interactive loop. Separate from `run_chat` to keep the
/// sync arg-parsing + resolution phase out of the tokio runtime.
async fn chat_interactive(
    agent_label:   &str,
    model_label:   &str,
    system_prompt: String,
    mcp_specs:     Vec<(String, aislang_runtime::SpawnConfig)>,
    budget_cfg:    Option<aislang_runtime::BudgetConfig>,
    tracker:       Option<aislang_runtime::SpendTracker>,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    // ── Spawn MCP children + list their tools to populate the registry ──
    let mut mcp_clients = Vec::with_capacity(mcp_specs.len());
    let mut server_tools: Vec<(String, Vec<aislang_runtime::ToolSpec>)> = Vec::new();
    for (label, cfg) in &mcp_specs {
        let child = aislang_runtime::StdioChild::spawn(cfg).await
            .map_err(|e| anyhow::anyhow!(
                "spawn MCP server `{label}` ({}): {e}", cfg.image))?;
        let tools = child.list_tools().await
            .map_err(|e| anyhow::anyhow!(
                "list tools from MCP server `{label}`: {e}"))?;
        eprintln!("  mcp server `{label}` connected ({} tool{})",
            tools.len(), if tools.len() == 1 { "" } else { "s" });
        server_tools.push((label.clone(), tools));
        mcp_clients.push(child);
    }

    let tool_registry = aislang_runtime::ToolRegistry::from_servers(&server_tools);
    let tool_count    = tool_registry.len();

    let session = {
        let s = aislang_runtime::Session::open_with_tools(
            model_label,
            system_prompt,
            None, // default http://127.0.0.1:4000
            mcp_clients,
            tool_registry,
        ).map_err(|e| anyhow::anyhow!("open session: {e}"))?;
        let s = match budget_cfg {
            Some(b) => s.with_budget(b),
            None    => s,
        };
        match tracker {
            Some(t) => s.with_tracker(t),
            None    => s,
        }
    };

    // Banner — stderr so stdout stays pipe-clean for the model output.
    eprintln!("aislang chat — agent `{agent_label}` (model alias `{model_label}`)");
    eprintln!("connected to LiteLLM at {}", aislang_runtime::LiteLlmClient::DEFAULT_URL);
    if tool_count > 0 {
        eprintln!("tools: {tool_count} available across {} MCP server{}",
            mcp_specs.len(), if mcp_specs.len() == 1 { "" } else { "s" });
    }
    if session.has_budget() {
        eprintln!("budget: per-run cap enforcement enabled");
    }
    eprintln!("type your message and press Enter; `/exit` or Ctrl-D ends the session.");
    eprintln!();

    // Read one line at a time from stdin.
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    let exit_reason = loop {
        eprint!("> ");
        use std::io::Write;
        let _ = std::io::stderr().flush();

        let line = match reader.next_line().await {
            Ok(Some(l)) => l,
            Ok(None)    => break "(end of input)",
            Err(e)      => {
                // Print AFTER shutting down MCP children to leave a
                // clean host state.
                session.shutdown().await.ok();
                return Err(anyhow::anyhow!("stdin read error: {e}"));
            }
        };
        let line = line.trim();
        if line.is_empty() { continue; }
        if line == "/exit" { break "(/exit — session ended)"; }

        match session.turn(line).await {
            Ok(reply) => println!("{reply}"),
            Err(e)    => {
                eprintln!("aislang chat: {e}");
                continue;
            }
        }
    };

    // Clean shutdown — kill MCP children.
    eprintln!("{exit_reason}");
    session.shutdown().await
        .map_err(|e| anyhow::anyhow!("shutdown MCP children: {e}"))?;
    Ok(())
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
    let counts: [(&str, usize); 8] = [
        ("budget",      project.budget.is_some() as usize),
        ("models",      project.models.len()),
        ("prompts",     project.prompts.len()),
        ("mcp_servers", project.mcp_servers.len()),
        ("agents",      project.agents.len()),
        ("evals",       project.evals.len()),
        ("secrets",     project.secrets.len()),
        ("deploys",     project.deploys.len()),
    ];
    for (kind, count) in counts {
        if count > 0 {
            eprintln!("  {kind:<12}{count}");
        }
    }
}

fn stub(cmd: &str, week: u8) -> Result<()> {
    println!("aislang {cmd}: not implemented yet (scheduled for week {week} — see PLAN-90-days.md)");
    Ok(())
}

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

    let mut all_passed = true;
    for eval in evals {
        let report = rt.block_on(run_eval_inner(&project, eval, source_dir))?;
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
    }

    if all_passed { Ok(()) } else {
        Err(anyhow::anyhow!(
            "one or more evals failed their threshold (see reports above)"))
    }
}

/// Async: open a Session for one eval, run the dataset, build the
/// EvalReport. Mirrors `chat_interactive`'s session-construction
/// path so the agent runs exactly as it would for `aislang chat`.
async fn run_eval_inner(
    project:    &aislang_core::ast::Project,
    eval:       &aislang_core::ast::Eval,
    source_dir: &Path,
) -> Result<aislang_runtime::EvalReport> {
    use aislang_runtime::SpawnConfig;

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

    eprintln!("eval `{}`: running {} cases against agent `{}`...",
        eval.label, dataset.len(), agent.label);

    let mut case_results: Vec<aislang_runtime::CaseResult> =
        Vec::with_capacity(dataset.len());
    for case in &dataset {
        let t0 = std::time::Instant::now();
        let turn_result = session.turn(&case.input).await;
        let latency_ms = t0.elapsed().as_millis() as u64;
        let actual = match turn_result {
            Ok(s) => s,
            // A per-case turn failure (network, MCP, budget abort)
            // is recorded as an empty actual + score 0. The eval
            // continues so the user sees the FULL run, not just
            // the first failure.
            Err(e) => format!("(turn errored: {e})"),
        };
        let score = aislang_runtime::metric_score(eval.metric, &case.expected, &actual)
            .map_err(|e| anyhow::anyhow!("metric scoring failed: {e}"))?;
        case_results.push(aislang_runtime::CaseResult {
            id:       case.id.clone(),
            input:    case.input.clone(),
            expected: case.expected.clone(),
            actual,
            score,
            latency_ms,
        });
    }

    // ── 8. aggregate + threshold compare ──
    let scores: Vec<f64> = case_results.iter().map(|c| c.score).collect();
    let aggregate_score = aislang_runtime::eval_aggregate(eval.aggregator, &scores);
    let pass = aggregate_score >= eval.threshold.ratio;

    // ── 9. clean shutdown of MCP children ──
    session.shutdown().await
        .map_err(|e| anyhow::anyhow!("shutdown MCP children: {e}"))?;

    Ok(aislang_runtime::EvalReport {
        eval_label:      eval.label.clone(),
        dataset_path:    dataset_path.display().to_string(),
        metric:          format!("{:?}", eval.metric).to_lowercase(),
        aggregator:      format!("{:?}", eval.aggregator).to_lowercase(),
        threshold:       eval.threshold.ratio,
        aggregate_score,
        pass,
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
                    scope:  vec![Ref { kind: RefKind::McpServer, label: "alpha".into() }],
                },
                Secret {
                    label:  "b_only".into(),
                    source: "env://TEST_SCOPE_B_ONLY".into(),
                    scope:  vec![Ref { kind: RefKind::McpServer, label: "beta".into()  }],
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
}

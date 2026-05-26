//! `aislang` — declarative AI infrastructure CLI.
//!
//! v0.1 subcommand surface (most are stubs until their scheduled week).
//! See PLAN-90-days.md for the implementation schedule.

use std::path::PathBuf;
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
        /// isn't on PATH (prints "skipped" — exit 0). v0.2 will
        /// extend with `up -d` + health-poll + real LLM call.
        #[arg(long)]
        smoke_test: bool,
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
    Eval {
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
        Command::Plan { file, verbose, no_lock, output_dir, smoke_test } =>
            run_plan(file, verbose, no_lock, output_dir, smoke_test),
        Command::Apply { file: _ }   => stub("apply",   6),
        Command::Destroy { file: _ } => stub("destroy", 10),
        Command::Eval { file: _ }    => stub("eval",    8),
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
    file:       PathBuf,
    verbose:    u8,
    no_lock:    bool,
    output_dir: Option<PathBuf>,
    smoke_test: bool,
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

    if smoke_test {
        eprintln!();
        eprintln!("smoke test:");
        run_smoke_test(&artifacts.docker_compose)?;
    }

    Ok(())
}

/// `--smoke-test`: hand the generated compose YAML to `docker compose
/// config -q`, report whether Docker accepts the structure.
///
/// Tiered degradation:
///   1. `docker` not on PATH        → skipped, exit 0
///   2. `docker compose config -q`  → run; failure surfaces stderr
///                                     to the user and propagates as
///                                     an error
///
/// v0.2 extends with: `docker compose up -d` + health-poll + real
/// `/v1/chat/completions` curl + `docker compose down`. The polling
/// + cleanup machinery is non-trivial; v0.1 stops at the config-
/// validation tier (which already catches every YAML-shape bug seen
/// in week-5 — most recently the AISLANG_CATALOG_VERSION date-shape
/// regression).
fn run_smoke_test(compose_yaml: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // Tier 1: docker CLI present?
    let docker_present = Command::new("docker").arg("--version").output();
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

    // Tier 2: pipe the compose YAML to `docker compose config -q`.
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
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("  compose validate:  ✗ Docker rejected the YAML:");
        for line in stderr.lines() {
            eprintln!("      {line}");
        }
        return Err(anyhow::anyhow!(
            "docker compose config -q rejected the generated YAML \
             (exit {:?}); see lines above",
            output.status.code()
        ));
    }

    // Tier 3 (`up -d` + health-poll + LLM call + `down`): v0.2.
    eprintln!("  up/down + LLM call: deferred to v0.2");

    Ok(())
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

    // ── 5+6. open session, run interactive loop ──
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("init tokio runtime: {e}"))?;
    rt.block_on(chat_interactive(
        &agent_label,
        &model_label,
        system_prompt,
    ))
}

/// The async interactive loop. Separate from `run_chat` to keep the
/// sync arg-parsing + resolution phase out of the tokio runtime.
async fn chat_interactive(
    agent_label:   &str,
    model_label:   &str,
    system_prompt: String,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let session = aislang_runtime::Session::open(
        model_label,
        system_prompt,
        None, // default http://localhost:4000
    ).map_err(|e| anyhow::anyhow!("open session: {e}"))?;

    // Banner — stderr so stdout stays pipe-clean for the model output.
    eprintln!("aislang chat — agent `{agent_label}` (model alias `{model_label}`)");
    eprintln!("connected to LiteLLM at {}", aislang_runtime::LiteLlmClient::DEFAULT_URL);
    eprintln!("type your message and press Enter; `/exit` or Ctrl-D ends the session.");
    eprintln!();

    // Read one line at a time from stdin. tokio's stdin handle
    // doesn't expose AsyncBufRead by default; wrap it.
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    loop {
        eprint!("> ");
        // Flush eprint! before awaiting stdin — otherwise the prompt
        // appears AFTER the user types, which is jarring.
        use std::io::Write;
        let _ = std::io::stderr().flush();

        let line = match reader.next_line().await {
            Ok(Some(l)) => l,
            Ok(None)    => {
                // EOF (Ctrl-D or piped input ran out).
                eprintln!("(end of input)");
                return Ok(());
            }
            Err(e) => return Err(anyhow::anyhow!("stdin read error: {e}")),
        };
        let line = line.trim();
        if line.is_empty() { continue; }
        if line == "/exit" {
            eprintln!("(/exit — session ended)");
            return Ok(());
        }

        // One round-trip.
        match session.turn(line).await {
            Ok(reply) => println!("{reply}"),
            Err(e)    => {
                eprintln!("aislang chat: {e}");
                // Don't abort the session on a single failed turn —
                // user can retry. Network blips, rate limits, etc.
                continue;
            }
        }
    }
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
}

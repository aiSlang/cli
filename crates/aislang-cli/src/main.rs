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
        /// aislang.lock). Defaults to the source file's directory. The
        /// emitted Compose YAML embeds absolute paths so the user can
        /// `docker compose -f - up` from anywhere.
        #[arg(short = 'o', long)]
        output_dir: Option<PathBuf>,
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
    Chat {
        /// Agent label (e.g. `support`)
        agent: String,
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
        Command::Plan { file, verbose, no_lock, output_dir } =>
            run_plan(file, verbose, no_lock, output_dir),
        Command::Apply { file: _ }   => stub("apply",   6),
        Command::Destroy { file: _ } => stub("destroy", 10),
        Command::Eval { file: _ }    => stub("eval",    8),
        Command::Cost { file: _ }    => stub("cost",    7),
        Command::Chat { agent: _ }   => stub("chat",    6),
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

    Ok(())
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

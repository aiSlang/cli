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
    /// Resolve models + emit the deployment plan (no side effects)
    Plan {
        file: PathBuf,
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
        Command::Validate { file, verbose } => run_validate(file, verbose),
        Command::Plan { file: _ }    => stub("plan",    4),
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
            // Source-aware error rendering (week-3 Thursday).
            //
            // For Parse(Vec<ParseError>): grammar-level errors carry
            // span info → render with line:col + 2-3-line snippet +
            // caret pointer. Converter-level errors carry span = None
            // → render as plain message.
            //
            // For Type/UnresolvedRef errors: no AST-side span info
            // tracked yet (per-AST-node span propagation is a future-
            // week enhancement) → plain message.
            match &validate_err {
                aislang_core::ValidateError::Parse(errors) => {
                    for err in errors {
                        match &err.span {
                            Some(span) => {
                                eprint!("{}", aislang_core::diagnostic::render_with_source(
                                    &source,
                                    span.clone(),
                                    aislang_core::diagnostic::Severity::Error,
                                    &err.message,
                                ));
                            }
                            None => eprintln!("aislang: parse error: {}", err.message),
                        }
                    }
                }
                other => eprintln!("aislang: {other}"),
            }

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

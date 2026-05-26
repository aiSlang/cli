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

    match aislang_core::validate(&source) {
        Ok((project, mut warnings)) => {
            // Catalog ID validation lives in the sibling `aislang-resolver`
            // crate (architecture decision: avoids aislang-core ↔
            // aislang-catalog dep cycle — see PLAN §Week-1 dep-graph note).
            // CLI is the natural place to chain core's parse+typecheck
            // with resolver's catalog-check, combining their warnings.
            match aislang_resolver::check_against_bundled_catalog(&project) {
                Ok(cat_warnings) => warnings.extend(cat_warnings),
                Err(e) => return Err(anyhow::anyhow!("catalog error: {e}")),
            }

            // Success line always goes to stdout (machine-readable surface).
            println!("OK: {} parsed and type-checked", file.display());

            // Warnings come right after the OK line, on stderr, prefixed
            // with `warnings:`. Exit code remains 0 — warnings inform but
            // don't fail (see PLAN §Locked design decisions).
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
            }
            if verbose >= 2 {
                eprintln!();
                eprintln!("AST (Debug):");
                eprintln!("{project:#?}");
            }
            if verbose >= 3 {
                eprintln!();
                eprintln!("AST (JSON IR):");
                match serde_json::to_string_pretty(&project) {
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

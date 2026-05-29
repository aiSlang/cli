//! Build script — injects git HEAD short-hash + dirty-tree marker into
//! the binary so `aislang --version` answers "exactly what code is
//! running?" without ambiguity during v0.2+ daily-build churn.
//!
//! Compile-time-only; zero runtime cost.  Emits two `cargo:rustc-env`
//! settings that the clap `#[command(version = …)]` attribute splices
//! into the final version string at the `main.rs` call site.
//!
//! ## Output format
//!
//! Combined with `CARGO_PKG_VERSION` in `main.rs`, the final string is:
//!
//! ```text
//! aislang 0.2.0-dev (c24861e)
//! aislang 0.2.0-dev (c24861e-dirty)        ← uncommitted local changes
//! aislang 0.2.0-dev (unknown)              ← built outside a git checkout
//!                                              (e.g. from a release tarball)
//! ```
//!
//! The `(unknown)` shape covers the release-pipeline case: GitHub
//! Actions' `actions/checkout@v4` DOES keep `.git/`, so release builds
//! get a real hash; but downstream consumers extracting a tarball into
//! their own build infrastructure (no `.git/` present) shouldn't see a
//! build failure — they get `unknown` and a working binary.
//!
//! ## Rebuild triggers
//!
//! `cargo:rerun-if-changed=.git/HEAD` + `.git/index` ensure cargo
//! re-runs this script when commits land (HEAD moves) OR when the
//! working tree's staged/index state changes (which the dirty check
//! cares about).  This is the standard pattern recommended in the
//! Cargo book.

use std::process::Command;

fn main() {
    // Resolve `.git/` location.  We assume the typical layout (build.rs
    // at <crate>/build.rs, .git at <workspace_root>/.git) and let the
    // git CLI handle the actual discovery via its built-in upward
    // walk — that's more robust than computing paths ourselves.
    //
    // The two `git rev-parse` + `git status` invocations are cheap (<5ms
    // total on a warm git index) and only re-run when HEAD or index
    // change (see the cargo:rerun-if-changed directives at the bottom).

    let hash = run_git(&["rev-parse", "--short", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());

    // `--porcelain=v1` produces machine-stable output.  Empty stdout
    // means a clean working tree; any line of output means there are
    // changes the user can see via `git status`.  Build-script perspective
    // doesn't care WHICH files are dirty — only that *something* is.
    let dirty_suffix = match run_git(&["status", "--porcelain=v1", "--untracked-files=no"]) {
        Some(out) if !out.is_empty() => "-dirty",
        _ => "",
    };

    // Splice into rustc's env so `env!("AISLANG_GIT_HASH")` in main.rs
    // sees these at compile time.
    println!("cargo:rustc-env=AISLANG_GIT_HASH={hash}");
    println!("cargo:rustc-env=AISLANG_GIT_DIRTY={dirty_suffix}");

    // Cargo re-runs build.rs by default on every build; the rerun-if-*
    // directives narrow that to "only when these inputs change," which
    // makes incremental rebuilds fast.  We re-trigger when:
    //   • HEAD moves (commit / checkout / reset)        → hash changes
    //   • index updates (git add / stash apply)         → dirty changes
    //   • the build script itself changes                → cargo default
    //
    // If `.git/HEAD` is absent (release tarball), the directives are
    // still valid — cargo treats missing-tracked-files as "always rerun,"
    // which is fine because the hash resolution falls back to "unknown".
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
}

/// Run `git <args>` from the build-script's CWD and return trimmed
/// stdout.  Returns `None` on any failure (git not on PATH, not a
/// git checkout, command failed) — the caller decides how to fall
/// back.  Never panics: a broken git environment shouldn't break the
/// build.
fn run_git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    Some(s.trim().to_string())
}

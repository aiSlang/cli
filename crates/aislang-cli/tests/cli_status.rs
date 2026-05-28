//! Integration tests for `aislang status` — week 10 Thu.
//!
//! Categories covered:
//!   1. **State absent**: exit 1 with "not applied" message.
//!   2. **State torn-down**: exit 1 with "torn down" hint.
//!   3. **State applied + no docker**: exit 2 (docker-unavailable
//!      takes precedence per Decision 28).
//!   4. **--spend section**: appends today's total from the
//!      isolated spend-file path (env override).
//!   5. **--model-spend bare (no =value)**: appends "all models"
//!      breakdown.
//!   6. **--model-spend=<id> filtered**: only the named model.
//!   7. **--spend --model-spend combined**: both sections present.
//!
//! We can't reliably test the docker-up live path in CI (needs a
//! daemon + image pulls). That's the empirical smoke-test the user
//! ran end-to-end against their local stack.

use std::path::PathBuf;
use std::process::Command;

fn tempdir_with_project(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aislang-status-{}-{suffix}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("system.md"), "you are helpful").expect("write prompt");
    let src = r#"project "demo" {
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.01
  }
  model "fast" {
    requires    = [chat]
    context_min = 32k
    prefer      = [ollama.llama_3_1_8b]
    pin         = ollama.llama_3_1_8b
  }
  prompt "system" {
    file    = "./system.md"
    version = "1.0.0"
  }
  agent "support" {
    model         = model.fast
    system_prompt = prompt.system
    tools         = []
  }
}
"#;
    let ais_path = dir.join("demo.ais");
    std::fs::write(&ais_path, src).expect("write demo.ais");
    ais_path
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_aislang"))
}

/// `aislang status` invocation with an isolated HOME so the spend
/// file points at a tempdir, not the dev's real `~/.aislang/spend.json`.
fn bin_isolated(home: &std::path::Path) -> Command {
    let mut cmd = bin();
    cmd.env("HOME", home);
    // Force docker off PATH so the live `docker compose ps` query
    // never fires; we're testing the read paths only.
    cmd.env("PATH", "/nonexistent-aislang-test-path");
    cmd
}

/// Write a `.aislang/state.json` with the given `applied_at`.
fn write_state(dir: &std::path::Path, applied_at: Option<&str>) {
    let aislang_dir = dir.join(".aislang");
    std::fs::create_dir_all(&aislang_dir).expect("mkdir .aislang");
    let applied = applied_at
        .map(|s| format!("\"applied_at\":\"{s}\","))
        .unwrap_or_default();
    let json = format!(r#"{{
        "schema_version": 1,
        {applied}
        "source_hash":     "deadbeef",
        "catalog_version": "2026-05-25",
        "stack_yaml_path": "{}",
        "project_name":    "demo"
    }}"#, dir.join("stack.yml").display());
    std::fs::write(aislang_dir.join("state.json"), json).expect("write state");
}

/// Seed `<home>/.aislang/spend.json` with a specific today's spend
/// for the `demo` project. Uses the current UTC date to match what
/// the binary computes; matches the production schema verbatim.
fn write_spend(home: &std::path::Path, total_mu: i64, by_model: &[(&str, i64)]) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    // Replicate utc_date_from_epoch_secs by hand — depends on Howard
    // Hinnant's civil-from-days. For test purposes we want the SAME
    // date the binary will compute, so call its formatter via a
    // shell-out to ourselves… or just compute it inline here.
    // Inline form (good enough for testing — only valid for current
    // millennium):
    let days = secs / 86_400;
    let (y, m, d) = {
        // Quick civil_from_days copy. Same algorithm spend.rs uses.
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365;
        let y   = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe/4 - yoe/100);
        let mp  = (5 * doy + 2) / 153;
        let d   = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m   = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
        let y   = if m <= 2 { y + 1 } else { y };
        (y, m, d)
    };
    let date = format!("{y:04}-{m:02}-{d:02}");

    let by_model_json: String = by_model.iter()
        .map(|(k, v)| format!("\"{k}\":{v}"))
        .collect::<Vec<_>>()
        .join(",");
    let json = format!(r#"{{
        "schema_version": 1,
        "projects": {{
            "demo": {{
                "{date}": {{
                    "micro_units": {total_mu},
                    "by_model": {{{by_model_json}}}
                }}
            }}
        }}
    }}"#);

    let aislang_home = home.join(".aislang");
    std::fs::create_dir_all(&aislang_home).expect("mkdir home/.aislang");
    std::fs::write(aislang_home.join("spend.json"), json).expect("write spend.json");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test] fn status_with_no_state_exits_1_with_not_applied_hint() {
    let ais = tempdir_with_project("no-state");
    let dir = ais.parent().unwrap().to_path_buf();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let out = bin_isolated(&home).args(["status"]).arg(&ais).output()
        .expect("spawn");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1),
        "no-state must exit 1 (drift); got {:?}\nstderr:\n{}",
        out.status.code(), String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("not applied"),
        "stdout must say `not applied`; got:\n{stdout}");
    assert!(stdout.contains("aislang apply"),
        "stdout must hint at apply; got:\n{stdout}");
}

#[test] fn status_with_torn_down_state_exits_1() {
    let ais = tempdir_with_project("torn-down");
    let dir = ais.parent().unwrap().to_path_buf();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    write_state(&dir, None);   // applied_at: null

    let out = bin_isolated(&home).args(["status"]).arg(&ais).output()
        .expect("spawn");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("torn down"),
        "must show torn-down state; got:\n{stdout}");
    assert!(stdout.contains("catalog_version: 2026-05-25"),
        "torn-down view must still surface catalog_version; got:\n{stdout}");
}

#[test] fn status_with_applied_state_and_no_docker_exits_2() {
    // Decision 28: docker-unavailable on the stack section
    // dominates over generic drift. Without docker on PATH AND
    // an applied state, exit 2 (not 1).
    let ais = tempdir_with_project("applied-no-docker");
    let dir = ais.parent().unwrap().to_path_buf();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    write_state(&dir, Some("2026-05-28T12:00:00Z"));
    std::fs::write(dir.join("stack.yml"), "# placeholder").unwrap();

    let out = bin_isolated(&home).args(["status"]).arg(&ais).output()
        .expect("spawn");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(2),
        "docker-unavailable must exit 2; got {:?}\nstderr:\n{}",
        out.status.code(), String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("docker not on PATH"),
        "stdout must explain docker is missing; got:\n{stdout}");
    assert!(stdout.contains("applied_at:"),
        "applied state should still render; got:\n{stdout}");
}

#[test] fn status_spend_renders_total_with_cap_percentage() {
    let ais = tempdir_with_project("spend-total");
    let dir = ais.parent().unwrap().to_path_buf();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // 250000 µ-USD = $0.25 — 25 % of the $1 day cap from the
    // hardcoded .ais source above.
    write_spend(&home, 250_000, &[("anthropic.claude_haiku_4_5", 250_000)]);

    let out = bin_isolated(&home).args(["status", "--spend"]).arg(&ais).output()
        .expect("spawn");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("spend ("),
        "must render `spend (... UTC):` header; got:\n{stdout}");
    assert!(stdout.contains("total:") && stdout.contains("$0.25"),
        "total must show $0.25; got:\n{stdout}");
    assert!(stdout.contains("25.0% used"),
        "must compute % of day cap; got:\n{stdout}");
}

#[test] fn status_model_spend_bare_lists_all_models() {
    let ais = tempdir_with_project("model-spend-all");
    let dir = ais.parent().unwrap().to_path_buf();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    write_spend(&home, 300_000, &[
        ("anthropic.claude_haiku_4_5", 200_000),
        ("openai.gpt_4o_mini",         100_000),
    ]);

    let out = bin_isolated(&home).args(["status", "--model-spend"]).arg(&ais).output()
        .expect("spawn");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("spend by model"),
        "must use `spend by model` header for unfiltered view; got:\n{stdout}");
    assert!(stdout.contains("anthropic.claude_haiku_4_5"),
        "Haiku entry must be present; got:\n{stdout}");
    assert!(stdout.contains("openai.gpt_4o_mini"),
        "gpt-4o-mini entry must be present; got:\n{stdout}");
    // Sort-by-amount-descending: Haiku ($0.20) before gpt ($0.10).
    let haiku_pos = stdout.find("anthropic.claude_haiku_4_5").unwrap();
    let gpt_pos   = stdout.find("openai.gpt_4o_mini").unwrap();
    assert!(haiku_pos < gpt_pos,
        "rows must sort by amount descending (Haiku before gpt); got:\n{stdout}");
}

#[test] fn status_model_spend_filtered_shows_only_named_model() {
    let ais = tempdir_with_project("model-spend-filtered");
    let dir = ais.parent().unwrap().to_path_buf();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    write_spend(&home, 300_000, &[
        ("anthropic.claude_haiku_4_5", 200_000),
        ("openai.gpt_4o_mini",         100_000),
    ]);

    let out = bin_isolated(&home)
        .args(["status", "--model-spend=anthropic.claude_haiku_4_5"])
        .arg(&ais).output()
        .expect("spawn");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("spend for model `anthropic.claude_haiku_4_5`"),
        "must use filtered header; got:\n{stdout}");
    assert!(stdout.contains("$0.2"),
        "must show Haiku's $0.20 spend; got:\n{stdout}");
    // The other model MUST NOT appear in the filtered output.
    assert!(!stdout.contains("openai.gpt_4o_mini"),
        "filtered view must NOT mention the other model; got:\n{stdout}");
}

#[test] fn status_filtered_model_with_no_spend_today_shows_explicit_zero_message() {
    let ais = tempdir_with_project("model-spend-empty");
    let dir = ais.parent().unwrap().to_path_buf();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    write_spend(&home, 200_000, &[("anthropic.claude_haiku_4_5", 200_000)]);

    let out = bin_isolated(&home)
        .args(["status", "--model-spend=openai.gpt_4o_mini"])
        .arg(&ais).output()
        .expect("spawn");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no spend recorded for `openai.gpt_4o_mini`"),
        "filtered view with missing model must say so; got:\n{stdout}");
}

#[test] fn status_spend_and_model_spend_render_both_sections() {
    let ais = tempdir_with_project("combined");
    let dir = ais.parent().unwrap().to_path_buf();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    write_spend(&home, 100_000, &[("anthropic.claude_haiku_4_5", 100_000)]);

    let out = bin_isolated(&home)
        .args(["status", "--spend", "--model-spend"])
        .arg(&ais).output()
        .expect("spawn");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("total:") && stdout.contains("$0.1"),
        "total section must be present; got:\n{stdout}");
    assert!(stdout.contains("spend by model"),
        "model-spend section must be present; got:\n{stdout}");
    // Order: total section comes BEFORE by-model section.
    let total_pos = stdout.find("total:").unwrap();
    let bymdl_pos = stdout.find("spend by model").unwrap();
    assert!(total_pos < bymdl_pos,
        "total section must precede by-model section; got:\n{stdout}");
}

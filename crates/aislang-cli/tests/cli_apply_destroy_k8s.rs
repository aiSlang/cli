//! Integration tests for `aislang apply` + `aislang destroy` against
//! `deploy { target = kubernetes }` projects — Phase 2 Week 14 Thu.
//!
//! Mirrors `cli_apply_destroy.rs` (the v0.1 Compose-target tests) but
//! exercises the Decision 43.7/43.8 helm-shellout paths:
//!
//!   1. **Apply with helm absent** (`PATH=/nonexistent`): write
//!      manifests/ + chart/, print the manual `helm upgrade --install`
//!      command, exit 0.  Same graceful-degrade posture as Compose
//!      apply without docker.
//!   2. **Destroy with helm absent**: hard-fail with a clear "install
//!      Helm 3" message + a `kubectl delete -f manifests/` fallback
//!      hint.  Mirrors v0.1 destroy's docker-absent hard-fail.
//!   3. **Destroy idempotency**: re-running destroy after the chart
//!      was uninstalled flips on the v0.1 state-file mechanism + exits
//!      0 with the "nothing to destroy" message.
//!   4. **Apply with helm present** (`--ignored`): requires a real
//!      cluster + helm; runs locally with --ignored, NOT in CI.

use std::path::PathBuf;
use std::process::Command;

/// Tool-less k8s-target .ais.  No MCP, no docker — keeps the
/// no-helm graceful-degrade path entirely self-contained.
fn k8s_demo_path(suffix: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "aislang-apply-destroy-k8s-{}-{suffix}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("system.md"), "you are helpful").expect("write prompt");
    let src = r#"project "k8s-demo" {
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
  deploy "prod" {
    target = kubernetes
    expose { port = 8080  web_ui = true  ingress = "ui.example.com" }
  }
}
"#;
    let ais = dir.join("demo.ais");
    std::fs::write(&ais, src).expect("write demo.ais");
    (ais, dir)
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_aislang"))
}

/// Clobber PATH so neither `docker` nor `helm` resolve.  Lets the
/// graceful-degrade path of run_apply_k8s assert without needing
/// a real cluster.
fn bin_no_tools() -> Command {
    let mut cmd = bin();
    cmd.env("PATH", "/nonexistent-aislang-test-path");
    cmd
}

// ---------------------------------------------------------------------------
// apply — graceful degrade when helm is absent
// ---------------------------------------------------------------------------

#[test] fn apply_k8s_without_helm_writes_artifacts_and_prints_manual_command() {
    let (ais, dir) = k8s_demo_path("apply-no-helm");
    let out = bin_no_tools().args(["apply"]).arg(&ais).output()
        .expect("spawn aislang");

    let manifests_present = dir.join("manifests").join("agent-runtime.yaml").exists();
    let chart_yaml_present = dir.join("chart").join("Chart.yaml").exists();
    let values_yaml_present = dir.join("chart").join("values.yaml").exists();
    let openwebui_template_present = dir.join("chart").join("templates")
        .join("openwebui.yaml").exists();
    let ingress_template_present = dir.join("chart").join("templates")
        .join("ingress.yaml").exists();
    let status = out.status;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(status.success(),
        "apply must exit 0 even without helm; stderr:\n{stderr}");
    assert!(manifests_present, "manifests/agent-runtime.yaml must be written");
    assert!(chart_yaml_present, "chart/Chart.yaml must be written");
    assert!(values_yaml_present, "chart/values.yaml must be written");
    assert!(openwebui_template_present,
        "chart/templates/openwebui.yaml must be written (web_ui=true)");
    assert!(ingress_template_present,
        "chart/templates/ingress.yaml must be written (ingress set)");
    assert!(stderr.contains("helm not on PATH"),
        "stderr must explain helm is missing; got:\n{stderr}");
    assert!(stderr.contains("helm upgrade --install"),
        "stderr must print the manual helm command; got:\n{stderr}");
    assert!(stderr.contains("kubectl apply -f"),
        "stderr must also offer the kubectl fallback; got:\n{stderr}");
}

#[test] fn apply_k8s_lists_release_name_from_deploy_label() {
    // Decision 43.1: chart name + helm release name = deploy block label.
    // Our example uses `deploy "prod"`; the manual-command line must
    // reference `prod`, not "aislang" or some other generic name.
    let (ais, dir) = k8s_demo_path("apply-release-name");
    let out = bin_no_tools().args(["apply"]).arg(&ais).output()
        .expect("spawn aislang");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(out.status.success());
    assert!(stderr.contains("helm upgrade --install prod"),
        "manual command must use the deploy label as release name; got:\n{stderr}");
}

#[test] fn apply_k8s_state_file_not_written_when_helm_absent() {
    // Mirror of the Compose apply behaviour: without docker, no
    // `docker compose up -d` runs, so no state.json gets written.
    // Same posture here for helm.
    let (ais, dir) = k8s_demo_path("apply-no-state");
    let out = bin_no_tools().args(["apply"]).arg(&ais).output()
        .expect("spawn aislang");
    let state_present = dir.join(".aislang").join("state.json").exists();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(out.status.success());
    assert!(!state_present,
        ".aislang/state.json must NOT be written when helm absent + chart wasn't installed");
}

// ---------------------------------------------------------------------------
// destroy — hard fail when helm is absent (no state file)
// ---------------------------------------------------------------------------

#[test] fn destroy_k8s_no_state_no_chart_exits_cleanly() {
    // Pre-apply project: no chart/, no state.json.  Destroy should
    // exit 0 with a "nothing to destroy" hint (same idempotent
    // posture as v0.1 Compose destroy).
    let (ais, dir) = k8s_demo_path("destroy-pristine");
    let out = bin_no_tools().args(["destroy"]).arg(&ais).output()
        .expect("spawn aislang");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(out.status.success(),
        "destroy on pristine project must exit 0; stderr:\n{stderr}");
    assert!(stderr.contains("nothing to destroy"),
        "stderr must hint nothing to destroy; got:\n{stderr}");
}

#[test] fn destroy_k8s_with_chart_dir_but_no_helm_hard_fails() {
    // Simulate the post-apply-without-helm state: chart/ dir
    // exists but no state.json (apply gracefully degraded).
    // Without `--force` the idempotency check passes through
    // (state absent but chart present) → reaches the helm
    // shellout, which now reports helm absence as a hard failure.
    let (ais, dir) = k8s_demo_path("destroy-no-helm");
    // First, apply (graceful degrade) to create chart/.
    let apply_out = bin_no_tools().args(["apply"]).arg(&ais).output()
        .expect("spawn aislang apply");
    assert!(apply_out.status.success(), "apply pre-step failed: {}",
        String::from_utf8_lossy(&apply_out.stderr));

    // Now destroy WITHOUT helm on PATH.
    let out = bin_no_tools().args(["destroy"]).arg(&ais).output()
        .expect("spawn aislang destroy");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!out.status.success(),
        "destroy without helm must hard-fail when chart/ exists; stderr:\n{stderr}");
    assert!(stderr.contains("helm not on PATH"),
        "stderr must explain helm absence; got:\n{stderr}");
    assert!(stderr.contains("kubectl delete -f"),
        "stderr should suggest the kubectl fallback for manual cleanup; got:\n{stderr}");
}

// ---------------------------------------------------------------------------
// Live tests — require `helm` + a real cluster (kind / k3s / GKE)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires Helm 3 + a real K8s cluster; run with --ignored on kind"]
fn apply_k8s_with_helm_installs_chart() {
    // Live tier: with helm + kind on PATH, apply should install the
    // chart cleanly + write state.json.  Run locally with:
    //   cargo test -p aislang-cli --test cli_apply_destroy_k8s -- --ignored
    let (ais, dir) = k8s_demo_path("apply-live");
    let out = bin().args(["apply"]).arg(&ais).output()
        .expect("spawn aislang");
    let state_present = dir.join(".aislang").join("state.json").exists();

    // Clean up the cluster state regardless of test outcome.
    let _ = bin().args(["destroy"]).arg(&ais).output();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(out.status.success(), "live apply failed: {}",
        String::from_utf8_lossy(&out.stderr));
    assert!(state_present,
        "state.json must be written after successful helm install");
}

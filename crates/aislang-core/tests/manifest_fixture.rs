//! v0.3 Week 16 Mon — integration test pinning the on-disk
//! `examples/multi-module-demo/` fixture against the manifest loader.
//!
//! Two assertions:
//!
//!   1. `load_manifest(examples/multi-module-demo)` returns a manifest
//!      whose `[modules]` table matches the on-disk `aislang.toml`.
//!   2. `discover_ais_files(examples/multi-module-demo/agents)` finds
//!      every `.ais` file the manifest references.
//!
//! Together these pin the Mon contract: parser shape + discovery
//! walker + on-disk fixture all line up.  Tue Week 16 grows this with
//! cross-module ref resolution.

use std::path::PathBuf;

use aislang_core::manifest::{discover_ais_files, load_manifest};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..")
        .join("examples").join("multi-module-demo")
}

#[test] fn manifest_fixture_parses_with_expected_modules() {
    let m = load_manifest(&fixture_dir()).expect("load")
        .expect("examples/multi-module-demo/aislang.toml must exist");
    assert_eq!(m.project.name,    "multi-module-demo");
    assert_eq!(m.project.version, "0.1.0");

    let keys: Vec<&String> = m.modules.keys().collect();
    assert_eq!(keys, vec!["architect", "coder"]);

    assert_eq!(
        m.modules.get("architect"),
        Some(&PathBuf::from("agents/architect.ais")));
    assert_eq!(
        m.modules.get("coder"),
        Some(&PathBuf::from("agents/coder.ais")));
}

#[test] fn fixture_module_paths_resolve_to_real_files_on_disk() {
    // Discovery walker against the agents/ subdirectory must find
    // every module the manifest declares.  Catches the "manifest
    // points at a missing file" failure mode + a sanity-check that
    // the fixture is internally consistent.
    let dir = fixture_dir();
    let m = load_manifest(&dir).expect("load").expect("present");

    let agents_dir = dir.join("agents");
    let discovered = discover_ais_files(&agents_dir).expect("walk");
    let discovered_names: Vec<String> = discovered.iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    // Sorted-by-path order matches the BTreeMap iteration order
    // here because the module-name and file-stem happen to share a
    // common prefix sort (architect < coder), but the assertion
    // checks the SET shape rather than the precise order to keep
    // the test resilient to future fixture additions.
    assert!(discovered_names.contains(&"architect.ais".to_string()),
        "discovery should find architect.ais; got: {discovered_names:?}");
    assert!(discovered_names.contains(&"coder.ais".to_string()),
        "discovery should find coder.ais; got: {discovered_names:?}");

    // Cross-reference: every manifest-declared file exists on disk.
    for (module, rel_path) in &m.modules {
        let abs = dir.join(rel_path);
        assert!(abs.is_file(),
            "manifest references module `{module}` at {} but the file \
             is absent",
            abs.display());
    }
}

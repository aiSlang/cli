# v0.2 plain-files store — current schemas

Three artifacts together form the v0.2 "store".  Every field
below is load-bearing for v0.2 byte-stable round-trip + the
audit-trail (Decisions 25 / 26 / 30 from PLAN week-10).

## SpendFile — `~/.aislang/spend.json`

```rust
struct SpendFile {
    schema_version: u32,                       // currently 1
    projects: BTreeMap<String, BTreeMap<String, DayEntry>>,
    //        ^project_name      ^utc_date_str
}

struct DayEntry {
    micro_units: i64,                          // µ-USD, authoritative
    by_model:    BTreeMap<String, i64>,        // "provider.name" -> µ-USD
    by_main:     BTreeMap<String, i64>,        // main_label -> µ-USD
}
```

- Path: `~/.aislang/spend.json` (per-user, NOT per-project).
- `BTreeMap` (not `HashMap`) — deterministic iteration order
  for byte-stable serialization.
- `by_model` + `by_main`: `#[serde(default,
  skip_serializing_if = "BTreeMap::is_empty")]` — v0.1 files
  with no per-model breakdown round-trip identically.
- Sums of `by_model` values are expected to equal
  `micro_units` for new writes; older v1.0 files have empty
  `by_model` + non-zero `micro_units` (no per-model
  attribution).
- `by_main` (v0.2 Week 13 Thu): schema-only in v0.2 — v0.3's
  OpenAI-compat adapter for `mcp-serve` populates it.

## StateFile — `<source_dir>/.aislang/state.json`

```rust
struct StateFile {
    schema_version:  u32,                      // currently 1
    applied_at:      Option<String>,           // RFC3339 UTC; None = torn down
    source_hash:     String,                   // SHA-256 hex of .ais source
    catalog_version: String,                   // matches lockfile's top-level
    stack_yaml_path: PathBuf,                  // absolute
    project_name:    String,
}
```

- Path: `<source_dir>/.aislang/state.json` (per-project).
- `applied_at = None` means torn down by a previous
  `aislang destroy` — kept for audit-trail continuity, not
  deleted.
- `source_hash` lets `aislang status` flag "source has
  changed since apply" drift without re-hashing live.
- `catalog_version` recorded HERE so destroy + status work
  even when the lockfile has been deleted between apply +
  next command.
- `#[serde(default, skip_serializing_if = "Option::is_none")]`
  on `applied_at` — same byte-stability discipline.

## Lockfile — `<source_dir>/aislang.lock`

JSON file with these top-level keys (exact shapes vary across
versions; pin to the current schema for the migration):

```jsonc
{
  "schema_version":  1,
  "catalog_version": "<YYYY-MM-DD>",
  "source_hash":     "<hex SHA-256>",
  "model_bindings": {
    "<module>.<label>": { "provider": "...", "name": "..." }
  },
  "prompt_hashes": {
    "<module>.<label>": "<hex SHA-256>"
  },
  "oci_digests": { "...": "..." },
  "modules": {
    "<module>": "<hex SHA-256 of module source>"
  }
}
```

- Module-qualified keys for multi-module projects (W16 Wed +
  W20 Mon resolver-merge fix).
- Single-file projects: bare labels (no `module.` prefix).
- Path: `<source_dir>/aislang.lock` (per-project; NOT inside
  `.aislang/`).
- `modules` field present only when multi-module.
- Same `BTreeMap` discipline for deterministic key order.

## Why these are the three load-bearing artifacts

Per `state.rs` module docs:

> **Compliance audit trail**: the file plus `aislang.lock`
> plus `spend.json` cover the three pillars EU AI Act Art. 15
> + ISO 42001 reviewers ask about — what was deployed
> (state.json), under what model resolution (lockfile), at
> what cost (spend.json).

The SQLite migration MUST preserve every field above + the
byte-stable round-trip discipline so audit reviewers see the
same wire shape regardless of backend choice.

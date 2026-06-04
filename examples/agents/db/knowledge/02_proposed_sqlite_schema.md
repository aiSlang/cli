# Proposed SQLite schema for `~/.aislang/store.db`

Sketch — the Architect refines on receipt of the Feature spec.
This corpus pins the SHAPE so the brief has concrete schema
landmarks without locking the Architect into specific column
types.

## Tables

### `spend_days`

```sql
CREATE TABLE spend_days (
    project_name   TEXT    NOT NULL,
    utc_date       TEXT    NOT NULL,            -- YYYY-MM-DD
    micro_units    INTEGER NOT NULL,            -- µ-USD
    by_model_json  TEXT    NOT NULL DEFAULT '{}',  -- BTreeMap<String,i64>
    by_main_json   TEXT    NOT NULL DEFAULT '{}',
    PRIMARY KEY (project_name, utc_date)
);
```

- `by_model_json` + `by_main_json` are JSON-encoded BTreeMaps
  (preserves the deterministic-key-order from spend.json).
  SQLite JSON1 functions can query them when needed.
- Default `'{}'` mirrors the v0.1 `#[serde(default,
  skip_serializing_if = "BTreeMap::is_empty")]` byte-stable
  posture — empty maps don't appear in spend.json + don't
  appear here either when retrieved.

### `applied_stacks`

```sql
CREATE TABLE applied_stacks (
    project_name     TEXT    NOT NULL,
    schema_version   INTEGER NOT NULL,
    applied_at       TEXT,                       -- RFC3339 UTC; NULL = torn down
    source_hash      TEXT    NOT NULL,
    catalog_version  TEXT    NOT NULL,
    stack_yaml_path  TEXT    NOT NULL,
    PRIMARY KEY (project_name)
);
```

- Today's state.json is per-project file; consolidating into a
  single project-keyed row preserves the same uniqueness +
  audit-trail surface.
- `applied_at NULL` for torn-down stacks matches
  `Option<String>::None` byte-stability.

### `aislang_lock`

```sql
CREATE TABLE aislang_lock (
    project_name      TEXT    NOT NULL,
    schema_version    INTEGER NOT NULL,
    catalog_version   TEXT    NOT NULL,
    source_hash       TEXT    NOT NULL,
    model_bindings_json TEXT  NOT NULL,
    prompt_hashes_json  TEXT  NOT NULL,
    oci_digests_json    TEXT  NOT NULL DEFAULT '{}',
    modules_json        TEXT,                    -- NULL for single-file projects
    PRIMARY KEY (project_name)
);
```

- `modules_json NULL` for single-file projects mirrors today's
  "field absent" byte-stability.
- Same JSON-encoded BTreeMap discipline for bindings + hashes
  + digests.

### `migration_log`

```sql
CREATE TABLE migration_log (
    migrated_at     TEXT    NOT NULL,            -- RFC3339 UTC
    direction       TEXT    NOT NULL,            -- 'to_sqlite' | 'to_plain'
    bytes_imported  INTEGER NOT NULL,
    source_paths    TEXT    NOT NULL             -- JSON array of absolute paths
);
```

- Auditing surface for the migration itself: when, in which
  direction, how much.  Lets the user see "this store.db came
  from these specific plain-files" for compliance.

## Indexes

```sql
CREATE INDEX idx_spend_days_date ON spend_days(utc_date);
CREATE INDEX idx_applied_stacks_applied_at
    ON applied_stacks(applied_at) WHERE applied_at IS NOT NULL;
```

- `idx_spend_days_date` accelerates per-day rollups (today's
  `SpendTracker::today_spend()` reads one project + one date
  — that's the primary key already, but the secondary index
  speeds cross-project per-day reports).
- Partial index on `applied_at` excludes torn-down stacks
  (`applied_at IS NULL` rows) from the "currently applied"
  scan.

## Forward-compat hooks

- `schema_version` columns on every table support additive
  v0.5+ extensions without breaking v0.3 reads.
- JSON-encoded BTreeMap columns leave room for new keys
  without ALTER TABLE.
- Explicit `migration_log` table lets v0.5+ add new direction
  variants (e.g. `to_distributed` for cluster-mode store).

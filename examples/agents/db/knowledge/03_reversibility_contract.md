# Reversibility contract

The store.db migration is REVERSIBLE BY CONSTRUCTION.  Every
`aislang migrate --to=sqlite` invocation has an inverse
`aislang migrate --to=plain-files` that produces a
byte-identical plain-file tree as long as no new writes
happened post-migration.  This is non-negotiable for two
reasons:

1. **Decision 44.4 (`destroy -v` byte-stability)** — v0.2-
   shaped projects round-trip identically through the v0.3
   parser.  The store is one tier up: v0.2-shaped store files
   must round-trip identically through (migrate → rollback)
   so audit reviewers see the same wire shape.

2. **Operational escape hatch** — if the SQLite backend
   surfaces a regression in production, the user must be able
   to rollback to the plain-files backend WITHOUT data loss.
   The migration_log table records exactly which paths were
   imported; the rollback writes them back to the same paths.

## Round-trip test contract

The Architect's design MUST include this invariant in
"Decision implications":

> Every v0.2 fixture in `crates/aislang-runtime/tests/data/`
> + `crates/aislang-core/tests/data/` round-trips identically
> through `migrate --to=sqlite && migrate --to=plain-files`.
> `diff -r` against the pre-migration tree returns no output.

The Coder MUST emit a test along these lines:

```rust
#[test]
fn v0_2_fixture_round_trips_through_sqlite_migration() {
    let fixture_dir = include_dir!("tests/data/v0_2_baseline");
    let work = tempdir();
    copy_dir(&fixture_dir, &work);

    migrate(&work, Direction::ToSqlite).unwrap();
    migrate(&work, Direction::ToPlain).unwrap();

    assert_dir_byte_identical(&fixture_dir, &work);
}
```

Where `assert_dir_byte_identical` walks both trees + asserts
every file's contents are byte-equal.

## What "no new writes happened" means

The reversibility contract is a STRICT INVARIANT only when no
NEW writes happened to store.db between the to-sqlite and
to-plain migrations.  If new writes happened (`SpendTracker`
recorded a new spend, `apply` wrote a new state row),
rollback emits those new rows as plain-file appends — the
PRE-migration plain-file tree won't be identical, but the
POST-migration plain-file tree (with the new writes) WILL be
the byte-stable serialization of "everything in store.db
right now".

The migration_log table records every (direction, timestamp,
bytes_imported) tuple so audit can reconstruct the timeline.

## What CAN'T be reversed

- **Schema bumps**.  If a v0.5+ change adds a new column to
  `spend_days` that has no v0.2 plain-file analogue, rolling
  back DROPS that column's data.  v0.5+ MUST document the
  data-loss in `migration_log.notes` (a v0.5+-added field).
- **Concurrent multi-process writes** during the migration
  itself.  SQLite WAL mode handles concurrent reads; the
  migration takes an exclusive lock for the to-sqlite import
  + the to-plain export.

## Why not just keep plain-files

The driver for SQLite is multi-process safety (today's spend
tracker has a process-level Mutex that doesn't protect against
parallel CLI invocations from different shells) + queryability
for the future status surface (cross-project per-day rollups,
top-spender-by-model, etc).  The trade for adding a binary
backend is the reversibility contract above — without it,
SQLite would be a one-way door that audit reviewers would
have to manually verify by exporting back.

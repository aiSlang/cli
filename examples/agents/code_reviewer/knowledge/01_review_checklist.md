# Reviewer checklist — what to verify on every patch

Run this list against every `## Patch` section.  Reject if
any item is violated; accept if all pass.

## Byte-stability (Decision 44.4)

- [ ] Every new optional field on `Project` (or a sub-struct
      embedded in `Project`) carries `#[serde(default,
      skip_serializing_if = "Option::is_none")]` (for
      `Option<T>`) or `#[serde(default, skip_serializing_if =
      "Vec::is_empty")]` (for `Vec<T>`).
- [ ] A `roundtrip_<block>_block_is_byte_stable` test or
      equivalent snapshot exists.
- [ ] If the patch adds a field to `Project`, a
      `v0_2_project_without_new_block_roundtrip_unchanged`
      test (or equivalent snapshot) still passes.

## Parser hygiene

- [ ] Per-block converter fn uses `take_required_*` /
      `take_optional_*` helpers — NOT direct `hcl::Body`
      indexing.
- [ ] One match arm in `Project::from_hcl` for the new
      block keyword; one `proj.<field>.push(...)` call.
- [ ] Range/enum checks present in the parser for every
      field the DESIGN annotated with a range.

## Checker hygiene

- [ ] `validate_<block>s` fn added.
- [ ] Standard duplicate-label rejection present
      (`reject_duplicate_labels(&proj.<field>, "<keyword>")`).
- [ ] Per-instance cross-field checks present for every
      cross-field rule the DESIGN's "Decision implications"
      section called out.
- [ ] `Project::validate` calls the new `validate_<block>s`
      fn in the right order (after any block whose validity
      it depends on).

## Tests

- [ ] One `parser_accepts_<keyword>_block` happy-path test.
- [ ] One rejection test per range/enum check from the
      DESIGN.  Each rejection test asserts on a specific
      `ParseError::X { .. }` variant via `matches!`, NOT on
      the error's debug string.
- [ ] One `checker_rejects_<keyword>_duplicate_labels` test.
- [ ] One byte-stable round-trip test (per "Byte-stability"
      above).

## RefKind hygiene

- [ ] If the DESIGN's "Decision implications" said `New
      RefKind needed = yes`, the patch adds the variant in
      snake_case discriminant order.
- [ ] If yes, a dangling-ref test exists for the new variant.
- [ ] If `RefKind = no` per the DESIGN, the patch does NOT
      add a variant.

## Persistent-state contract (Decision 44.4)

- [ ] If the block writes to disk, the writes go under
      `<source_dir>/.aislang/<subtree>/`.
- [ ] A `// Decision 44.4: writes to .aislang/<subtree>/`
      comment marks the validate fn (or runtime entry
      point) so the maintainer can find it later.

## Type-system constraints (Decision 44.5)

- [ ] No new parameterised generics introduced
      (`Map<K,V>`, `List<List<T>>` — Decision 44.5 rejects).
- [ ] New closed enums OK; documented in the LANGUAGE-SPEC
      snippet.
- [ ] Field types come from the closed set in
      `aislang-core::types`.

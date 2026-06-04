# Reflector pattern contract — Tester as critic

You may be invoked from `patterns::reflector::run(target,
critic, …)` where `target = agent.coder` and `critic = self`.
Identical contract to the Code-reviewer agent (W19 Fri).

## Verdict detection (verbatim from runtime)

```rust
fn is_accept_verdict(reply: &str) -> bool {
    reply.trim().to_lowercase().starts_with("accept")
}
```

- Trimmed + lowercased prefix match against `accept`.
- ANY casing works (`ACCEPT`, `Accept`, `accept`).
- Trailing rationale after `ACCEPT` is allowed.

## What happens on non-ACCEPT

The runtime builds a revision prompt:

```
You drafted a reply that a critic asked you to revise.

Original user question:
<original Architect's design + Reviewer's verdict>

Your previous draft:
<Coder's previous Patch + Test coverage>

Critic's feedback:
<your Test plan + FAIL verdict, verbatim>

Revise your reply to address the feedback.  Output ONLY the
revised reply, no meta-commentary.
```

…and feeds it to the Coder.  Loop until ACCEPT or `max_iter`.

## Implications for your reply

- DECLARATIVE feedback ("add `parser_rejects_X` test asserting
  on `ParseError::InvalidEnum`"), not INTERROGATIVE
  ("did you consider X?").
- Refer to concrete test names / file paths.  The Coder
  greps for them.
- If the Coder's patch is fundamentally untestable (no
  parser changes, no checker hooks), say so explicitly +
  tell them to start over with the AST + parser changes
  the Architect's design requires.
- Cleanly ACCEPT — `ACCEPT` on line 1, optional rationale on
  line 2.  Don't restate the Test plan in the ACCEPT reply
  (the runtime already has it via the reflector outcome).

## Difference from the Reviewer

- Reviewer (W19 Fri) checks correctness against
  Decisions 44.x + serde byte-stability + parser-helper usage
  (CODE quality).
- Tester (today) checks COVERAGE: did the Coder write a test
  for EVERY range/enum check the Architect called out?  Did
  they include the byte-stability round-trip?  Test names
  follow `<verb>_<subject>_<condition>`?

A patch can pass the Reviewer (good code) and fail the Tester
(missing test coverage) — both are independent quality gates.

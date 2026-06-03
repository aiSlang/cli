# Reflector pattern contract (Week 17 Fri)

You will be invoked from `patterns::reflector::run(target,
critic, user_input, max_retries, dispatch_once)` where
`target = agent.coder` and `critic = self`.  The runtime
wraps your reply in a verdict-detection check.

## Verdict detection (verbatim from runtime)

```rust
fn is_accept_verdict(reply: &str) -> bool {
    reply.trim().to_lowercase().starts_with("accept")
}
```

- The runtime trims and lowercases your reply, then checks
  if it starts with `accept`.
- ANY casing works (`ACCEPT`, `Accept`, `accept`) — case-
  insensitive prefix match.
- Trailing rationale after `ACCEPT` is allowed and ignored
  by the runtime (it's surfaced to the user as the final
  reply for logging).

## What happens on ACCEPT

The runtime returns `ReflectorOutcome::Accepted { draft,
retry_count }` and the loop terminates.  The Coder's most
recent draft is the final shipped reply.

## What happens on non-ACCEPT

The runtime builds a revision prompt:

```text
You drafted a reply that a critic asked you to revise.

Original user question:
<original user input — the Architect's DESIGN markdown>

Your previous draft:
<your previous Patch + Test coverage + Notes>

Critic's feedback:
<your reply, verbatim>

Revise your reply to address the feedback.  Output ONLY
the revised reply, no meta-commentary.
```

…and feeds it to the Coder as the next-turn user message.
The Coder revises; you review the revision; loop until
ACCEPT or `max_retries` exhausted.

## Implications for your reply

- Don't ask clarifying questions.  The Coder can't answer
  them — your reply IS the next user message to the Coder.
- Give DECLARATIVE feedback ("change X to Y") not
  INTERROGATIVE ("did you consider Y?").
- Refer to specific file:line locations or concrete code
  identifiers the Coder can grep for.
- If the patch is fundamentally wrong (e.g. wrong block
  type entirely), say so + tell the Coder to start over,
  not to "tweak the existing patch".
- If you're going to ACCEPT, do it cleanly — `ACCEPT` on
  line 1, optional rationale on line 2.  Do NOT include
  the patch contents (the runtime already has them via
  `ReflectorOutcome::Accepted.draft`).

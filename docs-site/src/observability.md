# Observability

Every `aislang chat` and `aislang eval` run is observable in two ways: it emits
OpenTelemetry traces you can inspect in Jaeger, and it accumulates per-call
cost into a local spend ledger checked against your declared budget caps.

## Tracing

Every `aislang chat` and `aislang eval` invocation emits OpenTelemetry traces
(following the [OTel-GenAI semantic
conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/)) over
OTLP/HTTP to the bundled Jaeger sidecar, whose UI is at
**http://localhost:16686**.

The trace tree mirrors the chat loop's structure:

- Each **user turn** is one span.
- The **tool-use loop** fans out into one span per iteration.
- Each **iteration** carries one **LLM-call span** plus zero-or-more **MCP
  tool-call spans**.

Span attributes follow the OTel-GenAI conventions in the `gen_ai.*` namespace
(`gen_ai.system`, `gen_ai.request.model`, `gen_ai.usage.input_tokens`, …), plus
an `mcp.*` namespace for tool calls. The `mcp.*` attributes record **sizes
only, never argument contents** — PII-safe by construction.

After each command the trace URL prints to stderr — paste it into a browser to
see the full call hierarchy:

```
trace (eval `factual_accuracy`): http://localhost:16686/trace/7ca996386817d4859e6921c26245e4e3
```

`aislang eval`'s JSON report carries the same URLs as machine-readable fields,
so CI artifacts can deep-link straight into Jaeger per case:

```sh
aislang eval examples/support-agent.ais | jq -r '.trace_url, .cases[].trace_url'
```

Override the Jaeger UI URL with `AISLANG_JAEGER_UI` (e.g. for a shared
collector), or the OTLP endpoint with the standard
`OTEL_EXPORTER_OTLP_ENDPOINT` env var to ship to an external collector
(Honeycomb, Grafana Tempo, Datadog, an OTel Collector, …).

## Spend tracking

`aislang chat` and `aislang eval` accumulate per-call cost into
`~/.aislang/spend.json`. The file is written atomically and keyed by project +
UTC date, so a day's spend is always a consistent snapshot.

`aislang cost <file>` prints today's total plus a per-model breakdown, compared
against the `budget` caps declared in the source:

```sh
aislang cost examples/support-agent.ais
```

`aislang status` surfaces the same figures inline:

```sh
aislang status --spend examples/support-agent.ais         # today's total + cap utilization
aislang status --model-spend examples/support-agent.ais   # per-model breakdown
```

Budget caps are **enforced at runtime**, fail-closed: a turn that would push
spend past a cap is stopped before it runs.

```hcl
budget {
  hard_cap_per_run = $0.50   # a single run may not exceed this
  hard_cap_per_day = $50     # today's total may not exceed this
}
```

> **Note:** Open WebUI chats talk to LiteLLM directly and therefore **bypass
> this tracker** — their cost is not recorded in `~/.aislang/spend.json`. Use
> `aislang chat` / `aislang eval` when you need spend accounting and cap
> enforcement.

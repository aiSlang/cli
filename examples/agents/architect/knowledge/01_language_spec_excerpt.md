# aiSlang LANGUAGE-SPEC §4 — Resource blocks (excerpt)

Every aiSlang resource follows the same surface shape: `<keyword>
"<label>" { <attribute> = <value>  ... }`.  Labels are unique
within their kind; attributes are typed via the literal grammar
(§3).  Cross-references between blocks use `<kind>.<label>` syntax
(`agent.support`, `model.fast`, `pipeline.review_cycle`).

## §4.3 `model`

```
model "<identifier>" {
  requires    : List<Capability>      # chat | tool_use | reasoning | ...
  context_min : Size                  # min context window the model must offer
  cost_max    : Rate<Money, Size>?    # optional hard cost ceiling
  latency_p95 : Duration?
  prefer      : List<ModelId>         # ordered preference, e.g. anthropic.claude_haiku_4_5
  fallback    : List<ModelId>         # tried if prefer chain unavailable / over budget
  pin         : ModelId?              # force-resolve to exactly this model
}
```

## §4.4 `prompt`

```
prompt "<identifier>" {
  file     : Path           # ./prompts/<label>.md
  version  : SemVer
  template : Bool?          # v0.3 Week 18: opt-in MiniJinja rendering
}
```

## §4.5 `mcp_server`

```
mcp_server "<identifier>" {
  image   : OciImage         # docker.io/mcp/filesystem:1.0.2 etc.
  mount   : Path?            # bind-mounted at /data inside container
  env     : Map<String, String>?
  secrets : List<Ref<Secret>>?
}
```

## §4.6 `agent`

```
agent "<identifier>" {
  model         : Ref<Model>
  system_prompt : Ref<Prompt>
  tools         : List<Ref<McpServer>>
  knowledge     : List<Ref<VectorStore>>?   # v0.3 Week 19: RAG context
}
```

## §4.10 `main`

```
main "<identifier>" {
  model        : Ref<Model>
  chat_client  : Enum { claude_desktop | cursor | custom }
  pattern      : Enum { orchestrator | router | supervisor | ... }
}
```

## §4.12 `skill`

```
skill "<identifier>" {
  exec        : String?       # one of exec | mcp (XOR)
  mcp         : Ref<McpTool>?
  output_type : Enum { string | json | list_string }
}
```

## §4.13 `vector_store` + §4.14 `embedding` + §4.15 `agent.knowledge`

```
vector_store "<l>" {
  embedding     : Ref<Embedding>
  source        : String        # path | URL | crawl:URL
  chunk_size    : Size          # 256..=8192 tokens
  chunk_overlap : Size?         # default 0; must be < chunk_size
}

embedding "<l>" {
  provider : openai | cohere | ollama
  model    : String             # upstream model name verbatim
}
```

## Patterns this excerpt sets up

- **One-block grammar shape**: every resource gets a label, has typed attributes, optional fields use `Type?`, list fields use `List<...>`.
- **Cross-refs**: declared via `<kind>.<label>` (same-module) or `use <module>.<label>` (multi-module, Week 16 Tue).
- **Catalog-resolved**: `model.prefer/fallback/pin` + `embedding.provider/model` resolve against the bundled catalog at validate time.
- **Forward-compat fields**: new optional fields use `#[serde(default, skip_serializing_if = ...)]` so existing JSON fixtures round-trip byte-stable.
- **Closed enums**: `Capability`, `Provider`, `OutputType`, `DeployTarget` are closed sets; checker rejects unknown variants with the supported-set listed in the diagnostic.
- **Range checks**: `loop.max_iter` 1..=100, `vector_store.chunk_size` 256..=8192 — semantic ranges enforced at parse time.

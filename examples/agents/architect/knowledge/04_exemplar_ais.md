# Exemplar .ais — concrete shapes already in the repo

These are tiny, complete blocks copy-pastable as starting points.
When designing a new resource, anchor the grammar example on the
closest existing shape.

## Simplest agent (v0.1 baseline)

```hcl
project "support" {
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.05
  }
  model "fast" {
    requires    = [chat]
    context_min = 32k
    prefer      = [anthropic.claude_haiku_4_5]
  }
  prompt "system" {
    file    = "./prompts/system.md"
    version = "1.0.0"
  }
  agent "support" {
    model         = model.fast
    system_prompt = prompt.system
    tools         = []
  }
  deploy "local" { target = compose }
}
```

## RAG-enabled agent (v0.3 Week 19)

```hcl
embedding "local_bge" {
  provider = ollama
  model    = "bge-m3:latest"
}
vector_store "docs" {
  embedding     = embedding.local_bge
  source        = "./knowledge/"
  chunk_size    = 512
  chunk_overlap = 64
}
agent "researcher" {
  model         = model.thinker
  system_prompt = prompt.researcher_sys
  tools         = [mcp_server.docs]
  knowledge     = [vector_store.docs]
}
```

## Multi-agent pipeline (orchestrator pattern)

```hcl
pipeline "design_then_implement" {
  steps  = [agent.architect, agent.coder, agent.reviewer]
  input  = "natural-language feature spec"
  output = "rust diff + review comments"
}
```

## Skill block (shell + MCP variants)

```hcl
skill "list_files" {
  exec        = "git ls-files"
  output_type = list_string
}
skill "read_doc" {
  mcp         = mcp_server.docs.tool.read_text_file
  output_type = string
}
```

## Loop + branch composition

```hcl
loop "review_cycle" {
  body      = pipeline.draft_then_critique
  condition = "{{ critic.accept }}"
  max_iter  = 5
}
branch "triage" {
  on      = "{{ classifier_result }}"
  cases   = {
    billing   = pipeline.handle_billing,
    technical = pipeline.handle_technical,
    general   = pipeline.handle_general,
  }
  default = pipeline.handle_general
}
```

## Anti-patterns (do NOT design like this)

- `agent "x" { @retry-on-fail }` — Decision 44.7 defers
  annotations to v0.5+.  Express retries as `agent.retry_policy
  = "..."` or a `loop` block wrapping the agent's pipeline.
- `model "x" { custom_provider = "myprovider" }` — extending the
  closed provider set requires a catalog entry, not a per-block
  override.
- Parameterised generics in grammar (`output_type = list<json>`)
  — Decision 44.5 keeps types closed in v0.3.
- Writing files outside `<source_dir>/.aislang/` — destroy -v
  contract relies on the subtree convention.

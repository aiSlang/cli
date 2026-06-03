# RAG resources (v0.3 Week 19)

aiSlang ships native RAG (Retrieval-Augmented Generation) as
first-class grammar (Decision 44.6):

  - **`vector_store "<label>" { embedding = embedding.<l>  source
    = "<path|url|crawl:url>"  chunk_size = N  chunk_overlap = M }`**
    — declares a chunkable corpus.  Mon ships AST + parser +
    checker; Wed ships the runtime (build_index + query + save +
    load) backed by a brute-force cosine scan (v0.3 simplification
    of Decision 44.4's usearch — the surface is swap-compatible
    for v0.5+).

  - **`embedding "<label>" { provider = <openai|cohere|ollama>
    model = "<upstream-name>" }`** — declares the embedding
    model.  Tue adds AST + parser + checker + LiteLLM `/embeddings`
    route emission + three catalog entries (OpenAI's
    text-embedding-3-small, Cohere's embed-english-v3.0, and
    Ollama's bge-m3 for local-only setups).

  - **`agent "<label>" { … knowledge = [vector_store.<l>] }`** —
    Wed's agent-side hook.  Chat-time prepend of Top-K retrieved
    chunks to the system prompt lands in Phase 2 alongside the
    dogfood agent inventory.

Storage: indexes persist at `<source_dir>/.aislang/vector/<label>.bin`;
`aislang destroy -v` wipes the subtree.  The `aislang rag build` +
`aislang rag query` CLI subcommands let .ais authors test the RAG
surface end-to-end without writing Rust.

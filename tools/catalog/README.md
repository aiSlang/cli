# tools/catalog — the aiSlang agents catalog builder

A two-stage, dependency-free pipeline that turns the `examples/` tree into a
published agents catalog. It powers announcements (X, HN, …) and the agents
catalog page on docs.aislang.org.

```
examples/**/*.ais + aislang.toml
        │
        ▼   scan_catalog.py   (parse .ais / manifests → structured JSON)
   agents-catalog.json
        │
        ▼   render_catalog.py (JSON → human-readable Markdown)
 agents-catalog.md
        │
        ▼   aiSlang/landing    (Markdown → styled HTML on docs.aislang.org,
                                 with info-graphics, screenshots, site theme)
```

## Usage

```sh
# 1. scan the examples tree into agents-catalog.json
python3 tools/catalog/scan_catalog.py            # --target examples --out agents-catalog.json

# 2. render agents-catalog.json into agents-catalog.md
python3 tools/catalog/render_catalog.py          # --catalog agents-catalog.json --out agents-catalog.md

# 3. also render the docs-site page (absolute GitHub source links, so they
#    don't 404 on the built mdBook at docs.aislang.org)
python3 tools/catalog/render_catalog.py \
    --link-base "https://github.com/aiSlang/cli/blob/main/" \
    --out docs-site/src/agents.md
```

Both stages are pure filesystem read + parse with sorted, deterministic output —
same tree in, same catalog out — so they run cleanly in CI. No network and no
`aislang` binary required.

Consumers of the generated catalog:

- **docs.aislang.org** — `docs-site/src/agents.md` (mdBook page, absolute links).
- **aislang.org/agents.html** — the `aiSlang/landing` repo copies
  `agents-catalog.json` + `agents-catalog.md` into its `data/` and renders a
  themed, filterable page from them.

## Files

| File | Role |
|---|---|
| `ais_parse.py` | Lightweight, dependency-free parser for the `.ais` DSL (block tree + scalar/list assignments). Not the canonical parser — just enough for tooling. |
| `scan_catalog.py` | Walks the target folder, reconstructs each agent / multi-agent project (models, prompts, agents, skills, pipelines, RAG, MCP tools, evals, deploy, docs) → `agents-catalog.json`. |
| `render_catalog.py` | Renders `agents-catalog.json` → `agents-catalog.md` (index table + one detailed section per project). |
| `agents-catalog.json` | Generated intermediate — the machine-readable catalog. |
| `agents-catalog.md` | Generated hand-off artifact for the landing / docs site. |
| `tests/` | Unit tests for the `.ais` parser. |

## What counts as a catalog entry

- A directory with an `aislang.toml` `[modules]` manifest → one **multi-module**
  entry (its member `.ais` files are folded in, not listed separately).
- Any `.ais` file with a `project { … }` block that declares at least one
  `agent`, `skill`, `pipeline`, or `loop` → one **single-file** entry.

Pure value snippets (`examples/snippets/`) and error fixtures (`examples/errors/`)
declare no agents/skills/pipelines and are skipped automatically.

## Regenerating

The committed `agents-catalog.json` / `agents-catalog.md` are generated artifacts. After
adding or editing an example, rebuild both and commit the result:

```sh
python3 tools/catalog/scan_catalog.py && python3 tools/catalog/render_catalog.py
```

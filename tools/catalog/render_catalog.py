#!/usr/bin/env python3
"""Render the agents catalog JSON into `agents-catalog.md`.

Takes the `agents-catalog.json` produced by `scan_catalog.py` and emits a single,
human-readable Markdown catalog: an at-a-glance index table followed by one
richly-detailed section per agent / multi-agent project — description, the
agents and their models, skills, pipelines, RAG knowledge, MCP tools, evals,
deploy targets, and links to the source + docs.

That Markdown is the hand-off artifact for docs.aislang.org (the aiSlang/landing
project renders it — with info-graphics, screenshots, and the site theme — into
an HTML catalog page).

    python3 tools/catalog/render_catalog.py \
        [--catalog tools/catalog/agents-catalog.json] [--out tools/catalog/agents-catalog.md]

Deterministic: same agents-catalog.json in, same Markdown out.
"""
from __future__ import annotations

import argparse
import json
import os
import re


def _slug(s: str | None) -> str:
    return re.sub(r"[^a-z0-9]+", "-", (s or "").lower()).strip("-")


def display_name(e: dict) -> str:
    """Name, plus a variant hint when the id disambiguates a shared name
    (e.g. project 'support-agent' from support-agent-paid.ais → 'support-agent (paid)')."""
    name = e["name"]
    base = _slug(name)
    if e["id"] != base and e["id"].startswith(base + "-"):
        return f"{name} ({e['id'][len(base) + 1:]})"
    return name

TAG_LABELS = {
    "multi-agent": "🤝 multi-agent",
    "rag": "📚 RAG",
    "skills": "🛠 skills",
    "mcp-tools": "🔌 MCP tools",
    "pipeline": "➡️ pipeline",
    "reflector-loop": "🔁 reflector loop",
    "evals": "✅ evals",
    "deploy-compose": "🐳 compose",
    "deploy-kubernetes": "☸️ kubernetes",
}


def tag(t: str) -> str:
    return TAG_LABELS.get(t, f"`{t}`")


def md_escape(s: str | None) -> str:
    if not s:
        return ""
    return s.replace("|", "\\|").replace("\n", " ").strip()


def code(s: str | None) -> str:
    return f"`{s}`" if s else "—"


def joincode(items) -> str:
    items = [i for i in (items or []) if i]
    return ", ".join(f"`{i}`" for i in items) if items else "—"


# Base for source links. Default is a repo-relative hop (works from
# tools/catalog/ on GitHub); set --link-base to an absolute URL for the docs
# site, where relative paths to files outside the book would 404.
LINK_BASE = "../../"


def repo_link(path: str | None) -> str:
    if not path:
        return "—"
    return f"[`{path}`]({LINK_BASE}{path})"


# ── section renderers ────────────────────────────────────────────────
def render_index(entries: list[dict]) -> str:
    rows = ["| Agent / Project | Kind | Agents | Skills | Highlights |",
            "|---|---|:-:|:-:|---|"]
    for e in entries:
        tags = " ".join(tag(t) for t in e.get("tags", []))
        rows.append(
            f"| [**{md_escape(display_name(e))}**](#{e['id']}) | {e['kind']} "
            f"| {len(e['agents'])} | {len(e['skills'])} | {tags} |")
    return "\n".join(rows)


def render_agents(e: dict) -> str:
    if not e["agents"]:
        return ""
    multimod = e["kind"] == "multi-module"
    head = "| Agent | Model | Prompt | Tools | Knowledge |"
    sep = "|---|---|---|---|---|"
    if multimod:
        head = "| Agent | Module | Model | Prompt | Tools | Knowledge |"
        sep = "|---|---|---|---|---|---|"
    rows = [head, sep]
    for a in e["agents"]:
        cells = [f"**{md_escape(a['name'])}**"]
        if multimod:
            cells.append(code(a.get("module")))
        cells += [code(a.get("model")), code(a.get("system_prompt")),
                  joincode(a.get("tools")), joincode(a.get("knowledge"))]
        rows.append("| " + " | ".join(cells) + " |")
    return "**Agents**\n\n" + "\n".join(rows)


def render_skills(e: dict) -> str:
    if not e["skills"]:
        return ""
    rows = ["| Skill | Kind | Command / server | Output |", "|---|---|---|---|"]
    for s in e["skills"]:
        cmd = s.get("exec") or s.get("mcp") or ""
        rows.append(f"| **{md_escape(s['name'])}** | {s.get('kind', 'exec')} "
                    f"| {code(cmd)} | {code(s.get('output_type'))} |")
    return "**Skills** (deterministic compute — the model never does this work)\n\n" \
        + "\n".join(rows)


def render_pipelines(e: dict) -> str:
    out = []
    for p in e["pipelines"]:
        steps = " → ".join(f"`{s}`" for s in p.get("steps", [])) or "—"
        line = f"- **`{p['name']}`**: {steps}"
        if p.get("input") or p.get("output"):
            line += f"\n  - in: {md_escape(p.get('input')) or '—'}; " \
                    f"out: {md_escape(p.get('output')) or '—'}"
        out.append(line)
    for lp in e.get("loops", []):
        cond = md_escape(lp.get("condition"))
        out.append(f"- **`{lp['name']}`** (loop): body `{lp.get('body')}`, "
                   f"until `{cond}`, max {lp.get('max_iter')}")
    if not out:
        return ""
    return "**Composition**\n\n" + "\n".join(out)


def render_models(e: dict) -> str:
    if not e["models"]:
        return ""
    rows = ["| Model | Requires | Prefer | Fallback |", "|---|---|---|---|"]
    for m in e["models"]:
        rows.append(f"| **{md_escape(m['name'])}** | {joincode(m.get('requires'))} "
                    f"| {joincode(m.get('prefer'))} | {joincode(m.get('fallback'))} |")
    return "**Models**\n\n" + "\n".join(rows)


def render_knowledge(e: dict) -> str:
    parts = []
    vs = e.get("vector_stores", [])
    if vs:
        names = ", ".join(f"`{v['name']}` (`{v.get('source') or '?'}`)" for v in vs)
        parts.append(f"- Vector store(s): {names}")
    kf = e.get("knowledge_files", [])
    if kf:
        parts.append("- Corpus: " + ", ".join(f"`{f}`" for f in kf))
    if not parts:
        return ""
    return "**RAG knowledge**\n\n" + "\n".join(parts)


def render_tools_evals_deploy(e: dict) -> str:
    parts = []
    if e.get("mcp_servers"):
        srv = ", ".join(
            f"`{s['name']}` → `{s.get('image') or '?'}`" for s in e["mcp_servers"])
        parts.append(f"- MCP tools: {srv}")
    if e.get("evals"):
        ev = ", ".join(
            f"`{v['name']}` ({v.get('metric') or '?'} ≥ {v.get('threshold') or '?'})"
            for v in e["evals"])
        parts.append(f"- Evals: {ev}")
    if e.get("deploy"):
        dp = ", ".join(f"`{d.get('target') or '?'}`" for d in e["deploy"])
        parts.append(f"- Deploy: {dp}")
    if e.get("prompt_files"):
        parts.append("- Prompts: " + ", ".join(f"`{p}`" for p in e["prompt_files"]))
    if not parts:
        return ""
    return "**Tooling & delivery**\n\n" + "\n".join(parts)


def render_docs(e: dict) -> str:
    docs = e.get("docs", {})
    parts = [f"- Source: {repo_link(e.get('entry_file'))} "
             f"(dir: {repo_link(e.get('path'))})"]
    for key, label in (("readme", "README"), ("design", "DESIGN"),
                       ("architecture", "ARCHITECTURE")):
        d = docs.get(key)
        if isinstance(d, dict) and d.get("path"):
            parts.append(f"- {label}: {repo_link(d['path'])}")
    if e.get("modules"):
        mods = ", ".join(
            f"`{m['name']}`" + ("" if not m.get("agents")
                                else f" ({', '.join(m['agents'])})")
            for m in e["modules"])
        parts.append(f"- Modules: {mods}")
    return "**Source & docs**\n\n" + "\n".join(parts)


def render_entry(e: dict) -> str:
    tags = " ".join(tag(t) for t in e.get("tags", []))
    meta = f"*{e['kind']}*"
    if e.get("version"):
        meta += f" · v{e['version']}"
    lines = [f'<a id="{e["id"]}"></a>', f"## {md_escape(display_name(e))}", "",
             f"{meta} · {tags}", ""]
    desc = (e.get("description") or "").strip()
    if desc:
        lines += [md_escape(desc), ""]
    for section in (render_agents(e), render_skills(e), render_pipelines(e),
                    render_models(e), render_knowledge(e),
                    render_tools_evals_deploy(e), render_docs(e)):
        if section:
            lines += [section, ""]
    return "\n".join(lines).rstrip() + "\n"


def render(catalog: dict) -> str:
    entries = catalog.get("entries", [])
    n = len(entries)
    total_agents = sum(len(e["agents"]) for e in entries)
    header = [
        "# aiSlang Agents Catalog",
        "",
        "> **Generated** — do not edit by hand. Rebuild with:",
        "> `python3 tools/catalog/scan_catalog.py && "
        "python3 tools/catalog/render_catalog.py`.",
        "",
        f"Scanned `{catalog.get('root', 'examples')}/` on "
        f"{catalog.get('generated_at', '?')} — **{n} agents / multi-agent "
        f"projects**, **{total_agents} agents** in total.",
        "",
        "Each entry is a self-contained aiSlang project: declarative "
        "capability-based model selection, prompts, agents, deterministic "
        "compute skills, RAG knowledge, MCP tools, evals, and a deploy target — "
        "all reconstructed straight from the `.ais` / `aislang.toml` source.",
        "",
        "## Index",
        "",
        render_index(entries),
        "",
        "---",
        "",
    ]
    body = "\n".join(render_entry(e) + "\n---\n" for e in entries)
    footer = ("\n_Built by `tools/catalog` — the source of truth for "
              "docs.aislang.org's agents catalog page._\n")
    return "\n".join(header) + "\n" + body + footer


def main() -> int:
    global LINK_BASE
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--catalog", default=os.path.join(here, "agents-catalog.json"))
    ap.add_argument("--out", default=os.path.join(here, "agents-catalog.md"))
    ap.add_argument("--link-base", default=LINK_BASE,
                    help="base for source links (e.g. https://github.com/aiSlang/cli/blob/main/ "
                         "for the docs site, where relative paths would 404)")
    args = ap.parse_args()
    LINK_BASE = args.link_base

    with open(args.catalog, encoding="utf-8") as fh:
        catalog = json.load(fh)
    md = render(catalog)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as fh:
        fh.write(md)
    print(f"catalog markdown: {catalog.get('count', 0)} entries -> {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

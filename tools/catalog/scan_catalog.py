#!/usr/bin/env python3
"""Scan a folder of aiSlang examples into a machine-readable agents catalog.

Walks a target directory (``examples/`` by default) for ``*.ais`` files and
``aislang.toml`` multi-module manifests, reconstructs each agent / multi-agent
project's structure — models, prompts, agents, skills, pipelines, RAG stores,
MCP tools, evals, deploy targets, plus the docs (README / DESIGN / the ``.ais``
header comment) — and writes it all to a single ``catalog.json``.

That JSON is the source of truth for the second stage (``render_catalog.py`` ->
``agents-catalog.md``) and, downstream, for updating docs.aislang.org.

    python3 tools/catalog/scan_catalog.py \
        [--root .] [--target examples] [--out tools/catalog/catalog.json]

Determinism: pure filesystem read + parse, sorted output — same tree in, same
catalog out.  No network, no aislang binary required.
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ais_parse as ap  # noqa: E402

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # pragma: no cover
    tomllib = None

DOC_NAMES = ("README.md", "DESIGN.md", "ARCHITECTURE.md")


# ── small helpers ────────────────────────────────────────────────────
def slugify(text: str) -> str:
    s = re.sub(r"[^a-z0-9]+", "-", (text or "").lower()).strip("-")
    return s or "agent"


def rel(root: str, path: str) -> str:
    return os.path.relpath(path, root).replace(os.sep, "/")


def read(path: str) -> str:
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def first_paragraph(md: str) -> str:
    """First non-heading, non-blockquote prose paragraph of a markdown doc."""
    para: list[str] = []
    for line in md.splitlines():
        s = line.strip()
        if not s:
            if para:
                break
            continue
        if s.startswith("#") or s.startswith(">") or s.startswith("---") \
                or s.startswith("|") or s.startswith("```"):
            if para:
                break
            continue
        para.append(s)
    return " ".join(para).strip()


def doc_title(md: str) -> str | None:
    for line in md.splitlines():
        m = re.match(r"^#\s+(.*)$", line.strip())
        if m:
            return m.group(1).strip()
    return None


def list_files(dirpath: str, exts: tuple[str, ...]) -> list[str]:
    if not os.path.isdir(dirpath):
        return []
    return sorted(f for f in os.listdir(dirpath)
                  if f.lower().endswith(exts) and os.path.isfile(os.path.join(dirpath, f)))


def scripts_from_exec(exec_str: str | None) -> list[str]:
    """Pull referenced script basenames out of a skill's exec command."""
    if not exec_str:
        return []
    out = []
    for tok in exec_str.split():
        if tok.endswith((".py", ".sh", ".js", ".ts", ".rb")):
            out.append(os.path.basename(tok))
    return out


# ── block → dict extractors ──────────────────────────────────────────
def model_dict(b: ap.Block) -> dict:
    return {
        "name": b.name,
        "requires": ap.as_list(b.props.get("requires")),
        "prefer": ap.as_list(b.props.get("prefer")),
        "fallback": ap.as_list(b.props.get("fallback")),
        "context_min": b.props.get("context_min"),
        "cost_max": b.props.get("cost_max"),
    }


def prompt_dict(b: ap.Block) -> dict:
    return {"name": b.name, "file": ap.unquote(b.props.get("file")),
            "version": ap.unquote(b.props.get("version"))}


def agent_dict(b: ap.Block, module: str | None = None) -> dict:
    d = {
        "name": b.name,
        "model": ap.ref_label(b.props.get("model")),
        "system_prompt": ap.ref_label(b.props.get("system_prompt")),
        "tools": [ap.ref_label(t) or t for t in ap.as_list(b.props.get("tools"))],
        "knowledge": [ap.ref_label(k) or k for k in ap.as_list(b.props.get("knowledge"))],
    }
    if module:
        d["module"] = module
    return d


def skill_dict(b: ap.Block) -> dict:
    ex = ap.unquote(b.props.get("exec"))
    return {"name": b.name, "kind": "mcp" if b.props.get("mcp") else "exec",
            "exec": ex, "mcp": ap.ref_label(b.props.get("mcp")),
            "output_type": b.props.get("output_type"),
            "scripts": scripts_from_exec(ex)}


def pipeline_dict(b: ap.Block) -> dict:
    return {"name": b.name,
            "steps": [ap.ref_label(s) if "." in s and not s.startswith("use") else s
                      for s in ap.as_list(b.props.get("steps"))],
            "input": ap.unquote(b.props.get("input")),
            "output": ap.unquote(b.props.get("output"))}


def loop_dict(b: ap.Block) -> dict:
    return {"name": b.name, "body": ap.ref_label(b.props.get("body")),
            "condition": ap.unquote(b.props.get("condition")),
            "max_iter": b.props.get("max_iter")}


def vstore_dict(b: ap.Block) -> dict:
    return {"name": b.name, "source": ap.unquote(b.props.get("source"))}


def mcp_dict(b: ap.Block) -> dict:
    return {"name": b.name, "image": ap.unquote(b.props.get("image")),
            "mount": ap.unquote(b.props.get("mount"))}


def eval_dict(b: ap.Block) -> dict:
    return {"name": b.name, "dataset": ap.unquote(b.props.get("dataset")),
            "metric": b.props.get("metric"), "threshold": b.props.get("threshold")}


def deploy_dict(b: ap.Block) -> dict:
    return {"name": b.name, "target": b.props.get("target")}


def collect_from_project(proj: ap.Block, into: dict, module: str | None = None) -> None:
    """Fold one project block's resources into an entry dict (aggregating)."""
    into["models"] += [model_dict(b) for b in proj.find("model")]
    into["prompts"] += [prompt_dict(b) for b in proj.find("prompt")]
    into["agents"] += [agent_dict(b, module) for b in proj.find("agent")]
    into["skills"] += [skill_dict(b) for b in proj.find("skill")]
    into["pipelines"] += [pipeline_dict(b) for b in proj.find("pipeline")]
    into["loops"] += [loop_dict(b) for b in proj.find("loop")]
    into["vector_stores"] += [vstore_dict(b) for b in proj.find("vector_store")]
    into["mcp_servers"] += [mcp_dict(b) for b in proj.find("mcp_server")]
    into["evals"] += [eval_dict(b) for b in proj.find("eval")]
    into["deploy"] += [deploy_dict(b) for b in proj.find("deploy")]


def is_agentic(proj: ap.Block) -> bool:
    """A project is catalog-worthy if it declares at least one agent, skill,
    pipeline, or loop — filters out value snippets and error fixtures."""
    return any(proj.find(t) for t in ("agent", "skill", "pipeline", "loop"))


def blank_entry() -> dict:
    return {"models": [], "prompts": [], "agents": [], "skills": [],
            "pipelines": [], "loops": [], "vector_stores": [], "mcp_servers": [],
            "evals": [], "deploy": []}


def gather_docs(root: str, dirpath: str, header: str) -> dict:
    docs = {"header": header}
    for name in DOC_NAMES:
        p = os.path.join(dirpath, name)
        if os.path.isfile(p):
            md = read(p)
            key = name.split(".")[0].lower()
            docs[key] = {"path": rel(root, p), "title": doc_title(md),
                         "summary": first_paragraph(md)}
    return docs


def derive_tags(entry: dict) -> list[str]:
    tags = []
    if len(entry["agents"]) > 1 or entry["kind"] == "multi-module":
        tags.append("multi-agent")
    if entry["vector_stores"]:
        tags.append("rag")
    if entry["skills"]:
        tags.append("skills")
    if entry["mcp_servers"] or any(a["tools"] for a in entry["agents"]):
        tags.append("mcp-tools")
    if entry["pipelines"]:
        tags.append("pipeline")
    if entry["loops"]:
        tags.append("reflector-loop")
    for d in entry["deploy"]:
        if d.get("target"):
            tags.append(f"deploy-{d['target']}")
    if entry["evals"]:
        tags.append("evals")
    # de-dup, keep order
    seen, out = set(), []
    for t in tags:
        if t not in seen:
            seen.add(t)
            out.append(t)
    return out


# ── entry builders ───────────────────────────────────────────────────
def build_singlefile_entry(root: str, ais_path: str) -> dict | None:
    text = read(ais_path)
    blocks = ap.parse(text)
    projects = [b for b in blocks if b.type == "project"]
    if not projects or not any(is_agentic(p) for p in projects):
        return None
    proj = projects[0]
    dirpath = os.path.dirname(ais_path)
    stem = os.path.splitext(os.path.basename(ais_path))[0]
    entry = blank_entry()
    entry.update({
        # Prefer the project name for the id; the file stem is the tie-breaker
        # for variants that reuse a project name (support-agent vs -paid).
        "id": slugify(proj.name or stem),
        "_stem": slugify(stem),
        "name": proj.name or stem,
        "kind": "single-file",
        "path": rel(root, dirpath),
        "entry_file": rel(root, ais_path),
        "budget": _budget(proj),
    })
    for p in projects:
        collect_from_project(p, entry)
    entry["docs"] = gather_docs(root, dirpath, ap.leading_doc(text))
    entry["prompt_files"] = list_files(os.path.join(dirpath, "prompts"), (".md",))
    entry["knowledge_files"] = list_files(os.path.join(dirpath, "knowledge"),
                                          (".md", ".txt"))
    entry["has_scripts"] = os.path.isdir(os.path.join(dirpath, "scripts"))
    entry["tags"] = derive_tags(entry)
    entry["description"] = _describe(entry)
    return entry


def build_multimodule_entry(root: str, toml_path: str) -> tuple[dict | None, list[str]]:
    """Return (entry, member_ais_abspaths). Members are excluded as standalone."""
    if tomllib is None:  # pragma: no cover
        return None, []
    with open(toml_path, "rb") as fh:
        manifest = tomllib.load(fh)
    modules = manifest.get("modules") or {}
    if not modules:
        return None, []
    base = os.path.dirname(toml_path)
    proj_meta = manifest.get("project", {})
    entry = blank_entry()
    entry.update({
        "kind": "multi-module",
        "path": rel(root, base),
        "entry_file": rel(root, toml_path),
        "modules": [],
    })
    member_paths: list[str] = []
    header = ""
    entry_header = ""      # doc of the entry-point module (lives in the toml dir)
    for mod_name, mod_rel in sorted(modules.items()):
        mod_path = os.path.normpath(os.path.join(base, mod_rel))
        member_paths.append(mod_path)
        if not os.path.isfile(mod_path):
            entry["modules"].append({"name": mod_name, "file": mod_rel, "missing": True})
            continue
        text = read(mod_path)
        projs = [b for b in ap.parse(text) if b.type == "project"]
        proj = projs[0] if projs else None
        for p in projs:
            collect_from_project(p, entry, module=mod_name)
        entry["modules"].append({
            "name": mod_name,
            "file": rel(root, mod_path),
            "project": proj.name if proj else None,
            "agents": [a.name for p in projs for a in p.find("agent")],
        })
        doc = ap.leading_doc(text)
        # The entry-point module is the one referenced without a `../` hop.
        if not mod_rel.startswith("..") and not entry_header:
            entry_header = doc
        if not header:
            header = doc
    header = entry_header or header
    entry["name"] = proj_meta.get("name") or os.path.basename(base)
    entry["id"] = slugify(entry["name"])
    entry["_stem"] = slugify(os.path.basename(base))
    entry["version"] = proj_meta.get("version")
    entry["docs"] = gather_docs(root, base, header)
    entry["prompt_files"] = list_files(os.path.join(base, "prompts"), (".md",))
    entry["knowledge_files"] = list_files(os.path.join(base, "knowledge"), (".md", ".txt"))
    entry["has_scripts"] = os.path.isdir(os.path.join(base, "scripts"))
    entry["tags"] = derive_tags(entry)
    entry["description"] = _describe(entry)
    return entry, member_paths


def _budget(proj: ap.Block) -> dict | None:
    b = proj.first("budget")
    if not b:
        return None
    return {k: v for k, v in b.props.items()}


def _describe(entry: dict) -> str:
    docs = entry.get("docs", {})
    for key in ("readme", "design"):
        if docs.get(key) and docs[key].get("summary"):
            return docs[key]["summary"]
    header = docs.get("header") or ""
    return first_paragraph(header) or header.split("\n\n")[0].strip()


# ── driver ───────────────────────────────────────────────────────────
def scan(root: str, target: str) -> dict:
    target_abs = os.path.join(root, target)
    ais_files, toml_files = [], []
    for dirpath, _dirs, files in os.walk(target_abs):
        # skip generated / hidden dirs
        parts = set(dirpath.split(os.sep))
        if parts & {".aislang", "generated", "__pycache__", ".pytest_cache", "target"}:
            continue
        for f in files:
            if f.endswith(".ais"):
                ais_files.append(os.path.join(dirpath, f))
            elif f == "aislang.toml":
                toml_files.append(os.path.join(dirpath, f))

    entries: list[dict] = []
    consumed: set[str] = set()
    for toml_path in sorted(toml_files):
        entry, members = build_multimodule_entry(root, toml_path)
        consumed.update(os.path.normpath(m) for m in members)
        if entry:
            entries.append(entry)

    for ais_path in sorted(ais_files):
        if os.path.normpath(ais_path) in consumed:
            continue
        entry = build_singlefile_entry(root, ais_path)
        if entry:
            entries.append(entry)

    entries.sort(key=lambda e: (0 if e["kind"] == "multi-module" else 1,
                                -len(e["agents"]), e["name"]))
    # Guarantee unique ids (anchors in the rendered catalog). A "canonical"
    # entry whose file stem matches its project name gets first pick of the
    # base id; variants that reuse a project name fall back to their file stem,
    # then a numeric suffix.  (Assign ids in priority order without disturbing
    # the display order of `entries`.)
    order = sorted(range(len(entries)),
                   key=lambda i: (0 if entries[i].get("_stem") == slugify(entries[i]["name"])
                                  else 1, i))
    used: set[str] = set()
    for i in order:
        e = entries[i]
        cid = slugify(e["name"])
        if cid in used:
            cid = e.get("_stem") or cid
        n, base = 2, cid
        while cid in used:
            cid = f"{base}-{n}"
            n += 1
        e["id"] = cid
        used.add(cid)
    for e in entries:
        e.pop("_stem", None)
    return {
        "generated_at": datetime.datetime.now(datetime.timezone.utc)
                        .isoformat(timespec="seconds"),
        "root": target,
        "count": len(entries),
        "entries": entries,
    }


def main() -> int:
    ap_ = argparse.ArgumentParser(description=__doc__,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    here = os.path.dirname(os.path.abspath(__file__))
    repo_root = os.path.dirname(os.path.dirname(here))
    ap_.add_argument("--root", default=repo_root, help="repo root (paths are relative to it)")
    ap_.add_argument("--target", default="examples", help="folder to scan (default: examples)")
    ap_.add_argument("--out", default=os.path.join(here, "catalog.json"))
    args = ap_.parse_args()

    catalog = scan(os.path.abspath(args.root), args.target)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as fh:
        json.dump(catalog, fh, indent=2, ensure_ascii=False)
        fh.write("\n")
    print(f"catalog: {catalog['count']} agents/projects -> {args.out}")
    for e in catalog["entries"]:
        print(f"  • {e['name']:24} [{e['kind']}] "
              f"{len(e['agents'])} agent(s), {len(e['skills'])} skill(s)  {e['tags']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

"""Lightweight, dependency-free parser for the aiSlang (`.ais`) DSL.

This is NOT the canonical parser (that lives in `crates/aislang-core`); it is a
pragmatic reader for tooling — enough to walk a `.ais` file into a tree of
resource blocks and pull out names + scalar/list assignments for the agents
catalog.  It deliberately tolerates comments, nested blocks (`deploy { expose
{ … } }`), list values spanning lines, and MiniJinja strings that contain
braces (e.g. `condition = "{{ output | … }}"`).

Grammar recap (LANGUAGE-SPEC v0):
    project "name" { … }
    model "x" { requires = [chat]  prefer = [anthropic.claude_haiku_4_5] … }
    prompt "x" { file = "./p.md"  version = "1.0.0" }
    agent "x" { model = model.x  system_prompt = prompt.x  tools = [mcp_server.y] }
    skill "x" { exec = "python3 …"  output_type = json }
    pipeline "x" { steps = [agent.a, use mod.b] }
    loop / branch / eval / deploy / budget / embedding / vector_store / mcp_server

Blocks are `type "optional-name" { … }`; leaves are `key = value` (one per
line, values may be lists across lines).  Comments are `#`, `##`, or `//`.
"""
from __future__ import annotations

import re


class Block:
    """One `type "name" { props… children… }` node."""

    __slots__ = ("type", "name", "props", "children")

    def __init__(self, btype: str, name: str | None = None):
        self.type = btype
        self.name = name
        self.props: dict[str, str] = {}
        self.children: list["Block"] = []

    def find(self, btype: str) -> list["Block"]:
        return [c for c in self.children if c.type == btype]

    def first(self, btype: str) -> "Block | None":
        for c in self.children:
            if c.type == btype:
                return c
        return None

    def __repr__(self) -> str:  # pragma: no cover - debug aid
        n = f' "{self.name}"' if self.name else ""
        return f"<Block {self.type}{n} props={list(self.props)} children={len(self.children)}>"


def strip_comments(text: str) -> str:
    """Remove `#…` and `//…` line comments, honouring quoted strings."""
    out = []
    for line in text.splitlines():
        buf = []
        i, n = 0, len(line)
        quote = None
        while i < n:
            c = line[i]
            if quote:
                buf.append(c)
                if c == quote:
                    quote = None
                i += 1
                continue
            if c in "\"'":
                quote = c
                buf.append(c)
                i += 1
                continue
            if c == "#":
                break
            if c == "/" and i + 1 < n and line[i + 1] == "/":
                break
            buf.append(c)
            i += 1
        out.append("".join(buf))
    return "\n".join(out)


_NAME_RE = re.compile(r'^\s*(\w+)\s+"([^"]+)"')
_TYPE_RE = re.compile(r"^\s*(\w+)")


def _parse_body(s: str, i: int) -> tuple[list[Block], dict[str, str], int]:
    """Parse a `{ … }` body starting just after the opening brace at `i`."""
    children: list[Block] = []
    props: dict[str, str] = {}
    header = ""
    n = len(s)
    quote = None
    while i < n:
        c = s[i]
        if quote:
            header += c
            if c == quote:
                quote = None
            i += 1
            continue
        if c in "\"'":
            quote = c
            header += c
            i += 1
            continue
        if c == "}":
            return children, props, i + 1
        if c == "{":
            btype_m = _TYPE_RE.match(header)
            btype = btype_m.group(1) if btype_m else ""
            name_m = _NAME_RE.match(header)
            name = name_m.group(2) if name_m else None
            blk = Block(btype, name)
            blk.children, blk.props, i = _parse_body(s, i + 1)
            children.append(blk)
            header = ""
            continue
        if c == "=":
            key = header.strip()
            val, i = _read_value(s, i + 1)
            if key:
                props[key] = val
            header = ""
            continue
        header += c
        i += 1
    return children, props, i


def _read_value(s: str, i: int) -> tuple[str, int]:
    """Read an assignment RHS: to end-of-line, respecting [] () and strings."""
    n = len(s)
    while i < n and s[i] in " \t":
        i += 1
    depth = 0
    quote = None
    buf = []
    while i < n:
        c = s[i]
        if quote:
            buf.append(c)
            if c == quote:
                quote = None
            i += 1
            continue
        if c in "\"'":
            quote = c
            buf.append(c)
            i += 1
            continue
        if c in "[(":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif depth == 0 and c == "\n":
            break
        elif depth == 0 and c == "}":
            break
        buf.append(c)
        i += 1
    return "".join(buf).strip(), i


def parse(text: str) -> list[Block]:
    """Parse `.ais` source into a list of top-level blocks (usually one project)."""
    s = strip_comments(text)
    # Wrap the top level in a synthetic body so `_parse_body` can drive it.
    children, _, _ = _parse_body(s + "}", _index_of_top(s))
    return children


def _index_of_top(s: str) -> int:
    """Return an index so `_parse_body(s + '}', k)` reads the whole file.

    We prepend nothing and start at 0; `_parse_body` treats the buffer as the
    inside of a block and stops at the trailing synthetic `}`.
    """
    return 0


# ── value helpers ────────────────────────────────────────────────────
def as_list(val: str | None) -> list[str]:
    """`[a, b.c, use m.x]` → ['a', 'b.c', 'use m.x'];  '' / None → []."""
    if not val:
        return []
    v = val.strip()
    if v.startswith("[") and v.endswith("]"):
        v = v[1:-1]
    parts = [p.strip() for p in v.split(",")]
    return [p for p in parts if p]


def ref_label(val: str | None) -> str | None:
    """`model.reasoner` → 'reasoner';  `prompt.orchestrator_sys` → 'orchestrator_sys'."""
    if not val:
        return None
    v = val.strip()
    return v.split(".", 1)[1] if "." in v else v


def unquote(val: str | None) -> str | None:
    if val is None:
        return None
    v = val.strip()
    if len(v) >= 2 and v[0] in "\"'" and v[-1] == v[0]:
        return v[1:-1]
    return v


def leading_doc(text: str) -> str:
    """Extract the file's leading `##` / `//` doc-comment block as prose.

    Stops at the first non-comment, non-blank line (typically `project`).
    """
    lines = []
    for raw in text.splitlines():
        line = raw.rstrip()
        stripped = line.strip()
        if not stripped:
            if lines:
                lines.append("")
            continue
        m = re.match(r"^(#+|//+)\s?(.*)$", stripped)
        if not m:
            break
        lines.append(m.group(2))
    # collapse trailing blanks
    while lines and not lines[-1]:
        lines.pop()
    return "\n".join(lines).strip()

"""aisVerbo — the LLM bridge: every model call goes through the aiSlang project.

One agent, one composed input, one reply. `aislang chat <agent> aisverbo.ais`
routes the call through the project's model block (LiteLLM proxy, budget caps,
tracing); we pipe the composed input on stdin and parse the last JSON object
out of the reply. No SDKs, no direct provider calls. Lifted verbatim from the
aisGTM example — the bridge is project-agnostic.
"""
from __future__ import annotations

import json
import os
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


class LlmError(RuntimeError):
    pass


def extract_last_json_object(text: str):
    """The last balanced top-level {...} in text (models wrap JSON in prose)."""
    found, depth, start, in_str, esc = None, 0, -1, False, False
    for i, ch in enumerate(text):
        if in_str:
            if esc:
                esc = False
            elif ch == "\\":
                esc = True
            elif ch == '"':
                in_str = False
            continue
        if ch == '"':
            in_str = True
        elif ch == "{":
            if depth == 0:
                start = i
            depth += 1
        elif ch == "}":
            if depth > 0:
                depth -= 1
                if depth == 0 and start >= 0:
                    try:
                        obj = json.loads(text[start:i + 1])
                        if isinstance(obj, dict):
                            found = obj
                    except json.JSONDecodeError:
                        pass
    return found


def extract_last_json_array(text: str):
    """The last balanced top-level [...] (the classifier returns a list)."""
    found, depth, start, in_str, esc = None, 0, -1, False, False
    for i, ch in enumerate(text):
        if in_str:
            if esc:
                esc = False
            elif ch == "\\":
                esc = True
            elif ch == '"':
                in_str = False
            continue
        if ch == '"':
            in_str = True
        elif ch == "[":
            if depth == 0:
                start = i
            depth += 1
        elif ch == "]":
            if depth > 0:
                depth -= 1
                if depth == 0 and start >= 0:
                    try:
                        obj = json.loads(text[start:i + 1])
                        if isinstance(obj, list):
                            found = obj
                    except json.JSONDecodeError:
                        pass
    return found


def chat(agent: str, payload: str, cfg: dict) -> str:
    """One turn against one agent of the aiSlang project; returns the reply text."""
    cmd = [cfg.get("aislang_bin", "aislang"), "chat", agent,
           cfg.get("project_ais", os.path.join(ROOT, "aisverbo.ais"))]
    try:
        proc = subprocess.run(
            cmd, input=payload, capture_output=True, text=True,
            timeout=int(cfg.get("chat_timeout", 240)), cwd=ROOT)
    except FileNotFoundError as exc:
        raise LlmError(f"aislang binary not found ({cmd[0]}) — set aislang_bin in the config") from exc
    except subprocess.TimeoutExpired as exc:
        raise LlmError(f"aislang chat {agent}: timed out") from exc
    if proc.returncode != 0:
        tail = (proc.stderr or proc.stdout or "").strip()[-400:]
        raise LlmError(f"aislang chat {agent}: exit {proc.returncode}: {tail}")
    return proc.stdout


def ask_json(agent: str, payload_obj, cfg: dict) -> dict:
    reply = chat(agent, json.dumps(payload_obj, ensure_ascii=False), cfg)
    obj = extract_last_json_object(reply)
    if obj is None:
        raise LlmError(f"agent {agent}: no JSON object in the reply")
    return obj


def ask_json_array(agent: str, payload_obj, cfg: dict) -> list:
    reply = chat(agent, json.dumps(payload_obj, ensure_ascii=False), cfg)
    arr = extract_last_json_array(reply)
    if arr is None:
        obj = extract_last_json_object(reply)   # tolerate {"categories": [...]}
        if isinstance(obj, dict):
            for v in obj.values():
                if isinstance(v, list):
                    return v
        raise LlmError(f"agent {agent}: no JSON array in the reply")
    return arr

"""aisVerbo — minimal JWT (HS256) with the standard library only.

One signing secret rules both token kinds:
  * TENANT tokens  {"tid": "<uuid>"}            — data partitioning
  * ADMIN tokens   {"role": "admin", "sub": …}  — corpus management

The secret comes from the AISVERBO_SECRET env var (a HuggingFace Space
secret in hosted mode); without it, a random one is generated once and
kept in <data dir>/secret.key, so local use works out of the box.
"""
from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
import secrets
import time

import db as dbmod

_SECRET_FILE = os.path.join(dbmod.GEN_DIR, "secret.key")


def secret() -> bytes:
    env = os.environ.get("AISVERBO_SECRET")
    if env:
        return env.encode("utf-8")
    try:
        with open(_SECRET_FILE, "rb") as fh:
            return fh.read().strip()
    except OSError:
        pass
    val = secrets.token_hex(32).encode("ascii")
    os.makedirs(os.path.dirname(_SECRET_FILE), exist_ok=True)
    with open(_SECRET_FILE, "wb") as fh:
        fh.write(val)
    os.chmod(_SECRET_FILE, 0o600)
    return val


def _b64(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def _unb64(s: str) -> bytes:
    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


def sign(payload: dict) -> str:
    header = _b64(json.dumps({"alg": "HS256", "typ": "JWT"}).encode())
    body = _b64(json.dumps(payload, separators=(",", ":")).encode())
    mac = hmac.new(secret(), f"{header}.{body}".encode(), hashlib.sha256).digest()
    return f"{header}.{body}.{_b64(mac)}"


def verify(token: str) -> dict | None:
    """The payload when the signature (and exp, if present) hold; else None."""
    try:
        header, body, sig = token.strip().split(".")
        mac = hmac.new(secret(), f"{header}.{body}".encode(), hashlib.sha256).digest()
        if not hmac.compare_digest(_b64(mac), sig):
            return None
        payload = json.loads(_unb64(body))
        if "exp" in payload and time.time() > float(payload["exp"]):
            return None
        return payload if isinstance(payload, dict) else None
    except Exception:
        return None

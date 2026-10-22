#!/usr/bin/env python3
"""aisVerbo — the web app (FastAPI): learn verb conjugation by story.

Routes (JSON unless noted) — unchanged contract from the stdlib version:
  GET  /                    the single-page UI (app/ui.html)
  POST /api/tenant/new      a TENANT token (multi-tenant mode mints a fresh
                            isolated data dir per token; local mode returns
                            the shared "local" tenant)
  GET  /api/state           settings + languages + counts
  POST /api/settings        (verb_count ≤ 100, essay_words ≤ 200 — clamped)
  GET/POST /api/verbs       corpus — POST requires the ADMIN token
  GET/POST /api/tenses      corpus — POST requires the ADMIN token
  POST /api/essay           background job        GET /api/jobs
  GET  /api/essays?q=       the library           GET /api/essay?id=
  POST /api/card            the grammarian card (DB-cached; force re-asks)
  POST /api/conjugation     one lemma in one tense (DB-cached)
  POST /api/translate       story (cached) or fragment translation
  GET  /api/export/essay    POST /api/import/essay      (idempotent)
  GET  /api/export/corpus   POST /api/import/corpus     (idempotent)

Tenancy: every request may carry `Authorization: Bearer <tenant JWT>`;
each tenant gets its own SQLite + config under generated/tenants/<tid>/.
Without a token (or in local mode) the legacy single-user paths serve.
Admin: corpus-mutating POSTs require `X-Admin-Token: <JWT role=admin>`
minted by `python3 app/get_token.py`.

PORT env (HuggingFace Docker Spaces sets 7860) or --port override;
AISVERBO_MULTI_TENANT=1 turns fresh-tenant minting on (the Space mode).
"""
from __future__ import annotations

import argparse
import datetime
import os
import sys
import threading
import uuid as uuidmod

from fastapi import Depends, FastAPI, Header, HTTPException, Query, Request
from fastapi.responses import FileResponse, JSONResponse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import auth     # noqa: E402
import db as dbmod      # noqa: E402
import llm              # noqa: E402

UI_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ui.html")
MULTI_TENANT = bool(os.environ.get("AISVERBO_MULTI_TENANT"))

_LOCK = threading.Lock()
JOBS: dict = {}          # tenant id -> {"vocab": {...}, "essay": {...}}


def _jobs_of(tid: str) -> dict:
    with _LOCK:
        jobs = JOBS.setdefault(tid, {})
        for name in ("vocab", "essay"):
            jobs.setdefault(name, {"running": False, "report": None, "error": None})
        return jobs


def _job_start(tid: str, name: str) -> bool:
    with _LOCK:
        jobs = JOBS.setdefault(tid, {})
        j = jobs.setdefault(name, {"running": False, "report": None, "error": None})
        if j["running"]:
            return False
        jobs[name] = {"running": True, "report": None, "error": None}
        return True


def _job_end(tid: str, name: str, report=None, error=None) -> None:
    with _LOCK:
        JOBS.setdefault(tid, {})[name] = {"running": False, "report": report,
                                          "error": error}


def now_iso() -> str:
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


# ─── agent drivers ───────────────────────────────────────────────────
VOCAB_BATCH = 100          # verbs per lexicographer call


def generate_verbs(conn, cfg: dict, count: int, lang: str | None = None) -> dict:
    """Top-`count` vocabulary, batched; already-stored verbs are excluded."""
    lang, native = lang or cfg["foreign"], cfg["native"]
    added = 0
    while added < count:
        have = [v["infinitive"] for v in dbmod.list_verbs(conn, lang)]
        batch = min(VOCAB_BATCH, count - added)
        items = llm.ask_json_array("lexicographer", {
            "task": "verbs", "lang": lang, "native": native,
            "count": batch, "exclude": have[-300:]}, cfg)
        n = dbmod.add_verbs(conn, lang, [i for i in items if isinstance(i, dict)])
        if n == 0:
            break               # the model has nothing new to offer
        added += n
    return {"lang": lang, "added": added,
            "total": len(dbmod.list_verbs(conn, lang))}


def generate_tenses(conn, cfg: dict, lang: str | None = None) -> dict:
    lang = lang or cfg["foreign"]
    items = llm.ask_json_array("tensesmith", {
        "task": "tenses", "lang": lang, "native": cfg["native"]}, cfg)
    n = dbmod.add_tenses(conn, lang, [i for i in items if isinstance(i, dict)])
    return {"lang": lang, "added": n,
            "total": len(dbmod.list_tenses(conn, lang))}


def generate_essay(conn, cfg: dict, topic: str,
                   verb_ids: list, tense_ids: list) -> dict:
    lang = cfg["foreign"]
    verbs = [v for v in dbmod.list_verbs(conn, lang) if v["id"] in set(verb_ids)]
    tenses = [t for t in dbmod.list_tenses(conn, lang) if t["id"] in set(tense_ids)]
    if not verbs:
        raise RuntimeError("select at least one verb for this story")
    if not tenses:
        raise RuntimeError("select at least one tense for this story")
    out = llm.ask_json("essayist", {
        "task": "essay", "lang": lang, "native": cfg["native"],
        "topic": topic, "tenses": [t["name"] for t in tenses],
        "verbs": [v["infinitive"] for v in verbs],
        "words": int(cfg.get("essay_words", 180))}, cfg)
    title = (out.get("title") or topic).strip()
    body = (out.get("body") or "").strip()
    if not body:
        raise RuntimeError("the essayist returned an empty story")
    eid = dbmod.save_essay(conn, {
        "created_at": now_iso(), "foreign_lang": lang,
        "native_lang": cfg["native"], "topic": topic, "title": title,
        "body": body,
        "verbs_csv": ",".join(v["infinitive"] for v in verbs),
        "tenses_csv": ",".join(t["name"] for t in tenses)})
    return {"id": eid, "title": title, "body": body,
            "verbs": [v["infinitive"] for v in verbs],
            "tenses": [t["name"] for t in tenses]}


def analyze_essay(conn, cfg: dict, cfg_path: str, text: str,
                  topic: str) -> dict:
    """The reverse of generate_essay: a pasted EXISTING text (classical
    literature, song lyrics) becomes a story. The philologist recreates the
    metadata a learner otherwise sets by hand — every verb lemma and every
    tense the text uses — with tense names canonicalized against the Corpus;
    verbs (and tenses) the Corpus lacks are added to it, so the story's
    metadata always references Corpus items."""
    lang, native = cfg["foreign"], cfg["native"]
    text = text.strip()
    words = len(text.split())
    if words > dbmod.MAX_ESSAY_WORDS:
        raise RuntimeError(
            f"the pasted text is {words} words — the limit is "
            f"{dbmod.MAX_ESSAY_WORDS}. Cut it down and paste the fragment "
            "again.")
    out = llm.ask_json("philologist", {
        "task": "analyze", "lang": lang, "native": native, "text": text,
        "known_tenses": [t["name"] for t in dbmod.list_tenses(conn, lang)]},
        cfg)
    verbs = [v for v in (out.get("verbs") or [])
             if isinstance(v, dict) and (v.get("infinitive") or "").strip()]
    if not verbs:
        raise RuntimeError("the philologist found no verbs in the pasted "
                           f"text — is it really in «{lang}»?")
    tenses: list[str] = []
    for name in out.get("tenses") or []:
        c = dbmod.canonical_tense(conn, lang, str(name))
        if c and c not in tenses:
            tenses.append(c)
    dbmod.add_verbs(conn, lang, verbs)                    # INSERT OR IGNORE
    dbmod.add_tenses(conn, lang, [{"name": t} for t in tenses])
    infs: list[str] = []
    for v in verbs:
        inf = v["infinitive"].strip().lower()
        if inf not in infs:
            infs.append(inf)
    if words > int(cfg.get("essay_words", 180)):
        cfg["essay_words"] = words      # Setup reflects the actual length
        dbmod.save_config(cfg, cfg_path)
    title = (topic or out.get("title") or "").strip() \
        or " ".join(text.split()[:6])
    eid = dbmod.save_essay(conn, {
        "created_at": now_iso(), "foreign_lang": lang,
        "native_lang": native, "topic": topic or title, "title": title,
        "body": text,
        "verbs_csv": ",".join(infs), "tenses_csv": ",".join(tenses)})
    return {"id": eid, "title": title, "body": text,
            "verbs": infs, "tenses": tenses}


def word_card(conn, cfg: dict, word: str, sentence: str,
              force: bool = False) -> dict:
    lang, native = cfg["foreign"], cfg["native"]
    key = {"word": word.lower(), "foreign_lang": lang, "native_lang": native}
    cached = None if force else dbmod.get_card(conn, word, lang, native)
    if cached:
        cached.pop("conjugation", None)       # pre-v3 cards may still carry it
        if cached.get("tense"):               # pre-canonicalization casing
            cached["tense"] = dbmod.canonical_tense(conn, lang, cached["tense"])
        return {"cached": True, **cached, **key}
    card = llm.ask_json("grammarian", {
        "task": "card", "word": word, "sentence": sentence,
        "lang": lang, "native": native}, cfg)
    card.setdefault("word", word)
    # conjugation tables live ONLY in the conjugations table (one row per
    # lemma+tense+langs, shared by every card and story)
    card.pop("conjugation", None)
    # the card's tense references the Corpus item, never a re-spelling of it
    # («Presente de indicativo» → the corpus's «presente de indicativo»)
    if card.get("tense"):
        card["tense"] = dbmod.canonical_tense(conn, lang, card["tense"])
    dbmod.save_card(conn, word, lang, native, card, now_iso())
    return {"cached": False, **card, **key}


def _plain(text: str) -> str:
    """Translation metadata is PLAIN prose: no markdown markers, no literal
    escape sequences, no line breaks — one continuous block. (The UI derives
    the paragraph layout from the STORY via the select_subs mapping.)"""
    t = (text or "").replace("**", "").replace("\\n", " ")
    for ch in ("*", "`", "#", "_"):
        t = t.replace(ch, "")
    return " ".join(t.split())


def translate(conn, cfg: dict, to: str, essay_id: int | None = None,
              text: str | None = None, force: bool = False) -> dict:
    """Whole-story translation = TWO items per language (the title and the
    body, plain text — markdown markers never enter the metadata), cached;
    a selected fragment is translated ad hoc and not stored. The story ↔
    translation substring mapping is deterministic and lives in the UI
    (sentence-granular proportional mapping, bijective both ways)."""
    title = ""
    if essay_id:
        if not force:
            cached = dbmod.get_translation(conn, essay_id, to)
            if isinstance(cached, dict) and "body" in cached:
                return {"cached": True, **cached}
        r = conn.execute("SELECT title, body FROM essays WHERE id=?",
                         (essay_id,)).fetchone()
        if not r:
            raise RuntimeError("unknown essay")
        title, text = _plain(r["title"]), _plain(r["body"])
    if not (text or "").strip():
        raise RuntimeError("nothing to translate")
    out = llm.ask_json("translator", {
        "task": "translate", "title": title, "text": _plain(text),
        "lang": cfg["foreign"], "to": to}, cfg)
    result = {"title": _plain(out.get("title") or ""),
              "body": _plain(out.get("body") or "")}
    if not result["body"].strip():
        raise RuntimeError("the translator returned an empty translation")
    if essay_id:
        dbmod.save_translation(conn, essay_id, to, result, now_iso())
    return {"cached": False, **result}


def conjugation(conn, cfg: dict, lemma: str, tense: str,
                force: bool = False) -> dict:
    """All six persons of one lemma in one tense, with examples — cached
    DB-wide, so every story and word card shares one model call."""
    lang, native = cfg["foreign"], cfg["native"]
    tense = dbmod.canonical_tense(conn, lang, tense)
    key = {"lemma": lemma.lower(), "tense": tense,
           "foreign_lang": lang, "native_lang": native}
    cached = None if force else dbmod.get_conjugation(conn, lemma, tense,
                                                      lang, native)
    if cached:
        return {"cached": True, **cached, **key}
    data = llm.ask_json("grammarian", {
        "task": "conjugation", "lemma": lemma, "tense": tense,
        "lang": lang, "native": native}, cfg)
    dbmod.save_conjugation(conn, lemma, tense, lang, native, data, now_iso())
    return {"cached": False, **data, **key}


# ─── tenancy + auth (FastAPI dependencies) ───────────────────────────
class Ctx:
    def __init__(self, tid: str, user: str | None = None):
        self.tid = tid
        self.user = user
        self.db_path, self.cfg_path = dbmod.tenant_paths(tid)

    def conn(self):
        return dbmod.connect(self.db_path)

    def cfg(self) -> dict:
        return dbmod.load_config(self.cfg_path)


def get_ctx(authorization: str | None = Header(None)) -> Ctx:
    """Tenant = the signed-in identity; everyone else shares PUBLIC.

    A signed token (minted by the HF OAuth callback) carries the user's
    tenant; without one — anonymous Space visitors and local runs alike —
    the shared public profile serves: its corpus and stories are readable
    and extendable by everyone, and nothing there is private."""
    if authorization and authorization.lower().startswith("bearer "):
        payload = auth.verify(authorization[7:])
        tid = (payload or {}).get("tid")
        if tid:
            try:
                return Ctx(str(tid), (payload or {}).get("user"))
            except ValueError:
                raise HTTPException(401, "bad tenant token")
    return Ctx("public")


# ─── roles ────────────────────────────────────────────────────────────
# corpus_admin  — manage verbs/tenses, import a corpus, revoke tokens
# story_writer  — generate/delete/import stories, trigger LLM extras
#                 (uncached word cards, conjugations, translations)
# story_reader  — the DEFAULT, tokenless: read everything, generate nothing
# The legacy "admin" role is honored as both. A token's `sub` names its
# holder: it is refused unless sub matches the signed-in HF user (or
# "public" for the anonymous shared profile) — the HF sign-in IS the
# identity verification, no e-mail loop needed. `jti` enables revocation.
_ONBOARD_HINT = ("click the `Request corpus access (onboarding)` button "
                 "in Setup")


def _token_state(x_admin_token: str | None, ctx: "Ctx") -> tuple[dict | None, str]:
    if not x_admin_token:
        return None, f"no access token reached the server — {_ONBOARD_HINT}"
    payload = auth.verify(x_admin_token)
    if payload is None:
        return None, ("the access token's signature does not match THIS "
                      "server's secret, or it expired — request a fresh one "
                      f"({_ONBOARD_HINT})")
    holder = payload.get("sub") or "public"
    me = ctx.user or "public"
    if holder not in (me, "owner"):       # pre-RBAC owner tokens stay valid
        return None, (f"this token belongs to '{holder}' but you are "
                      f"'{me}' — tokens are per HF login; request your own "
                      f"({_ONBOARD_HINT})")
    if dbmod.is_revoked(payload.get("jti")):
        return None, "this token has been revoked"
    return payload, "ok"


def _roles_of(payload: dict | None) -> set:
    if not payload:
        return set()
    role = payload.get("role")
    if role in ("admin", "all"):          # the operator token: both powers
        return {"corpus_admin", "story_writer"}
    return {role} if role in ("corpus_admin", "story_writer") else set()


def require_role(role: str, label: str):
    def dep(x_admin_token: str | None = Header(None),
            ctx: Ctx = Depends(get_ctx)) -> dict:
        payload, why = _token_state(x_admin_token, ctx)
        if payload is None:
            raise HTTPException(401, f"{label} needs an access token: {why}")
        if role not in _roles_of(payload):
            raise HTTPException(403, f"{label} needs the {role} role — this "
                                     f"token's role is '{payload.get('role')}'")
        return payload
    return dep


require_admin = require_role("corpus_admin", "corpus management")
require_writer = require_role("story_writer", "story creation")


def _is_writer(x_admin_token: str | None, ctx: "Ctx") -> bool:
    payload, _ = _token_state(x_admin_token, ctx)
    return "story_writer" in _roles_of(payload)


def _reader_miss_error(x_admin_token: str | None, ctx: "Ctx") -> HTTPException:
    """When generation is refused, say WHY the presented token (if any) did
    not count as a writer — a sub mismatch must not masquerade as a
    missing-role situation."""
    if x_admin_token:
        payload, why = _token_state(x_admin_token, ctx)
        if payload is None:
            return HTTPException(401, f"your token was not accepted: {why}")
        return HTTPException(403, "generating needs the story_writer role — "
                                  f"this token's role is '{payload.get('role')}'")
    return HTTPException(401, _READER_MISS)


app = FastAPI(title="aisVerbo", docs_url=None, redoc_url=None)


@app.exception_handler(llm.LlmError)
async def _llm_err(request: Request, exc: llm.LlmError):
    return JSONResponse(status_code=502, content={"error": str(exc)})


@app.exception_handler(RuntimeError)
async def _runtime_err(request: Request, exc: RuntimeError):
    return JSONResponse(status_code=400, content={"error": str(exc)})


@app.exception_handler(HTTPException)
async def _http_err(request: Request, exc: HTTPException):
    return JSONResponse(status_code=exc.status_code, content={"error": exc.detail})


@app.get("/")
def ui():
    return FileResponse(UI_PATH, media_type="text/html")


# ─── HF OAuth: tenant = the Hugging Face user, not the browser ────────
def _oauth_env():
    cid = os.environ.get("OAUTH_CLIENT_ID")
    sec = os.environ.get("OAUTH_CLIENT_SECRET")
    provider = os.environ.get("OPENID_PROVIDER_URL", "https://huggingface.co")
    return (cid, sec, provider) if cid and sec else None


def _exchange_code(code: str, redirect_uri: str) -> str:
    """OAuth code -> the HF username (split out for tests)."""
    import base64
    import json as jsonmod
    import urllib.parse
    import urllib.request
    cid, sec, provider = _oauth_env()
    basic = base64.b64encode(f"{cid}:{sec}".encode()).decode()
    req = urllib.request.Request(
        f"{provider}/oauth/token",
        data=urllib.parse.urlencode({
            "grant_type": "authorization_code", "code": code,
            "redirect_uri": redirect_uri}).encode(),
        headers={"Authorization": f"Basic {basic}",
                 "Content-Type": "application/x-www-form-urlencoded"})
    with urllib.request.urlopen(req, timeout=20) as resp:
        access = jsonmod.loads(resp.read())["access_token"]
    req2 = urllib.request.Request(f"{provider}/oauth/userinfo",
                                  headers={"Authorization": f"Bearer {access}"})
    with urllib.request.urlopen(req2, timeout=20) as resp:
        info = jsonmod.loads(resp.read())
    return info.get("preferred_username") or info.get("name") or info["sub"]


def _redirect_uri(request: Request) -> str:
    host = os.environ.get("SPACE_HOST") or request.headers.get(
        "x-forwarded-host") or request.url.netloc
    return f"https://{host}/oauth/callback"


@app.get("/oauth/login")
def oauth_login(request: Request):
    import urllib.parse
    env = _oauth_env()
    if not env:
        raise HTTPException(404, "OAuth is not configured on this install")
    cid, _sec, provider = env
    from fastapi.responses import RedirectResponse
    q = urllib.parse.urlencode({
        "client_id": cid, "redirect_uri": _redirect_uri(request),
        "response_type": "code", "scope": "openid profile",
        "state": "aisverbo"})
    return RedirectResponse(f"{provider}/oauth/authorize?{q}")


@app.get("/oauth/callback")
def oauth_callback(request: Request, code: str = Query("")):
    from fastapi.responses import HTMLResponse
    if not code or not _oauth_env():
        raise HTTPException(400, "missing code")
    username = _exchange_code(code, _redirect_uri(request))
    tid = "hf-" + __import__("re").sub(r"[^A-Za-z0-9_-]", "-", username)[:40]
    token = auth.sign({"tid": tid, "user": username})
    return HTMLResponse(f"""<!doctype html><meta charset="utf-8">
<script>
  localStorage.setItem('aisverbo-tenant', {json_dumps(token)});
  location.replace('/');
</script>signed in — returning…""")


def json_dumps(s):
    import json as jsonmod
    return jsonmod.dumps(s)


@app.get("/api/auth/check")
def auth_check(x_admin_token: str | None = Header(None),
               ctx: Ctx = Depends(get_ctx)):
    """Zero-secret diagnostics: did the token arrive, and what may it do?"""
    payload, why = _token_state(x_admin_token, ctx)
    return {"tenant": ctx.tid, "user": ctx.user,
            "token_received": bool(x_admin_token),
            "token_length": len(x_admin_token or ""),
            "ok": payload is not None,
            "roles": sorted(_roles_of(payload)),
            "detail": why,
            "sub": (payload or {}).get("sub"),
            "jti": (payload or {}).get("jti")}


@app.get("/api/state")
def state(ctx: Ctx = Depends(get_ctx),
          x_admin_token: str | None = Header(None)):
    conn, cfg = ctx.conn(), ctx.cfg()
    payload, _why = _token_state(x_admin_token, ctx)
    roles = _roles_of(payload)
    return {"settings": cfg, "languages": dbmod.all_languages(cfg),
            "tenant": ctx.tid, "user": ctx.user,
            "can_write": "story_writer" in roles,
            "can_admin": "corpus_admin" in roles,
            "oauth_available": _oauth_env() is not None,
            "multi_tenant": MULTI_TENANT,
            "verbs": len(dbmod.list_verbs(conn, cfg["foreign"])),
            "tenses": len(dbmod.list_tenses(conn, cfg["foreign"])),
            "essays": conn.execute(
                "SELECT COUNT(*) FROM essays WHERE foreign_lang=?",
                (cfg["foreign"],)).fetchone()[0]}


@app.post("/api/settings")
def settings(body: dict, ctx: Ctx = Depends(get_ctx)):
    return dbmod.save_config({**ctx.cfg(), **body}, ctx.cfg_path)


@app.get("/api/verbs")
def verbs(lang: str | None = None, ctx: Ctx = Depends(get_ctx)):
    return dbmod.list_verbs(ctx.conn(), lang or ctx.cfg()["foreign"])


@app.get("/api/tenses")
def tenses(lang: str | None = None, ctx: Ctx = Depends(get_ctx)):
    return dbmod.list_tenses(ctx.conn(), lang or ctx.cfg()["foreign"])


def _vocab_job(ctx: Ctx, what: str, count: int, lang: str | None) -> None:
    try:
        conn, cfg = ctx.conn(), ctx.cfg()
        if what == "verbs":
            report = generate_verbs(conn, cfg, count, lang)
        else:
            report = generate_tenses(conn, cfg, lang)
        _job_end(ctx.tid, "vocab", report={"what": what, **report})
    except Exception as exc:
        _job_end(ctx.tid, "vocab", error=f"{type(exc).__name__}: {exc}")


@app.post("/api/verbs")
def verbs_post(body: dict, ctx: Ctx = Depends(get_ctx),
               admin: dict = Depends(require_admin)):
    conn, cfg = ctx.conn(), ctx.cfg()
    act = body.get("action")
    if act == "generate":
        if not _job_start(ctx.tid, "vocab"):
            raise HTTPException(409, "a generation is already running")
        count = min(int(body.get("count") or cfg.get("verb_count", 100)),
                    dbmod.MAX_VERB_COUNT)
        threading.Thread(target=_vocab_job,
                         args=(ctx, "verbs", count, body.get("lang")),
                         daemon=True).start()
        return {"started": True, "count": count}
    if act == "add":
        return {"added": dbmod.add_verbs(conn, body.get("lang") or cfg["foreign"],
                                         [{"infinitive": body.get("infinitive"),
                                           "gloss": body.get("gloss", "")}])}
    if act == "update":
        dbmod.update_verb(conn, int(body.get("id") or 0),
                          body.get("infinitive") or "", body.get("gloss") or "")
        return {"ok": True}
    if act == "delete":
        r = conn.execute("SELECT lang, infinitive FROM verbs WHERE id=?",
                         (int(body.get("id") or 0),)).fetchone()
        if r:
            n = dbmod.verb_references(conn, r["lang"], r["infinitive"])
            if n:
                raise HTTPException(409, f"'{r['infinitive']}' is used by {n} "
                                    "stored story(ies) — keep it, or delete "
                                    "those stories first.")
            conn.execute("DELETE FROM verbs WHERE id=?", (int(body.get("id")),))
            conn.commit()
        return {"ok": True}
    raise HTTPException(400, "unknown action")


@app.post("/api/tenses")
def tenses_post(body: dict, ctx: Ctx = Depends(get_ctx),
                admin: dict = Depends(require_admin)):
    conn, cfg = ctx.conn(), ctx.cfg()
    act = body.get("action")
    if act == "generate":
        if not _job_start(ctx.tid, "vocab"):
            raise HTTPException(409, "a generation is already running")
        threading.Thread(target=_vocab_job,
                         args=(ctx, "tenses", 0, body.get("lang")),
                         daemon=True).start()
        return {"started": True}
    if act == "toggle":
        conn.execute("UPDATE tenses SET enabled=1-enabled WHERE id=?",
                     (int(body.get("id") or 0),))
        conn.commit()
        return {"ok": True}
    if act == "delete":
        r = conn.execute("SELECT lang, name FROM tenses WHERE id=?",
                         (int(body.get("id") or 0),)).fetchone()
        if r:
            n = dbmod.tense_references(conn, r["lang"], r["name"])
            if n:
                raise HTTPException(409, f"'{r['name']}' is used by {n} stored "
                                    "story(ies) — keep it, or delete those first.")
            conn.execute("DELETE FROM tenses WHERE id=?", (int(body.get("id")),))
            conn.commit()
        return {"ok": True}
    raise HTTPException(400, "unknown action")


def _prefill_conjugations(ctx: Ctx, verbs: list, tenses: list) -> None:
    """Warm the conjugations cache for every (verb, tense) the story drills,
    so each Word card's table is a plain DB read — best effort, skips what
    is already cached, never blocks the story report. Capped: a pasted
    classic can carry dozens of verbs × tenses, and every cache miss is a
    model call — the rest fills lazily as cards are opened."""
    try:
        conn, cfg = ctx.conn(), ctx.cfg()
        budget = 60
        for v in verbs:
            for t in tenses:
                if budget <= 0:
                    return
                try:
                    if dbmod.get_conjugation(conn, v, t, cfg["foreign"],
                                             cfg["native"]) is None:
                        budget -= 1
                        conjugation(conn, cfg, v, t)
                except Exception:
                    pass                    # one miss never stops the rest
    except Exception:
        pass


def _essay_job(ctx: Ctx, topic: str, verb_ids: list, tense_ids: list,
               text: str = "") -> None:
    try:
        if text.strip():        # pasted existing text → analyze, don't write
            report = analyze_essay(ctx.conn(), ctx.cfg(), ctx.cfg_path,
                                   text, topic)
        else:
            report = generate_essay(ctx.conn(), ctx.cfg(), topic,
                                    verb_ids, tense_ids)
        _job_end(ctx.tid, "essay", report=report)
        threading.Thread(target=_prefill_conjugations,
                         args=(ctx, report.get("verbs") or [],
                               report.get("tenses") or []),
                         daemon=True).start()
    except Exception as exc:
        _job_end(ctx.tid, "essay", error=f"{type(exc).__name__}: {exc}")


@app.post("/api/essay")
def essay_post(body: dict, ctx: Ctx = Depends(get_ctx),
               writer: dict = Depends(require_writer)):
    topic = (body.get("topic") or "").strip()
    text = (body.get("text") or "").strip()
    if not topic and not text:
        raise HTTPException(400, "give a topic — or paste an existing text "
                                 "into the story area")
    if text and len(text.split()) > dbmod.MAX_ESSAY_WORDS:
        raise HTTPException(400, f"the pasted text is {len(text.split())} "
                            f"words — the limit is {dbmod.MAX_ESSAY_WORDS}. "
                            "Cut it down and paste the fragment again.")
    if not _job_start(ctx.tid, "essay"):
        raise HTTPException(409, "a story is already being written")
    threading.Thread(target=_essay_job,
                     args=(ctx, topic, body.get("verbs") or [],
                           body.get("tenses") or [], text),
                     daemon=True).start()
    return {"started": True}


@app.get("/api/jobs")
def jobs(ctx: Ctx = Depends(get_ctx)):
    return _jobs_of(ctx.tid)


@app.get("/api/essays")
def essays(q: str = "", ctx: Ctx = Depends(get_ctx)):
    return dbmod.search_essays(ctx.conn(), ctx.cfg()["foreign"], q)


@app.get("/api/essay")
def essay_get(id: int = Query(...), ctx: Ctx = Depends(get_ctx)):
    r = ctx.conn().execute("SELECT * FROM essays WHERE id=?", (id,)).fetchone()
    if not r:
        raise HTTPException(404, "unknown essay")
    return dict(r)


_READER_MISS = ("this needs a model call, and generating anything needs "
                "the Story Writer role — cached results stay free; "
                + _ONBOARD_HINT)


@app.post("/api/card")
def card(body: dict, ctx: Ctx = Depends(get_ctx),
         x_admin_token: str | None = Header(None)):
    word = (body.get("word") or "").strip()
    if not word:
        raise HTTPException(400, "word required")
    conn, cfg = ctx.conn(), ctx.cfg()
    if not _is_writer(x_admin_token, ctx):
        cached = dbmod.get_card(conn, word, cfg["foreign"], cfg["native"])
        if cached is None:
            raise _reader_miss_error(x_admin_token, ctx)
        cached.pop("conjugation", None)
        if cached.get("tense"):
            cached["tense"] = dbmod.canonical_tense(conn, cfg["foreign"],
                                                    cached["tense"])
        return {"cached": True, **cached,
                "word": cached.get("word", word.lower()),
                "foreign_lang": cfg["foreign"], "native_lang": cfg["native"]}
    return word_card(conn, cfg, word, body.get("sentence") or "",
                     force=bool(body.get("force")))


@app.post("/api/conjugation")
def conjugation_post(body: dict, ctx: Ctx = Depends(get_ctx),
                     x_admin_token: str | None = Header(None)):
    lemma = (body.get("lemma") or "").strip()
    tense = (body.get("tense") or "").strip()
    if not lemma or not tense:
        raise HTTPException(400, "lemma + tense required")
    conn, cfg = ctx.conn(), ctx.cfg()
    if not _is_writer(x_admin_token, ctx):
        tense = dbmod.canonical_tense(conn, cfg["foreign"], tense)
        cached = dbmod.get_conjugation(conn, lemma, tense,
                                       cfg["foreign"], cfg["native"])
        if cached is None:
            raise _reader_miss_error(x_admin_token, ctx)
        return {"cached": True, **cached, "lemma": lemma.lower(), "tense": tense,
                "foreign_lang": cfg["foreign"], "native_lang": cfg["native"]}
    return conjugation(conn, cfg, lemma, tense, force=bool(body.get("force")))


@app.post("/api/translate")
def translate_post(body: dict, ctx: Ctx = Depends(get_ctx),
                   x_admin_token: str | None = Header(None)):
    conn, cfg = ctx.conn(), ctx.cfg()
    to = (body.get("to") or cfg["native"])[:8]
    if not _is_writer(x_admin_token, ctx):
        eid = body.get("essay_id")
        cached = dbmod.get_translation(conn, eid, to) if eid else None
        if not (isinstance(cached, dict) and "body" in cached):
            raise _reader_miss_error(x_admin_token, ctx)
        return {"cached": True, **cached}
    return translate(conn, cfg, to, essay_id=body.get("essay_id"),
                     text=body.get("text"), force=bool(body.get("force")))


@app.get("/api/export/corpus")
def export_corpus(lang: str | None = None, ctx: Ctx = Depends(get_ctx)):
    return dbmod.export_corpus(ctx.conn(), lang or ctx.cfg()["foreign"])


@app.post("/api/import/corpus")
def import_corpus(body: dict, ctx: Ctx = Depends(get_ctx),
                  admin: dict = Depends(require_admin)):
    return dbmod.import_corpus(ctx.conn(), body)


@app.get("/api/export/essay")
def export_essay(id: int = Query(...), ctx: Ctx = Depends(get_ctx)):
    return dbmod.export_essay(ctx.conn(), id)


@app.post("/api/import/essay")
def import_essay(body: dict, ctx: Ctx = Depends(get_ctx),
                 writer: dict = Depends(require_writer)):
    return dbmod.import_essay(ctx.conn(), body)


@app.post("/api/essay/delete")
def essay_delete(body: dict, ctx: Ctx = Depends(get_ctx),
                 writer: dict = Depends(require_writer)):
    n = dbmod.delete_essay(ctx.conn(), int(body.get("id") or 0))
    return {"deleted": n}


@app.post("/api/admin/revoke")
def admin_revoke(body: dict, admin: dict = Depends(require_admin)):
    jti = (body.get("jti") or "").strip()
    if body.get("token"):
        p = auth.verify(body["token"]) or {}
        jti = p.get("jti") or jti
    if not jti:
        raise HTTPException(400, "give token or jti")
    dbmod.revoke_jti(jti, now_iso())
    return {"revoked": jti}


def main() -> int:
    import uvicorn
    ap = argparse.ArgumentParser(description="aisVerbo web app (FastAPI).")
    ap.add_argument("--port", type=int, default=int(os.environ.get("PORT", 8787)))
    ap.add_argument("--host", default=os.environ.get("HOST", "127.0.0.1"))
    args = ap.parse_args()
    print(f"aisVerbo → http://{args.host}:{args.port}/"
          + ("  [multi-tenant]" if MULTI_TENANT else "  [local]"))
    uvicorn.run(app, host=args.host, port=args.port, log_level="warning")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

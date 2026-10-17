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


def word_card(conn, cfg: dict, word: str, sentence: str,
              force: bool = False) -> dict:
    lang, native = cfg["foreign"], cfg["native"]
    cached = None if force else dbmod.get_card(conn, word, lang, native)
    if cached:
        return {"cached": True, **cached}
    card = llm.ask_json("grammarian", {
        "task": "card", "word": word, "sentence": sentence,
        "lang": lang, "native": native}, cfg)
    card.setdefault("word", word)
    dbmod.save_card(conn, word, lang, native, card, now_iso())
    return {"cached": False, **card}


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
    cached = None if force else dbmod.get_conjugation(conn, lemma, tense,
                                                      lang, native)
    if cached:
        return {"cached": True, **cached}
    data = llm.ask_json("grammarian", {
        "task": "conjugation", "lemma": lemma, "tense": tense,
        "lang": lang, "native": native}, cfg)
    data.setdefault("lemma", lemma)
    data.setdefault("tense", tense)
    dbmod.save_conjugation(conn, lemma, tense, lang, native, data, now_iso())
    return {"cached": False, **data}


# ─── tenancy + auth (FastAPI dependencies) ───────────────────────────
class Ctx:
    def __init__(self, tid: str):
        self.tid = tid
        self.db_path, self.cfg_path = dbmod.tenant_paths(tid)

    def conn(self):
        return dbmod.connect(self.db_path)

    def cfg(self) -> dict:
        return dbmod.load_config(self.cfg_path)


def get_ctx(authorization: str | None = Header(None)) -> Ctx:
    if authorization and authorization.lower().startswith("bearer "):
        payload = auth.verify(authorization[7:])
        tid = (payload or {}).get("tid")
        if tid:
            try:
                return Ctx(str(tid))
            except ValueError:
                raise HTTPException(401, "bad tenant token")
    if MULTI_TENANT:
        raise HTTPException(401, "tenant token required — POST /api/tenant/new")
    return Ctx("local")


def _admin_state(x_admin_token: str | None) -> tuple[dict | None, str]:
    """(payload, diagnostic) — the diagnostic names the EXACT failure so a
    hosted deployment can be debugged from the browser alone."""
    if not x_admin_token:
        return None, ("no ADMIN token reached the server — paste one into "
                      "Setup (mint: `python3 app/get_token.py`)")
    payload = auth.verify(x_admin_token)
    if payload is None:
        return None, ("the ADMIN token's signature does not match THIS "
                      "server's secret (or it expired) — mint it with the "
                      "same secret the server runs with: "
                      "AISVERBO_SECRET='<the Space secret>' "
                      "python3 app/get_token.py")
    if payload.get("role") != "admin":
        return None, "the token is valid but its role is not 'admin'"
    return payload, "ok"


def require_admin(x_admin_token: str | None = Header(None)) -> dict:
    payload, why = _admin_state(x_admin_token)
    if payload is None:
        raise HTTPException(401, f"corpus management needs the ADMIN token: {why}")
    return payload


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


@app.post("/api/tenant/new")
def tenant_new():
    tid = str(uuidmod.uuid4()) if MULTI_TENANT else "local"
    return {"token": auth.sign({"tid": tid}), "tid": tid,
            "multi_tenant": MULTI_TENANT}


@app.get("/api/auth/check")
def auth_check(x_admin_token: str | None = Header(None),
               ctx: Ctx = Depends(get_ctx)):
    """Zero-secret diagnostics: did the admin token arrive, and is it valid?"""
    payload, why = _admin_state(x_admin_token)
    return {"tenant": ctx.tid,
            "admin_token_received": bool(x_admin_token),
            "admin_token_length": len(x_admin_token or ""),
            "admin_ok": payload is not None,
            "detail": why,
            "sub": (payload or {}).get("sub")}


@app.get("/api/state")
def state(ctx: Ctx = Depends(get_ctx)):
    conn, cfg = ctx.conn(), ctx.cfg()
    return {"settings": cfg, "languages": dbmod.all_languages(cfg),
            "tenant": ctx.tid, "multi_tenant": MULTI_TENANT,
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


def _essay_job(ctx: Ctx, topic: str, verb_ids: list, tense_ids: list) -> None:
    try:
        report = generate_essay(ctx.conn(), ctx.cfg(), topic, verb_ids, tense_ids)
        _job_end(ctx.tid, "essay", report=report)
    except Exception as exc:
        _job_end(ctx.tid, "essay", error=f"{type(exc).__name__}: {exc}")


@app.post("/api/essay")
def essay_post(body: dict, ctx: Ctx = Depends(get_ctx)):
    topic = (body.get("topic") or "").strip()
    if not topic:
        raise HTTPException(400, "give a topic")
    if not _job_start(ctx.tid, "essay"):
        raise HTTPException(409, "a story is already being written")
    threading.Thread(target=_essay_job,
                     args=(ctx, topic, body.get("verbs") or [],
                           body.get("tenses") or []),
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


@app.post("/api/card")
def card(body: dict, ctx: Ctx = Depends(get_ctx)):
    word = (body.get("word") or "").strip()
    if not word:
        raise HTTPException(400, "word required")
    return word_card(ctx.conn(), ctx.cfg(), word, body.get("sentence") or "",
                     force=bool(body.get("force")))


@app.post("/api/conjugation")
def conjugation_post(body: dict, ctx: Ctx = Depends(get_ctx)):
    lemma = (body.get("lemma") or "").strip()
    tense = (body.get("tense") or "").strip()
    if not lemma or not tense:
        raise HTTPException(400, "lemma + tense required")
    return conjugation(ctx.conn(), ctx.cfg(), lemma, tense,
                       force=bool(body.get("force")))


@app.post("/api/translate")
def translate_post(body: dict, ctx: Ctx = Depends(get_ctx)):
    return translate(ctx.conn(), ctx.cfg(),
                     (body.get("to") or ctx.cfg()["native"])[:8],
                     essay_id=body.get("essay_id"), text=body.get("text"),
                     force=bool(body.get("force")))


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
def import_essay(body: dict, ctx: Ctx = Depends(get_ctx)):
    return dbmod.import_essay(ctx.conn(), body)


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

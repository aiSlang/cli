#!/usr/bin/env python3
"""aisVerbo — the web app: learn verb conjugation by story.

Routes (JSON unless noted):
  GET  /                               the single-page UI (app/ui.html)
  GET  /api/state                      settings + languages + counts
  POST /api/settings  {foreign,native,verb_count,essay_words,...}
  GET  /api/verbs                      vocabulary for the chosen language
  POST /api/verbs     {action: generate|add|toggle|delete, ...}  (generate: background)
  GET  /api/tenses                     tense inventory for the chosen language
  POST /api/tenses    {action: generate|toggle}                  (generate: background)
  POST /api/essay     {topic, verbs:[ids], tenses:[ids]}         (background)
  GET  /api/essays?q=<word or phrase>  the library, searchable
  GET  /api/essay?id=N                 one stored essay
  POST /api/card      {word, sentence}  the grammarian's card (cached)
  GET  /api/jobs                       background job states

Local only by default; `PORT` env (HuggingFace Docker Spaces sets 7860) or
--port override. Reading aloud happens in the BROWSER (speechSynthesis) —
no server audio, no third-party TTS bill.
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import sys
import threading
import traceback
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import db as dbmod      # noqa: E402
import llm              # noqa: E402

UI_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ui.html")

_LOCK = threading.Lock()
JOBS = {
    "vocab": {"running": False, "report": None, "error": None},
    "essay": {"running": False, "report": None, "error": None},
}


def now_iso() -> str:
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


def _job_start(name: str) -> bool:
    with _LOCK:
        if JOBS[name]["running"]:
            return False
        JOBS[name] = {"running": True, "report": None, "error": None}
        return True


def _job_end(name: str, report=None, error=None) -> None:
    with _LOCK:
        JOBS[name] = {"running": False, "report": report, "error": error}


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
    return (text or "").replace("**", "")


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


# ─── background jobs ─────────────────────────────────────────────────
def _vocab_job(what: str, count: int, lang: str | None = None) -> None:
    try:
        cfg = dbmod.load_config()
        conn = dbmod.connect()
        if what == "verbs":
            report = generate_verbs(conn, cfg, count, lang)
        else:
            report = generate_tenses(conn, cfg, lang)
        _job_end("vocab", report={"what": what, **report})
    except Exception as exc:
        _job_end("vocab", error=f"{type(exc).__name__}: {exc}")


def _essay_job(topic: str, verb_ids: list, tense_ids: list) -> None:
    try:
        cfg = dbmod.load_config()
        conn = dbmod.connect()
        report = generate_essay(conn, cfg, topic, verb_ids, tense_ids)
        _job_end("essay", report=report)
    except Exception as exc:
        _job_end("essay", error=f"{type(exc).__name__}: {exc}")


# ─── HTTP ────────────────────────────────────────────────────────────
class Handler(BaseHTTPRequestHandler):
    server_version = "aisVerbo/0.1"

    def _send(self, code: int, body, ctype="application/json; charset=utf-8"):
        data = body if isinstance(body, (bytes, bytearray)) else \
            json.dumps(body, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _body(self) -> dict:
        n = int(self.headers.get("Content-Length") or 0)
        if not n:
            return {}
        try:
            return json.loads(self.rfile.read(n).decode("utf-8"))
        except json.JSONDecodeError:
            return {}

    def log_message(self, *a):            # quiet
        pass

    def do_GET(self):
        url = urllib.parse.urlparse(self.path)
        q = urllib.parse.parse_qs(url.query)
        try:
            if url.path in ("/", "/index.html"):
                with open(UI_PATH, "rb") as fh:
                    return self._send(200, fh.read(), "text/html; charset=utf-8")
            conn = dbmod.connect()
            cfg = dbmod.load_config()
            if url.path == "/api/state":
                return self._send(200, {
                    "settings": cfg, "languages": dbmod.all_languages(cfg),
                    "verbs": len(dbmod.list_verbs(conn, cfg["foreign"])),
                    "tenses": len(dbmod.list_tenses(conn, cfg["foreign"])),
                    "essays": conn.execute(
                        "SELECT COUNT(*) FROM essays WHERE foreign_lang=?",
                        (cfg["foreign"],)).fetchone()[0]})
            if url.path == "/api/verbs":
                return self._send(200, dbmod.list_verbs(
                    conn, (q.get("lang") or [cfg["foreign"]])[0]))
            if url.path == "/api/tenses":
                return self._send(200, dbmod.list_tenses(
                    conn, (q.get("lang") or [cfg["foreign"]])[0]))
            if url.path == "/api/export/corpus":
                lang = (q.get("lang") or [cfg["foreign"]])[0]
                return self._send(200, dbmod.export_corpus(conn, lang))
            if url.path == "/api/export/essay":
                return self._send(200, dbmod.export_essay(
                    conn, int((q.get("id") or ["0"])[0])))
            if url.path == "/api/essays":
                return self._send(200, dbmod.search_essays(
                    conn, cfg["foreign"], (q.get("q") or [""])[0]))
            if url.path == "/api/essay":
                r = conn.execute("SELECT * FROM essays WHERE id=?",
                                 ((q.get("id") or ["0"])[0],)).fetchone()
                if not r:
                    return self._send(404, {"error": "unknown essay"})
                return self._send(200, dict(r))
            if url.path == "/api/jobs":
                with _LOCK:
                    return self._send(200, JOBS)
            return self._send(404, {"error": "no such route"})
        except Exception as exc:
            traceback.print_exc()
            return self._send(500, {"error": f"{type(exc).__name__}: {exc}"})

    def do_POST(self):
        url = urllib.parse.urlparse(self.path)
        body = self._body()
        try:
            conn = dbmod.connect()
            cfg = dbmod.load_config()
            if url.path == "/api/settings":
                merged = dbmod.save_config({**cfg, **body})
                return self._send(200, merged)
            if url.path == "/api/verbs":
                act = body.get("action")
                if act == "generate":
                    if not _job_start("vocab"):
                        return self._send(409, {"error": "a generation is already running"})
                    count = int(body.get("count") or cfg.get("verb_count", 100))
                    threading.Thread(target=_vocab_job,
                                     args=("verbs", count, body.get("lang")),
                                     daemon=True).start()
                    return self._send(202, {"started": True, "count": count})
                if act == "update":
                    dbmod.update_verb(conn, int(body.get("id") or 0),
                                      body.get("infinitive") or "",
                                      body.get("gloss") or "")
                    return self._send(200, {"ok": True})
                if act == "add":
                    n = dbmod.add_verbs(conn, cfg["foreign"],
                                        [{"infinitive": body.get("infinitive"),
                                          "gloss": body.get("gloss", "")}])
                    return self._send(200, {"added": n})
                if act == "toggle":
                    conn.execute("UPDATE verbs SET enabled=1-enabled WHERE id=?",
                                 (int(body.get("id") or 0),))
                    conn.commit()
                    return self._send(200, {"ok": True})
                if act == "delete":
                    r = conn.execute("SELECT lang, infinitive FROM verbs WHERE id=?",
                                     (int(body.get("id") or 0),)).fetchone()
                    if r:
                        n = dbmod.verb_references(conn, r["lang"], r["infinitive"])
                        if n:
                            return self._send(409, {"error":
                                f"'{r['infinitive']}' is used by {n} stored "
                                "story(ies) — deleting it would break their "
                                "metadata. Keep it, or delete those stories first."})
                        conn.execute("DELETE FROM verbs WHERE id=?",
                                     (int(body.get("id") or 0),))
                        conn.commit()
                    return self._send(200, {"ok": True})
                return self._send(400, {"error": "unknown action"})
            if url.path == "/api/tenses":
                act = body.get("action")
                if act == "generate":
                    if not _job_start("vocab"):
                        return self._send(409, {"error": "a generation is already running"})
                    threading.Thread(target=_vocab_job,
                                     args=("tenses", 0, body.get("lang")),
                                     daemon=True).start()
                    return self._send(202, {"started": True})
                if act == "toggle":
                    conn.execute("UPDATE tenses SET enabled=1-enabled WHERE id=?",
                                 (int(body.get("id") or 0),))
                    conn.commit()
                    return self._send(200, {"ok": True})
                if act == "delete":
                    r = conn.execute("SELECT lang, name FROM tenses WHERE id=?",
                                     (int(body.get("id") or 0),)).fetchone()
                    if r:
                        n = dbmod.tense_references(conn, r["lang"], r["name"])
                        if n:
                            return self._send(409, {"error":
                                f"'{r['name']}' is used by {n} stored "
                                "story(ies) — keep it, or delete those first."})
                        conn.execute("DELETE FROM tenses WHERE id=?",
                                     (int(body.get("id") or 0),))
                        conn.commit()
                    return self._send(200, {"ok": True})
                return self._send(400, {"error": "unknown action"})
            if url.path == "/api/essay":
                topic = (body.get("topic") or "").strip()
                if not topic:
                    return self._send(400, {"error": "give a topic"})
                if not _job_start("essay"):
                    return self._send(409, {"error": "a story is already being written"})
                threading.Thread(target=_essay_job,
                                 args=(topic, body.get("verbs") or [],
                                       body.get("tenses") or []),
                                 daemon=True).start()
                return self._send(202, {"started": True})
            if url.path == "/api/translate":
                return self._send(200, translate(
                    conn, cfg, (body.get("to") or cfg["native"])[:8],
                    essay_id=body.get("essay_id"),
                    text=body.get("text"),
                    force=bool(body.get("force"))))
            if url.path == "/api/conjugation":
                lemma = (body.get("lemma") or "").strip()
                tense = (body.get("tense") or "").strip()
                if not lemma or not tense:
                    return self._send(400, {"error": "lemma + tense required"})
                return self._send(200, conjugation(conn, cfg, lemma, tense,
                                                   force=bool(body.get("force"))))
            if url.path == "/api/import/corpus":
                return self._send(200, dbmod.import_corpus(conn, body))
            if url.path == "/api/import/essay":
                return self._send(200, dbmod.import_essay(conn, body))
            if url.path == "/api/card":
                word = (body.get("word") or "").strip()
                if not word:
                    return self._send(400, {"error": "word required"})
                return self._send(200, word_card(conn, cfg, word,
                                                 body.get("sentence") or "",
                                                 force=bool(body.get("force"))))
            return self._send(404, {"error": "no such route"})
        except llm.LlmError as exc:
            return self._send(502, {"error": str(exc)})
        except Exception as exc:
            traceback.print_exc()
            return self._send(500, {"error": f"{type(exc).__name__}: {exc}"})


def main() -> int:
    ap = argparse.ArgumentParser(description="aisVerbo web app (local only).")
    ap.add_argument("--port", type=int,
                    default=int(os.environ.get("PORT", 8787)))
    ap.add_argument("--host", default=os.environ.get("HOST", "127.0.0.1"))
    args = ap.parse_args()
    srv = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"aisVerbo → http://{args.host}:{args.port}/")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

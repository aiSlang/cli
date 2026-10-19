"""aisVerbo — the local store: settings, vocabulary, tenses, essays, cards.

SQLite + JSON config under app/generated/ (git-ignored). Standard library
only, same discipline as the aisGTM example.
"""
from __future__ import annotations

import json
import os
import sqlite3
from typing import Mapping

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)                       # examples/aisverbo
# AISVERBO_DATA relocates all state (tests, HF Spaces persistent storage,
# running a second instance next to a live one)
GEN_DIR = os.environ.get("AISVERBO_DATA") or os.path.join(HERE, "generated")
DEFAULT_DB = os.path.join(GEN_DIR, "aisverbo.db")
CONFIG_PATH = os.path.join(GEN_DIR, "config.json")

# the built-in language list; "foreign" may equal "native" (formal grammar
# of one's own language is a valid course). The user can add any further
# language in Setup — stored in the config's languages_extra map.
LANGUAGES = {"en": "English", "es": "Español", "pt": "Português",
             "fr": "Français", "it": "Italiano", "de": "Deutsch"}


def all_languages(cfg) -> dict:
    out = dict(LANGUAGES)
    extra = cfg.get("languages_extra") or {}
    out.update({str(k)[:8].lower(): str(v)[:40] for k, v in extra.items() if k})
    return out

DEFAULT_CONFIG = {
    "foreign": "es",                  # the language being learned
    "native": "en",                   # the learner's language
    "verb_count": 100,                # size of the generated vocabulary
    "essay_words": 180,               # target story length
    "languages_extra": {},            # user-added languages: {code: name}
    "aislang_bin": "aislang",
    "project_ais": os.path.join(ROOT, "aisverbo.ais"),
    "chat_timeout": 240,
}

SCHEMA = """
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS verbs(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  lang TEXT NOT NULL,
  infinitive TEXT NOT NULL,
  gloss TEXT,
  enabled INTEGER DEFAULT 1,
  UNIQUE(lang, infinitive)
);
CREATE TABLE IF NOT EXISTS tenses(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  lang TEXT NOT NULL,
  name TEXT NOT NULL,
  native_name TEXT,
  hint TEXT,
  example TEXT,
  enabled INTEGER DEFAULT 1,
  UNIQUE(lang, name)
);
CREATE TABLE IF NOT EXISTS essays(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  created_at TEXT,
  foreign_lang TEXT,
  native_lang TEXT,
  topic TEXT,
  title TEXT,
  body TEXT,
  verbs_csv TEXT,
  tenses_csv TEXT
);
CREATE TABLE IF NOT EXISTS translations(
  essay_id INTEGER NOT NULL,
  lang TEXT NOT NULL,
  pairs_json TEXT,
  created_at TEXT,
  PRIMARY KEY(essay_id, lang)
);
CREATE TABLE IF NOT EXISTS conjugations(
  lemma TEXT NOT NULL,
  tense TEXT NOT NULL,
  foreign_lang TEXT NOT NULL,
  native_lang TEXT NOT NULL,
  json TEXT,
  created_at TEXT,
  PRIMARY KEY(lemma, tense, foreign_lang, native_lang)
);
CREATE TABLE IF NOT EXISTS cards(
  word TEXT NOT NULL,
  foreign_lang TEXT NOT NULL,
  native_lang TEXT NOT NULL,
  json TEXT,
  created_at TEXT,
  PRIMARY KEY(word, foreign_lang, native_lang)
);
"""


# bump when the schema changes; _migrate upgrades EXISTING databases in
# place with ALTER/backfill — data is never dropped on upgrade
SCHEMA_VERSION = 2


def _migrate(conn: sqlite3.Connection) -> None:
    if conn.execute("PRAGMA user_version").fetchone()[0] >= SCHEMA_VERSION:
        return
    # v2: the conjugations table (created by SCHEMA's IF NOT EXISTS) — and
    # the pattern itself: future versions add ALTERs here, never DROPs
    conn.execute(f"PRAGMA user_version = {SCHEMA_VERSION}")
    conn.commit()


def connect(path: str = DEFAULT_DB) -> sqlite3.Connection:
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    conn = sqlite3.connect(path)
    conn.row_factory = sqlite3.Row
    conn.executescript(SCHEMA)
    _migrate(conn)
    return conn


# hosted limits — the app is free of charge, so one vocabulary generation
# is capped at 100 verbs and one story at 200 words
MAX_VERB_COUNT = 100
MAX_ESSAY_WORDS = 200

_TID_OK = __import__("re").compile(r"^[A-Za-z0-9_-]{1,64}$")


def tenant_paths(tid: str | None) -> tuple[str, str]:
    """(db_path, config_path) of one tenant's isolated data directory.

    The shared PUBLIC profile (every visitor without an identity, and every
    local run) lives at the legacy single-user paths — so a laptop install's
    corpus and stories ARE the public seed when the same data dir ships to
    a hosted Space."""
    if not tid or tid in ("public", "local"):
        return DEFAULT_DB, CONFIG_PATH
    if not _TID_OK.match(tid):
        raise ValueError("bad tenant id")
    tdir = os.path.join(GEN_DIR, "tenants", tid)
    return os.path.join(tdir, "aisverbo.db"), os.path.join(tdir, "config.json")


def _clamp(cfg: dict) -> None:
    try:
        cfg["verb_count"] = max(1, min(int(cfg.get("verb_count", 100)), MAX_VERB_COUNT))
    except (TypeError, ValueError):
        cfg["verb_count"] = 100
    try:
        cfg["essay_words"] = max(30, min(int(cfg.get("essay_words", 180)), MAX_ESSAY_WORDS))
    except (TypeError, ValueError):
        cfg["essay_words"] = 180


def load_config(path: str = CONFIG_PATH) -> dict:
    cfg = dict(DEFAULT_CONFIG)
    try:
        with open(path, encoding="utf-8") as fh:
            cfg.update(json.load(fh))
    except (OSError, json.JSONDecodeError):
        pass
    langs = all_languages(cfg)
    if cfg.get("foreign") not in langs:
        cfg["foreign"] = "es"
    if cfg.get("native") not in langs:
        cfg["native"] = "en"
    _clamp(cfg)
    return cfg


def save_config(cfg: Mapping, path: str = CONFIG_PATH) -> dict:
    merged = dict(DEFAULT_CONFIG)
    merged.update({k: v for k, v in cfg.items() if k in DEFAULT_CONFIG})
    _clamp(merged)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(merged, fh, indent=2, ensure_ascii=False)
    return merged


# ─── vocabulary ──────────────────────────────────────────────────────
def add_verbs(conn, lang: str, items: list[dict]) -> int:
    n = 0
    for it in items:
        inf = (it.get("infinitive") or "").strip().lower()
        if not inf:
            continue
        cur = conn.execute(
            "INSERT OR IGNORE INTO verbs(lang, infinitive, gloss) VALUES(?,?,?)",
            (lang, inf, (it.get("gloss") or "").strip()))
        n += cur.rowcount
    conn.commit()
    return n


def list_verbs(conn, lang: str) -> list[dict]:
    return [dict(r) for r in conn.execute(
        "SELECT * FROM verbs WHERE lang=? ORDER BY id", (lang,))]


def add_tenses(conn, lang: str, items: list[dict]) -> int:
    n = 0
    for it in items:
        name = (it.get("name") or "").strip()
        if not name:
            continue
        cur = conn.execute(
            "INSERT OR IGNORE INTO tenses(lang, name, native_name, hint, example) "
            "VALUES(?,?,?,?,?)",
            (lang, name, it.get("native_name"), it.get("hint"), it.get("example")))
        n += cur.rowcount
    conn.commit()
    return n


def list_tenses(conn, lang: str) -> list[dict]:
    return [dict(r) for r in conn.execute(
        "SELECT * FROM tenses WHERE lang=? ORDER BY id", (lang,))]


# ─── essays ──────────────────────────────────────────────────────────
def save_essay(conn, rec: Mapping) -> int:
    cur = conn.execute(
        "INSERT INTO essays(created_at, foreign_lang, native_lang, topic, title, "
        "body, verbs_csv, tenses_csv) VALUES(:created_at,:foreign_lang,"
        ":native_lang,:topic,:title,:body,:verbs_csv,:tenses_csv)", rec)
    conn.commit()
    return cur.lastrowid


def search_essays(conn, lang: str, q: str = "") -> list[dict]:
    """Newest first; `q` matches the title, body, topic or the verb list —
    so any verb or keyword finds every story it appears in."""
    sql = ("SELECT id, created_at, foreign_lang, native_lang, topic, title, "
           "substr(body, 1, 160) excerpt FROM essays WHERE foreign_lang=?")
    args: list = [lang]
    if q.strip():
        sql += (" AND (title LIKE ? OR body LIKE ? OR topic LIKE ? "
                "OR verbs_csv LIKE ?)")
        like = f"%{q.strip()}%"
        args += [like, like, like, like]
    sql += " ORDER BY id DESC LIMIT 200"
    return [dict(r) for r in conn.execute(sql, args)]


# ─── word cards (grammarian cache) ───────────────────────────────────
def get_card(conn, word: str, foreign: str, native: str) -> dict | None:
    r = conn.execute("SELECT json FROM cards WHERE word=? AND foreign_lang=? "
                     "AND native_lang=?", (word.lower(), foreign, native)).fetchone()
    return json.loads(r["json"]) if r else None


def save_card(conn, word: str, foreign: str, native: str, card: dict, now: str) -> None:
    conn.execute("INSERT OR REPLACE INTO cards(word, foreign_lang, native_lang, "
                 "json, created_at) VALUES(?,?,?,?,?)",
                 (word.lower(), foreign, native,
                  json.dumps(card, ensure_ascii=False), now))
    conn.commit()


# ─── translations (full-essay, sentence-aligned, cached) ─────────────
def get_translation(conn, essay_id: int, lang: str):
    r = conn.execute("SELECT pairs_json FROM translations WHERE essay_id=? "
                     "AND lang=?", (essay_id, lang)).fetchone()
    if not r:
        return None
    data = json.loads(r["pairs_json"])
    # pre-v3 rows stored sentence pairs; the new shape is {"title","body"} —
    # returning None makes the next request re-translate and overwrite
    return data if isinstance(data, dict) else None


def save_translation(conn, essay_id: int, lang: str, pairs, now: str) -> None:
    conn.execute("INSERT OR REPLACE INTO translations(essay_id, lang, pairs_json, "
                 "created_at) VALUES(?,?,?,?)",
                 (essay_id, lang, json.dumps(pairs, ensure_ascii=False), now))
    conn.commit()


def update_verb(conn, verb_id: int, infinitive: str, gloss: str) -> None:
    conn.execute("UPDATE verbs SET infinitive=?, gloss=? WHERE id=?",
                 (infinitive.strip().lower(), gloss.strip(), verb_id))
    conn.commit()


def verb_references(conn, lang: str, infinitive: str) -> int:
    """How many stored essays used this verb — a delete guard."""
    return conn.execute(
        "SELECT COUNT(*) FROM essays WHERE foreign_lang=? "
        "AND ','||verbs_csv||',' LIKE ?",
        (lang, f"%,{infinitive},%")).fetchone()[0]


def tense_references(conn, lang: str, name: str) -> int:
    return conn.execute(
        "SELECT COUNT(*) FROM essays WHERE foreign_lang=? "
        "AND ','||tenses_csv||',' LIKE ?",
        (lang, f"%,{name},%")).fetchone()[0]


# ─── conjugation cache (shared across every story and word card) ─────
def get_conjugation(conn, lemma: str, tense: str, foreign: str, native: str):
    r = conn.execute("SELECT json FROM conjugations WHERE lemma=? AND tense=? "
                     "AND foreign_lang=? AND native_lang=?",
                     (lemma.lower(), tense, foreign, native)).fetchone()
    return json.loads(r["json"]) if r else None


def save_conjugation(conn, lemma: str, tense: str, foreign: str, native: str,
                     data: dict, now: str) -> None:
    conn.execute("INSERT OR REPLACE INTO conjugations(lemma, tense, foreign_lang, "
                 "native_lang, json, created_at) VALUES(?,?,?,?,?,?)",
                 (lemma.lower(), tense, foreign, native,
                  json.dumps(data, ensure_ascii=False), now))
    conn.commit()


# ─── import / export (idempotent on load) ────────────────────────────
def export_corpus(conn, lang: str) -> dict:
    return {"kind": "aisverbo-corpus", "lang": lang,
            "verbs": [{"infinitive": v["infinitive"], "gloss": v["gloss"] or ""}
                      for v in list_verbs(conn, lang)],
            "tenses": [{k: t[k] or "" for k in
                        ("name", "native_name", "hint", "example")}
                       for t in list_tenses(conn, lang)]}


def import_corpus(conn, data: dict) -> dict:
    lang = (data.get("lang") or "").strip()
    if not lang:
        raise ValueError("corpus file has no lang")
    nv = add_verbs(conn, lang, data.get("verbs") or [])
    nt = add_tenses(conn, lang, data.get("tenses") or [])
    return {"lang": lang, "verbs_added": nv, "tenses_added": nt}


def export_essay(conn, essay_id: int) -> dict:
    r = conn.execute("SELECT * FROM essays WHERE id=?", (essay_id,)).fetchone()
    if not r:
        raise ValueError("unknown essay")
    translations = {}
    for t in conn.execute("SELECT lang, pairs_json FROM translations "
                          "WHERE essay_id=?", (essay_id,)):
        data = json.loads(t["pairs_json"])
        if isinstance(data, dict):            # pre-v3 pair lists are not exported
            translations[t["lang"]] = {
                "title": (data.get("title") or "").replace("**", ""),
                "body": (data.get("body") or "").replace("**", "")}
    # the story's words anchor its word cards and conjugations — exporting
    # them moves the whole learned context between installs without paying
    # the LLM again
    import re as _re
    words = {w.lower() for w in _re.findall(r"[\w'’-]+", f"{r['title']} {r['body']}".replace("**", ""), _re.UNICODE)}
    cards, lemmas = [], set()
    for c in conn.execute("SELECT word, foreign_lang, native_lang, json "
                          "FROM cards WHERE foreign_lang=?", (r["foreign_lang"],)):
        if c["word"] in words:
            data = json.loads(c["json"])
            cards.append({"word": c["word"], "native_lang": c["native_lang"],
                          "card": data})
            if data.get("lemma"):
                lemmas.add(str(data["lemma"]).lower())
    conjugations = [
        {"lemma": cj["lemma"], "tense": cj["tense"],
         "native_lang": cj["native_lang"], "data": json.loads(cj["json"])}
        for cj in conn.execute("SELECT lemma, tense, native_lang, json "
                               "FROM conjugations WHERE foreign_lang=?",
                               (r["foreign_lang"],))
        if cj["lemma"] in lemmas or cj["lemma"] in words]
    return {"kind": "aisverbo-essay",
            **{k: r[k] for k in ("created_at", "foreign_lang", "native_lang",
                                 "topic", "title", "body", "verbs_csv",
                                 "tenses_csv")},
            "translations": translations,
            "cards": cards,
            "conjugations": conjugations}


def import_essay(conn, data: dict) -> dict:
    """Idempotent: the (foreign_lang, title, body) triple is the identity —
    re-importing the same file never duplicates the story."""
    body = (data.get("body") or "").strip()
    if not body:
        raise ValueError("essay file has no body")
    row = conn.execute("SELECT id FROM essays WHERE foreign_lang=? AND title=? "
                       "AND body=?", (data.get("foreign_lang"),
                                      data.get("title"), body)).fetchone()
    if row:
        eid, created = row["id"], False
    else:
        eid = save_essay(conn, {k: data.get(k) for k in
                                ("created_at", "foreign_lang", "native_lang",
                                 "topic", "title", "body", "verbs_csv",
                                 "tenses_csv")})
        created = True
    for lang, tr in (data.get("translations") or {}).items():
        if isinstance(tr, dict) and "body" in tr \
                and get_translation(conn, eid, lang) is None:
            save_translation(conn, eid, lang, tr,
                             data.get("created_at") or "")
    # cards and conjugations: fill the shared caches, never overwrite
    foreign = data.get("foreign_lang") or ""
    cards_added = conj_added = 0
    for c in data.get("cards") or []:
        if not (isinstance(c, dict) and c.get("word") and isinstance(c.get("card"), dict)):
            continue
        cur = conn.execute(
            "INSERT OR IGNORE INTO cards(word, foreign_lang, native_lang, json, "
            "created_at) VALUES(?,?,?,?,?)",
            (str(c["word"]).lower(), foreign, c.get("native_lang") or "en",
             json.dumps(c["card"], ensure_ascii=False),
             data.get("created_at") or ""))
        cards_added += cur.rowcount
    for cj in data.get("conjugations") or []:
        if not (isinstance(cj, dict) and cj.get("lemma") and cj.get("tense")):
            continue
        cur = conn.execute(
            "INSERT OR IGNORE INTO conjugations(lemma, tense, foreign_lang, "
            "native_lang, json, created_at) VALUES(?,?,?,?,?,?)",
            (str(cj["lemma"]).lower(), cj["tense"], foreign,
             cj.get("native_lang") or "en",
             json.dumps(cj.get("data") or {}, ensure_ascii=False),
             data.get("created_at") or ""))
        conj_added += cur.rowcount
    conn.commit()
    return {"id": eid, "created": created,
            "cards_added": cards_added, "conjugations_added": conj_added}

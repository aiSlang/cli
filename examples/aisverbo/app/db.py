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
GEN_DIR = os.path.join(HERE, "generated")
DEFAULT_DB = os.path.join(GEN_DIR, "aisverbo.db")
CONFIG_PATH = os.path.join(GEN_DIR, "config.json")

# the language pairs the UI offers; "foreign" may equal "native" (formal
# grammar of one's own language is a valid course)
LANGUAGES = {"en": "English", "es": "Español", "pt": "Português",
             "fr": "Français", "it": "Italiano", "de": "Deutsch"}

DEFAULT_CONFIG = {
    "foreign": "es",                  # the language being learned
    "native": "en",                   # the learner's language
    "verb_count": 100,                # size of the generated vocabulary
    "essay_words": 180,               # target story length
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
CREATE TABLE IF NOT EXISTS cards(
  word TEXT NOT NULL,
  foreign_lang TEXT NOT NULL,
  native_lang TEXT NOT NULL,
  json TEXT,
  created_at TEXT,
  PRIMARY KEY(word, foreign_lang, native_lang)
);
"""


def connect(path: str = DEFAULT_DB) -> sqlite3.Connection:
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    conn = sqlite3.connect(path)
    conn.row_factory = sqlite3.Row
    conn.executescript(SCHEMA)
    return conn


def load_config(path: str = CONFIG_PATH) -> dict:
    cfg = dict(DEFAULT_CONFIG)
    try:
        with open(path, encoding="utf-8") as fh:
            cfg.update(json.load(fh))
    except (OSError, json.JSONDecodeError):
        pass
    if cfg.get("foreign") not in LANGUAGES:
        cfg["foreign"] = "es"
    if cfg.get("native") not in LANGUAGES:
        cfg["native"] = "en"
    return cfg


def save_config(cfg: Mapping, path: str = CONFIG_PATH) -> dict:
    merged = dict(DEFAULT_CONFIG)
    merged.update({k: v for k, v in cfg.items() if k in DEFAULT_CONFIG})
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

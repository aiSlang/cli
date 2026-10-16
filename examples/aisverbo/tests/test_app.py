"""Offline tests for the aisVerbo app — no network, no aislang binary:
the LLM bridge is monkeypatched. Run:  python3 -m pytest tests -q"""
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__))), "app"))
import db as dbmod      # noqa: E402
import serve            # noqa: E402


def cfg_for(tmp_path):
    return dict(dbmod.DEFAULT_CONFIG, foreign="es", native="en")


def test_config_roundtrip_and_language_guard(tmp_path):
    path = str(tmp_path / "config.json")
    saved = dbmod.save_config({"foreign": "fr", "native": "de",
                               "verb_count": 50}, path)
    assert saved["foreign"] == "fr" and saved["verb_count"] == 50
    cfg = dbmod.load_config(path)
    assert cfg["native"] == "de"
    with open(path, "w") as fh:                 # a corrupt language falls back
        json.dump({"foreign": "xx"}, fh)
    assert dbmod.load_config(path)["foreign"] == "es"


def test_vocabulary_generation_batches_and_excludes(tmp_path, monkeypatch):
    conn = dbmod.connect(str(tmp_path / "v.db"))
    cfg = cfg_for(tmp_path)
    calls = []
    def fake_array(agent, payload, c):
        calls.append(payload)
        start = len(payload["exclude"])
        return [{"infinitive": f"verbo{start + i}", "gloss": f"g{start + i}"}
                for i in range(payload["count"])]
    monkeypatch.setattr(serve.llm, "ask_json_array", fake_array)
    rep = serve.generate_verbs(conn, cfg, 150)
    assert rep["added"] == 150 and rep["total"] == 150
    assert [c["count"] for c in calls] == [100, 50]      # batched
    assert calls[1]["exclude"][-1] == "verbo99"          # dedupe context passed
    # idempotent re-run with a saturated model stops cleanly
    monkeypatch.setattr(serve.llm, "ask_json_array",
                        lambda a, p, c: [{"infinitive": "verbo0", "gloss": "dup"}])
    rep2 = serve.generate_verbs(conn, cfg, 50)
    assert rep2["added"] == 0 and rep2["total"] == 150


def test_essay_uses_selection_and_stores(tmp_path, monkeypatch):
    conn = dbmod.connect(str(tmp_path / "e.db"))
    cfg = cfg_for(tmp_path)
    dbmod.add_verbs(conn, "es", [{"infinitive": "ir", "gloss": "to go"},
                                 {"infinitive": "comer", "gloss": "to eat"}])
    dbmod.add_tenses(conn, "es", [{"name": "Presente"},
                                  {"name": "Futuro Simple"}])
    seen = {}
    def fake_json(agent, payload, c):
        seen.update(payload)
        return {"title": "Un viaje", "body": "Hoy **voy** al mar y **comeré** pescado."}
    monkeypatch.setattr(serve.llm, "ask_json", fake_json)
    verbs = dbmod.list_verbs(conn, "es")
    tenses = dbmod.list_tenses(conn, "es")
    rep = serve.generate_essay(conn, cfg, "el mar",
                               [verbs[0]["id"]], [tenses[1]["id"]])
    assert seen["verbs"] == ["ir"] and seen["tenses"] == ["Futuro Simple"]
    assert rep["id"] == 1
    # the selection is REQUIRED now — an empty one is a clear error
    try:
        serve.generate_essay(conn, cfg, "otra vez", [], [])
        assert False, "expected a selection error"
    except RuntimeError as e:
        assert "verb" in str(e)
    # the library finds it by verb and by topic word
    assert len(dbmod.search_essays(conn, "es", "comeré")) >= 1
    assert len(dbmod.search_essays(conn, "es", "viaje")) >= 1
    assert dbmod.search_essays(conn, "es", "zzz-nope") == []


def test_word_card_is_cached(tmp_path, monkeypatch):
    conn = dbmod.connect(str(tmp_path / "c.db"))
    cfg = cfg_for(tmp_path)
    calls = []
    def fake_json(agent, payload, c):
        calls.append(agent)
        return {"word": payload["word"], "lemma": "ir", "pos": "verb",
                "analysis": "1sg presente", "ipa": "boi"}
    monkeypatch.setattr(serve.llm, "ask_json", fake_json)
    c1 = serve.word_card(conn, cfg, "voy", "Hoy voy al mar.")
    c2 = serve.word_card(conn, cfg, "Voy", "Voy otra vez.")   # case-insensitive hit
    assert c1["cached"] is False and c2["cached"] is True
    assert calls == ["grammarian"]                            # exactly one model call
    # another language pair is its own cache slot
    cfg2 = dict(cfg, native="de")
    serve.word_card(conn, cfg2, "voy", "Hoy voy.")
    assert calls == ["grammarian", "grammarian"]


def test_verb_management(tmp_path):
    conn = dbmod.connect(str(tmp_path / "m.db"))
    dbmod.add_verbs(conn, "es", [{"infinitive": " Ser ", "gloss": "to be"},
                                 {"infinitive": "ser", "gloss": "dup"},
                                 {"infinitive": "", "gloss": "skipped"}])
    rows = dbmod.list_verbs(conn, "es")
    assert [r["infinitive"] for r in rows] == ["ser"]         # trimmed, deduped
    assert dbmod.list_verbs(conn, "fr") == []                 # per-language


def test_translation_cached_per_essay_and_language(tmp_path, monkeypatch):
    conn = dbmod.connect(str(tmp_path / "t.db"))
    cfg = cfg_for(tmp_path)
    eid = dbmod.save_essay(conn, {
        "created_at": "now", "foreign_lang": "es", "native_lang": "en",
        "topic": "mar", "title": "Un viaje",
        "body": "Hoy **voy** al mar. Mañana **comeré** pescado.",
        "verbs_csv": "ir,comer", "tenses_csv": "Presente"})
    calls = []
    def fake_json(agent, payload, c):
        calls.append(payload["to"])
        return {"pairs": [["Un viaje", "A trip"],
                          ["Hoy **voy** al mar.", "Today I go to the sea."]]}
    monkeypatch.setattr(serve.llm, "ask_json", fake_json)
    r1 = serve.translate(conn, cfg, "en", essay_id=eid)
    r2 = serve.translate(conn, cfg, "en", essay_id=eid)      # cache hit
    assert r1["cached"] is False and r2["cached"] is True
    assert calls == ["en"]
    serve.translate(conn, cfg, "de", essay_id=eid)           # its own slot
    assert calls == ["en", "de"]
    r4 = serve.translate(conn, cfg, "en", essay_id=eid, force=True)  # explicit refresh
    assert r4["cached"] is False and calls == ["en", "de", "en"]
    # a fragment is ad hoc: translated but never stored
    serve.translate(conn, cfg, "en", text="Hoy voy al mar.")
    assert calls[-1] == "en"
    assert conn.execute("SELECT COUNT(*) FROM translations").fetchone()[0] == 2


def test_word_card_force_refresh(tmp_path, monkeypatch):
    conn = dbmod.connect(str(tmp_path / "f.db"))
    cfg = cfg_for(tmp_path)
    calls = []
    monkeypatch.setattr(serve.llm, "ask_json",
                        lambda a, p, c: calls.append(1) or {"word": p["word"], "lemma": "ir"})
    serve.word_card(conn, cfg, "voy", "x")
    serve.word_card(conn, cfg, "voy", "x")
    assert len(calls) == 1
    r = serve.word_card(conn, cfg, "voy", "x", force=True)   # the UI's ⟳
    assert len(calls) == 2 and r["cached"] is False


def test_user_added_languages(tmp_path):
    path = str(tmp_path / "config.json")
    dbmod.save_config({"languages_extra": {"uk": "Українська"},
                       "foreign": "uk"}, path)
    cfg = dbmod.load_config(path)
    assert cfg["foreign"] == "uk"                            # custom code accepted
    assert dbmod.all_languages(cfg)["uk"] == "Українська"
    assert "es" in dbmod.all_languages(cfg)                  # built-ins kept

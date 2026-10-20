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
        assert "**" not in payload["text"]                   # markers never sent
        return {"title": "A trip", "body": "Today I **go** to the sea."}
    monkeypatch.setattr(serve.llm, "ask_json", fake_json)
    r1 = serve.translate(conn, cfg, "en", essay_id=eid)
    r2 = serve.translate(conn, cfg, "en", essay_id=eid)      # cache hit
    assert r1["cached"] is False and r2["cached"] is True
    assert r1["body"] == "Today I go to the sea."            # markers never stored
    assert r1["title"] == "A trip"
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


def test_corpus_is_language_scoped_and_generation_respects_it(tmp_path, monkeypatch):
    conn = dbmod.connect(str(tmp_path / "l.db"))
    cfg = cfg_for(tmp_path)                               # course: es
    seen = []
    monkeypatch.setattr(serve.llm, "ask_json_array",
                        lambda a, p, c: seen.append(p["lang"]) or
                        [{"infinitive": f"v-{p['lang']}", "gloss": "g"}])
    serve.generate_verbs(conn, cfg, 1)                    # default = course
    serve.generate_verbs(conn, cfg, 1, lang="fr")         # Vocabulary selector
    assert seen == ["es", "fr"]
    assert [v["infinitive"] for v in dbmod.list_verbs(conn, "fr")] == ["v-fr"]
    assert [v["infinitive"] for v in dbmod.list_verbs(conn, "es")] == ["v-es"]


def test_verb_update_and_guarded_delete(tmp_path):
    conn = dbmod.connect(str(tmp_path / "g.db"))
    dbmod.add_verbs(conn, "es", [{"infinitive": "comerr", "gloss": ""}])
    v = dbmod.list_verbs(conn, "es")[0]
    dbmod.update_verb(conn, v["id"], "comer", "to eat")   # fix the typo, add gloss
    v2 = dbmod.list_verbs(conn, "es")[0]
    assert (v2["infinitive"], v2["gloss"]) == ("comer", "to eat")
    assert dbmod.verb_references(conn, "es", "comer") == 0
    dbmod.save_essay(conn, {"created_at": "x", "foreign_lang": "es",
                            "native_lang": "en", "topic": "t", "title": "T",
                            "body": "b", "verbs_csv": "ir,comer",
                            "tenses_csv": "Presente"})
    assert dbmod.verb_references(conn, "es", "comer") == 1   # delete must refuse
    assert dbmod.tense_references(conn, "es", "Presente") == 1


def test_conjugation_cached_shared(tmp_path, monkeypatch):
    conn = dbmod.connect(str(tmp_path / "cj.db"))
    cfg = cfg_for(tmp_path)
    calls = []
    monkeypatch.setattr(serve.llm, "ask_json", lambda a, p, c: calls.append(1) or
                        {"conjugation": [["yo", "iré"]], "examples": ["Iré."]})
    r1 = serve.conjugation(conn, cfg, "ir", "Futuro Simple")
    r2 = serve.conjugation(conn, cfg, "IR", "Futuro Simple")   # lemma-case shared
    assert r1["cached"] is False and r2["cached"] is True and len(calls) == 1
    r3 = serve.conjugation(conn, cfg, "ir", "Futuro Simple", force=True)
    assert r3["cached"] is False and len(calls) == 2


def test_corpus_export_import_idempotent(tmp_path):
    conn = dbmod.connect(str(tmp_path / "io.db"))
    dbmod.add_verbs(conn, "es", [{"infinitive": "ir", "gloss": "to go"}])
    dbmod.add_tenses(conn, "es", [{"name": "Presente", "hint": "now"}])
    blob = dbmod.export_corpus(conn, "es")
    assert blob["kind"] == "aisverbo-corpus"
    conn2 = dbmod.connect(str(tmp_path / "io2.db"))
    r1 = dbmod.import_corpus(conn2, blob)
    r2 = dbmod.import_corpus(conn2, blob)                     # idempotent
    assert r1["verbs_added"] == 1 and r2["verbs_added"] == 0
    assert r2["tenses_added"] == 0
    assert len(dbmod.list_verbs(conn2, "es")) == 1


def test_essay_export_import_idempotent(tmp_path):
    conn = dbmod.connect(str(tmp_path / "eio.db"))
    eid = dbmod.save_essay(conn, {"created_at": "x", "foreign_lang": "es",
                                  "native_lang": "en", "topic": "mar",
                                  "title": "Viaje", "body": "Hoy **voy**.",
                                  "verbs_csv": "ir", "tenses_csv": "Presente"})
    dbmod.save_translation(conn, eid, "en", {"title": "Trip", "body": "Today I go."}, "x")
    blob = dbmod.export_essay(conn, eid)
    assert blob["translations"]["en"] == {"title": "Trip", "body": "Today I go."}
    conn2 = dbmod.connect(str(tmp_path / "eio2.db"))
    r1 = dbmod.import_essay(conn2, blob)
    r2 = dbmod.import_essay(conn2, blob)                      # idempotent
    assert r1["created"] is True and r2["created"] is False and r1["id"] == r2["id"]
    assert dbmod.get_translation(conn2, r1["id"], "en")["body"] == "Today I go."


def test_migration_preserves_data(tmp_path):
    path = str(tmp_path / "old.db")
    conn = dbmod.connect(path)
    dbmod.add_verbs(conn, "es", [{"infinitive": "ser", "gloss": "to be"}])
    conn.close()
    conn2 = dbmod.connect(path)                               # re-open = migrate
    assert [v["infinitive"] for v in dbmod.list_verbs(conn2, "es")] == ["ser"]
    assert conn2.execute("PRAGMA user_version").fetchone()[0] == dbmod.SCHEMA_VERSION


def test_legacy_pair_translations_retranslate(tmp_path):
    conn = dbmod.connect(str(tmp_path / "lg.db"))
    eid = dbmod.save_essay(conn, {"created_at": "x", "foreign_lang": "es",
                                  "native_lang": "en", "topic": "t", "title": "T",
                                  "body": "b", "verbs_csv": "", "tenses_csv": ""})
    conn.execute("INSERT INTO translations(essay_id, lang, pairs_json, created_at) "
                 "VALUES(?,?,?,?)", (eid, "en", '[["a","b"]]', "x"))
    conn.commit()
    assert dbmod.get_translation(conn, eid, "en") is None    # forces a re-translate
    assert dbmod.export_essay(conn, eid)["translations"] == {}


def test_plain_translation_is_single_block():
    assert serve._plain("Hola **mundo**.\n\nOtro *parrafo* `x`.") == "Hola mundo. Otro parrafo x."
    assert "\\n" not in serve._plain("a \\n\\n b")


def test_export_carries_cards_and_conjugations(tmp_path):
    conn = dbmod.connect(str(tmp_path / "x.db"))
    eid = dbmod.save_essay(conn, {"created_at": "x", "foreign_lang": "es",
                                  "native_lang": "en", "topic": "mar",
                                  "title": "Viaje", "body": "Hoy **voy** al mar.",
                                  "verbs_csv": "ir", "tenses_csv": "Presente"})
    dbmod.save_card(conn, "voy", "es", "en",
                    {"word": "voy", "lemma": "ir", "tense": "Presente"}, "x")
    dbmod.save_card(conn, "otro", "es", "en", {"word": "otro"}, "x")   # not in story
    dbmod.save_conjugation(conn, "ir", "Futuro Simple", "es", "en",
                           {"conjugation": [["yo", "iré"]], "examples": ["Iré."]}, "x")
    blob = dbmod.export_essay(conn, eid)
    assert [c["word"] for c in blob["cards"]] == ["voy"]               # story words only
    assert blob["conjugations"][0]["lemma"] == "ir"                    # via the card's lemma
    # import into a fresh install: caches filled, never overwritten
    conn2 = dbmod.connect(str(tmp_path / "y.db"))
    r1 = dbmod.import_essay(conn2, blob)
    assert r1["cards_added"] == 1 and r1["conjugations_added"] == 1
    assert dbmod.get_card(conn2, "voy", "es", "en")["lemma"] == "ir"
    assert dbmod.get_conjugation(conn2, "ir", "Futuro Simple", "es", "en")["examples"] == ["Iré."]
    r2 = dbmod.import_essay(conn2, blob)                               # idempotent
    assert r2["cards_added"] == 0 and r2["conjugations_added"] == 0


# ─── FastAPI routes: tenancy + admin auth + limits ─────────────────────
import pytest


def _client(tmp_path, monkeypatch, multi=False):
    httpx = pytest.importorskip("httpx")  # noqa: F841
    from fastapi.testclient import TestClient
    monkeypatch.setenv("AISVERBO_SECRET", "test-secret")
    monkeypatch.setattr(dbmod, "GEN_DIR", str(tmp_path))
    monkeypatch.setattr(dbmod, "DEFAULT_DB", str(tmp_path / "aisverbo.db"))
    monkeypatch.setattr(dbmod, "CONFIG_PATH", str(tmp_path / "config.json"))
    monkeypatch.setattr(serve, "MULTI_TENANT", multi)
    return TestClient(serve.app)


def _admin_token(sub="public"):
    import auth
    return auth.sign({"role": "corpus_admin", "sub": sub, "jti": f"ja-{sub}"})


def _writer_token(sub="public"):
    import auth
    return auth.sign({"role": "story_writer", "sub": sub, "jti": f"jw-{sub}"})


def test_jwt_sign_verify_roundtrip(monkeypatch):
    monkeypatch.setenv("AISVERBO_SECRET", "s1")
    import auth
    tok = auth.sign({"tid": "abc"})
    assert auth.verify(tok)["tid"] == "abc"
    assert auth.verify(tok + "x") is None
    monkeypatch.setenv("AISVERBO_SECRET", "other")
    assert auth.verify(tok) is None                       # wrong secret
    import time
    monkeypatch.setenv("AISVERBO_SECRET", "s1")
    assert auth.verify(auth.sign({"exp": time.time() - 10})) is None


def test_corpus_posts_require_admin(tmp_path, monkeypatch):
    c = _client(tmp_path, monkeypatch)
    r = c.post("/api/verbs", json={"action": "add", "infinitive": "ir"})
    assert r.status_code == 401
    assert "Request corpus access (onboarding)" in r.json()["error"]
    r = c.post("/api/tenses", json={"action": "generate"})
    assert r.status_code == 401
    ok = c.post("/api/verbs", json={"action": "add", "infinitive": "ir"},
                headers={"X-Admin-Token": _admin_token()})
    assert ok.status_code == 200 and ok.json()["added"] == 1
    # a WRITER token is not enough for corpus work (role separation)
    r = c.post("/api/verbs", json={"action": "add", "infinitive": "ser"},
               headers={"X-Admin-Token": _writer_token()})
    assert r.status_code == 403 and "corpus_admin" in r.json()["error"]
    # reads stay open
    assert c.get("/api/verbs").status_code == 200


def test_public_fallback_and_user_isolation(tmp_path, monkeypatch):
    c = _client(tmp_path, monkeypatch, multi=True)
    import auth
    # no token -> the shared PUBLIC profile (one dataset for every browser)
    s = c.get("/api/state").json()
    assert s["tenant"] == "public" and s["user"] is None
    c.post("/api/verbs", json={"action": "add", "infinitive": "ir"},
           headers={"X-Admin-Token": _admin_token()})
    assert len(c.get("/api/verbs").json()) == 1          # visible anonymously
    # signed-in users get their own isolated space, keyed by IDENTITY
    h1 = {"Authorization": "Bearer " + auth.sign({"tid": "hf-alex", "user": "alex"})}
    h2 = {"Authorization": "Bearer " + auth.sign({"tid": "hf-maria", "user": "maria"})}
    assert c.get("/api/verbs", headers=h1).json() == []  # not the public data
    # a PUBLIC token does not work for a signed-in user (sub binding) …
    r = c.post("/api/verbs", json={"action": "add", "infinitive": "ser"},
               headers={**h1, "X-Admin-Token": _admin_token()})
    assert r.status_code == 401 and "belongs to 'public'" in r.json()["error"]
    # … their OWN token does
    c.post("/api/verbs", json={"action": "add", "infinitive": "ser"},
           headers={**h1, "X-Admin-Token": _admin_token(sub="alex")})
    assert len(c.get("/api/verbs", headers=h1).json()) == 1
    assert c.get("/api/verbs", headers=h2).json() == []  # isolated from alex
    assert c.get("/api/state", headers=h1).json()["user"] == "alex"
    # the same token in ANOTHER browser reaches the same data (identity,
    # not browser, is the key)
    assert len(c.get("/api/verbs", headers=dict(h1)).json()) == 1


def test_oauth_callback_mints_identity_tenant(tmp_path, monkeypatch):
    c = _client(tmp_path, monkeypatch)
    monkeypatch.setenv("OAUTH_CLIENT_ID", "cid")
    monkeypatch.setenv("OAUTH_CLIENT_SECRET", "sec")
    monkeypatch.setattr(serve, "_exchange_code", lambda code, uri: "Alex.Evseenko")
    r = c.get("/oauth/callback?code=abc")
    assert r.status_code == 200
    token = r.text.split("aisverbo-tenant', ")[1].split(")")[0].strip('"')
    import auth
    p = auth.verify(token)
    assert p["tid"] == "hf-Alex-Evseenko"                # sanitized identity
    assert p["user"] == "Alex.Evseenko"
    # and /oauth/login redirects to the provider
    r = c.get("/oauth/login", follow_redirects=False)
    assert r.status_code == 307 or r.status_code == 302
    assert "oauth/authorize" in r.headers["location"]


def test_settings_limits_clamped(tmp_path, monkeypatch):
    c = _client(tmp_path, monkeypatch)
    r = c.post("/api/settings", json={"verb_count": 5000, "essay_words": 9999})
    assert r.json()["verb_count"] == 100 and r.json()["essay_words"] == 200
    r = c.post("/api/settings", json={"verb_count": -3, "essay_words": 1})
    assert r.json()["verb_count"] == 1 and r.json()["essay_words"] == 30


def test_auth_check_diagnoses_each_failure(tmp_path, monkeypatch):
    c = _client(tmp_path, monkeypatch)
    r = c.get("/api/auth/check").json()
    assert r["ok"] is False and "Request corpus access" in r["detail"]
    r = c.get("/api/auth/check", headers={"X-Admin-Token": "garbage"}).json()
    assert r["token_received"] and "signature" in r["detail"]
    r = c.get("/api/auth/check",
              headers={"X-Admin-Token": _admin_token()}).json()
    assert r["ok"] is True and r["roles"] == ["corpus_admin"]
    r = c.get("/api/auth/check",
              headers={"X-Admin-Token": _writer_token()}).json()
    assert r["roles"] == ["story_writer"]


def test_reader_reads_free_but_generates_nothing(tmp_path, monkeypatch):
    c = _client(tmp_path, monkeypatch)
    # story generation: blocked for readers
    r = c.post("/api/essay", json={"topic": "el mar", "verbs": [1], "tenses": [1]})
    assert r.status_code == 401 and "access token" in r.json()["error"]
    # cached card: free; uncached: blocked
    conn = dbmod.connect(str(tmp_path / "aisverbo.db"))
    dbmod.save_card(conn, "voy", "es", "en", {"word": "voy", "lemma": "ir"}, "x")
    ok = c.post("/api/card", json={"word": "voy"})
    assert ok.status_code == 200 and ok.json()["cached"] is True
    r = c.post("/api/card", json={"word": "nuevo"})
    assert r.status_code == 401 and "Story Writer" in r.json()["error"]
    r = c.post("/api/translate", json={"text": "hola"})
    assert r.status_code == 401
    # state reflects capabilities
    s = c.get("/api/state").json()
    assert s["can_write"] is False and s["can_admin"] is False
    s = c.get("/api/state", headers={"X-Admin-Token": _writer_token()}).json()
    assert s["can_write"] is True and s["can_admin"] is False


def test_writer_deletes_stories_and_admin_does_not(tmp_path, monkeypatch):
    c = _client(tmp_path, monkeypatch)
    conn = dbmod.connect(str(tmp_path / "aisverbo.db"))
    eid = dbmod.save_essay(conn, {"created_at": "x", "foreign_lang": "es",
                                  "native_lang": "en", "topic": "t",
                                  "title": "T", "body": "b",
                                  "verbs_csv": "", "tenses_csv": ""})
    dbmod.save_translation(conn, eid, "en", {"title": "T", "body": "b"}, "x")
    r = c.post("/api/essay/delete", json={"id": eid})
    assert r.status_code == 401
    r = c.post("/api/essay/delete", json={"id": eid},
               headers={"X-Admin-Token": _admin_token()})
    assert r.status_code == 403                 # corpus admin is NOT a writer
    r = c.post("/api/essay/delete", json={"id": eid},
               headers={"X-Admin-Token": _writer_token()})
    assert r.json()["deleted"] == 1
    assert conn.execute("SELECT COUNT(*) FROM translations").fetchone()[0] == 0


def test_token_revocation(tmp_path, monkeypatch):
    c = _client(tmp_path, monkeypatch)
    tok = _writer_token(sub="public")
    ok = c.get("/api/auth/check", headers={"X-Admin-Token": tok}).json()
    assert ok["ok"] is True
    # revoke via the admin API (by token)
    r = c.post("/api/admin/revoke", json={"token": tok},
               headers={"X-Admin-Token": _admin_token()})
    assert r.json()["revoked"] == "jw-public"
    bad = c.get("/api/auth/check", headers={"X-Admin-Token": tok}).json()
    assert bad["ok"] is False and "revoked" in bad["detail"]
    # the admin token itself still works
    assert c.get("/api/auth/check",
                 headers={"X-Admin-Token": _admin_token()}).json()["ok"]

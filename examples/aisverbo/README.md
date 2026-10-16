# aisVerbo — learn verb conjugation by story, not by table

An aiSlang multi-agent example. The hardest part of learning es / en / pt /
fr / it / de is the verb conjugation space — Spanish alone has ~90 unique
suffixes across tenses and persons, plus the irregulars. Grammar tables and
artificial examples don't stick; the forms you meet inside a story *about
something you chose* do. aisVerbo turns that observation into a pipeline:
**you pick a topic, the verbs and the tenses — the agents write you a short
story that uses exactly those verbs in exactly those tenses — you read it,
listen to it at your speed, and tap any word for its full grammar card.**

It generalizes the `essayist` CLI prototype (Spanish-only, flashcard-file
driven) into a multi-language web app with a persistent library.

## The behavior, end to end

1. **Course** (Setup tab): choose the language you learn (`en es pt fr it
   de`, extendable — add any language code+name and it joins every
   selector) and the language explanations should be in. The pairs may coincide —
   `en → en` is a legitimate course in formal English grammar. Switching
   languages keeps every language's data; switch back any time.
2. **Vocabulary** (Vocabulary tab) — this page manages the CORPUS, not a
   story's cast, and has its own LANGUAGE selector (corpora are per
   language; Generate top-N / Generate the inventory respect it): search,
   20-per-page pagination, header-click alphabetical sorting (unsorted =
   the lexicographer's frequency order), inline editing of infinitive and
   gloss (fix a typo, add a missing gloss), guarded deletion (a verb or
   tense used by a stored story refuses to go — referential integrity),
   and one-file Export/Import of the whole corpus (verbs + tenses,
   idempotent on load). Which items go into a story is chosen on Learn. one click generates the **top-N most
   frequent verbs** of the course language (N is configurable: 50, 100,
   500…), each glossed in your native language, by the `lexicographer`
   agent — batched 100 per call, already-known verbs excluded, so you can
   grow the list incrementally. The list is yours: add your own verbs,
   delete, or untick ones you already master. The `tensesmith` agent
   generates the language's **tense inventory** the same way (name, your-
   language name, a usage hint, an example); untick tenses you are not
   ready for.
3. **A learning session** (Learn tab): type a topic in any language and
   chip-select this story's verbs and tenses from the whole corpus —
   nothing is selected by default and a selection is required; filter to
   find verbs, quick-add a new verb right there (it joins the corpus and
   the selection), *none* clears filter and selection. Press *Write the
   story*. The `essayist` agent writes a ~180-
   word story (length configurable) in the course language that conjugates
   verbs **only in the allowed tenses** and bolds every vocabulary verb.
4. **Reading & listening**: ▶ plays the whole story; select any fragment
   and ▶ selection plays just that; the speed slider goes 0.5×–1.4×. The
   voice is the **browser's own speech synthesis** — free, offline-capable,
   per-language voices, which is also why the UI can run on a static host.
5. **Tap any word** — not only verbs — the card's conjugation table has a
   tense dropdown (defaulting to the tapped form's own tense): see the
   same verb across every tense of the corpus, with fresh examples, each
   conjugation cached DB-wide like everything else. Tap any word — and the `grammarian` agent returns
   the full card *in your native language*: lemma, part of speech, meaning
   (of the lemma and of this exact form in this sentence), the form
   analysis (tense/mood/person), the rule and suffix pattern that produce
   it (irregularities called out), the six-person conjugation row of this
   tense, IPA transcription, 2–3 example sentences (each playable),
   synonyms, usage notes. **Cards are cached forever** — each word costs
   one model call per language pair, ever.
6. **Translation** (right panel, next to the Word card, laid out like the
   story itself — title above body, same paragraph structure): translate the
   whole story (sentence-aligned, cached per language — ⟳ re-translates on
   demand) or just the selected fragment; selecting text in the original
   highlights its semantic counterpart in the translation. Every model
   result in the app is cached this way — word cards, story translations —
   and every cache has an explicit ⟳ to re-ask.
7. **Stories** (the landing tab): every story is stored. Search by any verb,
   word form or topic — `comeré` finds every story where you met that
   future form — and reopen it as a full learning session with the
   story's own verbs/tenses restored as the current selection. Each story
   Exports to one JSON (text + metadata + translations) and Imports back
   idempotently — share a story between installations without re-paying
   for its generation.

## The agents (`aisverbo.ais`)

| agent | model block | job |
|---|---|---|
| `lexicographer` | `scholar` (haiku 5.5 → local qwen) | top-N verb list, glossed, batched, dedup-aware |
| `tensesmith` | `scholar` | the language's tense inventory with hints and examples |
| `essayist` | `author` (own block — tune creativity independently) | the themed story under verb + tense constraints, vocabulary bolded |
| `grammarian` | `scholar` | the on-demand word card, cached in the DB |
| `translator` | `scholar` | sentence-aligned story/fragment translation, cached per essay+language |

Budget-capped ($2/day, $0.05/run) and routed by aiSlang; the app shells out
to `aislang chat <agent> aisverbo.ais` — no SDKs, no direct provider calls.
Costs in practice: a story ≈ 1 call; 100 vocabulary verbs ≈ 1 call; a word
card ≈ 1 call *once per word*. Text-to-speech costs **zero** (client-side).

## Run it locally

```bash
aislang validate examples/aisverbo/aisverbo.ais
aislang apply    examples/aisverbo/aisverbo.ais     # LiteLLM router + budgets
cd examples/aisverbo && make serve                  # → http://127.0.0.1:8787
```

The backend is **FastAPI** (`requirements.txt`: fastapi + uvicorn — the
only dependencies; `make serve` runs uvicorn); the agents bridge, the
store and the UI are dependency-free as before. All state lives in `app/generated/`
(SQLite + config, git-ignored; `AISVERBO_DATA=<dir>` relocates it — tests,
HF persistent storage, a second instance beside a live one). The schema
migrates in place on open (`PRAGMA user_version`), never dropping data: vocabulary and tenses per language, every
story, every word card.

```bash
make test        # offline test suite (the LLM bridge is faked)
```

**After editing any prompt in `prompts/`, run `make plan`.** aiSlang pins
every prompt's content hash in `aislang.lock`, and `aislang chat` refuses a
drifted prompt ("system prompt hash mismatch") — which surfaces in the app
as agent errors. `make plan` refreshes the lockfile; no stack restart is
needed (each chat call reads it fresh).

## Hosting / sharing (HuggingFace Spaces and beyond)

The app is deliberately split into a **pure-JSON backend** and a **single
static `ui.html`** whose only runtime needs are `fetch` and the browser's
`speechSynthesis`. That split maps onto three hosting modes:

1. **Local (today)** — the full app next to the aiSlang CLI, as above.
2. **HuggingFace Docker Space (today, free CPU tier) — the Space type to
   choose.** The provided `Dockerfile` installs the requirements, runs
   `aislang apply` + uvicorn on the `PORT=7860` HF expects, and turns on
   `AISVERBO_MULTI_TENANT=1`. Space secrets to set: `ANTHROPIC_API_KEY`
   (the LiteLLM stack) and `AISVERBO_SECRET` (the JWT signing secret).
   Enable persistent storage and point `AISVERBO_DATA=/data` at it, or
   tenant data dies with the container. Bake the `aislang` binary into
   the image (see the Dockerfile comments) until a public release
   artifact exists. No PRO subscription required — Docker Spaces have a
   free tier; Gradio buys nothing since the UI is plain HTML.

### Tenancy & auth

- **Tenants**: every browser obtains a signed tenant token at first load
  (`POST /api/tenant/new`, kept in localStorage) and sends it as
  `Authorization: Bearer …`; each tenant lives in its own SQLite + config
  under `generated/tenants/<id>/` — fully isolated vocabulary, stories,
  settings. In local mode (no `AISVERBO_MULTI_TENANT`) everyone shares
  the legacy single-user data, so nothing changes for a laptop install.
  Word-card/conjugation caches are per tenant today; a shared cross-
  tenant cache is a straightforward follow-up once billing wants it.
- **Admin**: corpus management (every `POST /api/verbs` and
  `POST /api/tenses`, and corpus import) requires a JWT with
  `role=admin` in the `X-Admin-Token` header. Mint yours:

  ```bash
  AISVERBO_SECRET=<the same secret> python3 app/get_token.py   # 365 days
  ```

  and paste it into **Setup → Admin token** (stored only in that
  browser). Reading, story writing, cards, conjugations, translations
  and story import stay open to every tenant.
- **Limits** (the app is free of charge): one vocabulary generation ≤
  **100 verbs**, one story ≤ **200 words** — clamped server-side.
3. **HuggingFace Static Space (future, free)** — once the aiSlang **Cloud
   API** exists, `ui.html` alone is the deployment: point its `fetch`
   base at the Cloud API and the whole app runs from a free Static Space,
   because speech is already client-side and nothing else needs a server.
   The JSON routes in `app/serve.py` are the contract such a Cloud API
   endpoint would implement.

### Cost posture (the app is free of charge for a while)

- **TTS: $0** — browser speech synthesis, every target language, rate
  control included. On Linux the browser needs `speech-dispatcher` /
  `espeak-ng` installed (the UI says so when no voices are found);
  Chrome/Edge/Safari ship voices out of the box. (A server-side voice such as Google Cloud TTS can be
  slotted in later as an optional enhancement — or a dedicated TTS model
  hosted on aiSlang Cloud.)
- **Grammar/story generation**: haiku-class calls behind aiSlang budget
  caps; word cards amortize to zero through the permanent cache.
- Specialized free NLP (conjugation tables, IPA) could further cut model
  calls; candidates (Verbix-style data, Wiktionary dumps, eSpeak for
  offline IPA) are an optimization for a later release — or dedicated
  models hosted on aiSlang Cloud.

## Roadmap (not in this release)

- **Your own essays**: record speech or type a text; the agents find and
  highlight grammar/style errors against the course level.
- **Pronunciation coaching**: read a story aloud (browser
  `SpeechRecognition` or an aiSlang-Cloud STT model), compare with the
  baseline, highlight the syllables to work on; related drills.
- **Spaced repetition**: the cards and the library already know every form
  you've met and when — a review queue falls out naturally.
- **aiSlang Cloud API**: swap the local `aislang chat` bridge for the Cloud
  endpoint; the UI does not change (see hosting mode 3).

## Layout

```
examples/aisverbo/
  aisverbo.ais        the project: budget, 2 model blocks, 4 prompts, 4 agents
  prompts/            lexicographer / tensesmith / essayist / grammarian
  app/
    serve.py          stdlib HTTP server + JSON API + background jobs
    db.py             SQLite store (verbs, tenses, essays, cards) + config
    llm.py            the aislang-chat bridge (shared pattern with aisGTM)
    ui.html           the whole UI — one static file
  tests/test_app.py   offline suite, LLM bridge faked
  Dockerfile          HuggingFace Docker Space deployment
```

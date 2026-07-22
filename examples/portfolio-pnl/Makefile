# portfolio-pnl — convenience targets for the local-first portfolio analyst.
#
# The compute core is standard-library Python 3 only (no pip installs). Only the
# unit tests need `pytest`; `make venv` sets up a throwaway one for you. The
# advisor stage (`make run`) additionally needs Docker + a chat model.

PY      ?= python3
VENV    ?= .venv
VENV_PY := $(VENV)/bin/python
AISLANG ?= aislang

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help.
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

.PHONY: demo
demo: ## Offline end-to-end: seed prices -> analyze -> render the dashboard.
	$(PY) scripts/seed_demo.py
	$(PY) scripts/analyze.py --persist --compact >/dev/null
	$(PY) scripts/render_dashboard.py
	@echo "open generated/dashboard.html in a browser"

.PHONY: fetch
fetch: ## Pull live prices (Yahoo / CoinGecko, key-free) into the DB.
	$(PY) scripts/fetch.py

.PHONY: analyze
analyze: ## Compute P&L / risk / anti-fragility + the recommendation; persist to the DB.
	$(PY) scripts/analyze.py --persist

.PHONY: dashboard
dashboard: ## Render generated/dashboard.html from the current DB.
	$(PY) scripts/render_dashboard.py

.PHONY: eval
eval: ## Golden eval: grade the deterministic recommendation engine (no LLM).
	$(PY) scripts/eval_recommend.py

.PHONY: venv
venv: ## Create a throwaway venv with pytest (only the tests need it).
	$(PY) -m venv $(VENV)
	$(VENV_PY) -m pip -q install pytest

.PHONY: test
test: venv ## Run the unit tests + the golden eval (via the venv).
	$(VENV_PY) -m pytest tests -q

.PHONY: validate
validate: ## Type-check the .ais with the aislang CLI.
	$(AISLANG) validate portfolio.ais

.PHONY: run
run: ## Full advisory pipeline (needs Docker + a chat model): ./run.sh
	./run.sh

.PHONY: clean
clean: ## Remove all regenerable artifacts (DB, dashboard, aislang runtime, caches).
	rm -rf generated $(VENV) .aislang aislang.lock litellm_config.yaml stack.yml
	find . -name __pycache__ -type d -prune -exec rm -rf {} +
	rm -rf .pytest_cache
	@echo "cleaned."

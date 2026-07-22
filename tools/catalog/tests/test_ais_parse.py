"""Unit tests for the lightweight .ais parser used by the catalog builder.

Run:  python3 -m pytest tools/catalog/tests -q
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import ais_parse as ap  # noqa: E402


SAMPLE = '''\
## Header line one.
## Header line two.
//
## Header line three.

project "demo-agent" {
  budget {
    hard_cap_per_day = $2
  }

  model "brain" {
    requires = [chat, reasoning]
    prefer   = [anthropic.claude_haiku_4_5]
    fallback = [ollama.qwen3_8b]
  }

  prompt "sys" {
    file    = "./prompts/sys.md"   # inline comment
    version = "1.2.0"
  }

  skill "compute" {
    exec        = "python3 ../../../scripts/x.py --flag"
    output_type = json
  }

  agent "worker" {
    model         = model.brain
    system_prompt = prompt.sys
    tools         = [mcp_server.docs]
    knowledge     = [vector_store.kb]
  }

  pipeline "flow" {
    steps  = [agent.worker, use other.thing]
    input  = "some input"
    output = "some output"
  }

  loop "cycle" {
    body      = pipeline.flow
    condition = "{{ output | startswith('ACCEPT') | not }}"
    max_iter  = 3
  }

  deploy "local" { target = compose }
}
'''


def _project():
    blocks = ap.parse(SAMPLE)
    projs = [b for b in blocks if b.type == "project"]
    assert len(projs) == 1
    return projs[0]


def test_project_and_name():
    p = _project()
    assert p.name == "demo-agent"


def test_nested_budget_block():
    p = _project()
    b = p.first("budget")
    assert b is not None
    assert b.props["hard_cap_per_day"] == "$2"


def test_model_lists():
    m = _project().first("model")
    assert m.name == "brain"
    assert ap.as_list(m.props["requires"]) == ["chat", "reasoning"]
    assert ap.as_list(m.props["prefer"]) == ["anthropic.claude_haiku_4_5"]
    assert ap.as_list(m.props["fallback"]) == ["ollama.qwen3_8b"]


def test_prompt_scalars_and_inline_comment():
    pr = _project().first("prompt")
    assert ap.unquote(pr.props["file"]) == "./prompts/sys.md"
    assert ap.unquote(pr.props["version"]) == "1.2.0"


def test_agent_refs():
    a = _project().first("agent")
    assert ap.ref_label(a.props["model"]) == "brain"
    assert ap.ref_label(a.props["system_prompt"]) == "sys"
    assert [ap.ref_label(t) for t in ap.as_list(a.props["tools"])] == ["docs"]
    assert [ap.ref_label(k) for k in ap.as_list(a.props["knowledge"])] == ["kb"]


def test_pipeline_steps_incl_use():
    pl = _project().first("pipeline")
    steps = ap.as_list(pl.props["steps"])
    assert steps == ["agent.worker", "use other.thing"]
    assert ap.unquote(pl.props["input"]) == "some input"


def test_loop_condition_with_braces_survives():
    lp = _project().first("loop")
    # The MiniJinja `{{ … }}` braces must not confuse the brace-matcher.
    assert ap.ref_label(lp.props["body"]) == "flow"
    assert "startswith('ACCEPT')" in ap.unquote(lp.props["condition"])
    assert lp.props["max_iter"] == "3"


def test_deploy_inline_block():
    d = _project().first("deploy")
    assert d.name == "local"
    assert d.props["target"] == "compose"


def test_leading_doc_extracts_comment_block():
    doc = ap.leading_doc(SAMPLE)
    assert "Header line one." in doc
    assert "Header line three." in doc
    assert "project" not in doc


def test_snippet_without_project_is_empty():
    assert ap.parse('percentage = 85%\n') == [] or \
        all(b.type != "project" for b in ap.parse('percentage = 85%\n'))

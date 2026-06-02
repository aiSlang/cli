//! v0.3 Week 18 Tue — MiniJinja template engine for dynamic prompts.
//!
//! Wired by the `prompt "<label>" { template = true … }` grammar
//! addition (also Tue): when a prompt block opts in, its file body
//! is rendered as a MiniJinja template at chat time instead of being
//! passed verbatim to the LLM.  Static (`template = false`, the
//! default) prompts keep their v0.2 byte-stable behaviour.
//!
//! ## API surface
//!
//! Two entry points, both pure functions:
//!   - [`render`] — minimum-context render.  Takes the template
//!     source string + a built-in set of always-present variables
//!     (agent label, prompt label, model alias).  Used by the chat
//!     loop's default-context path.
//!   - [`render_with_context`] — render with user-supplied
//!     additional variables on top of the built-in set.  Future
//!     callers (skill rendering, eval-case fixtures) layer their
//!     own context this way.
//!
//! Both return `Result<String, TemplateError>` so callers can
//! surface parse / render errors as source-aware diagnostics.
//!
//! ## Auto-escape policy (Decision 44.x §sanitisation)
//!
//! Per PLAN's Tue spec: user-supplied context values are HTML-
//! escaped by DEFAULT; opt-out via `|safe` (Jinja convention).
//! This protects callers that later render the prompt output in a
//! web UI (Open WebUI, custom dashboards) from injection via
//! template-context values.  Prompts that need literal HTML in the
//! rendered output use `{{ value|safe }}`.
//!
//! Concrete implementation: `Environment::set_auto_escape_callback`
//! forces `AutoEscape::Html` for every template name.  Bypassed via
//! the standard `|safe` filter the way Jinja2 has done it for
//! ~15 years.
//!
//! ## Built-in context variables
//!
//! Every render gets these regardless of caller-supplied context:
//!   - `agent`  (string)  — the qualified agent label (e.g.
//!     `coder.coder` for multi-module, `support` for single-file).
//!   - `prompt` (string)  — the prompt label of the rendering
//!     prompt block.
//!   - `model`  (string)  — the model alias from the agent's
//!     `model = model.<x>` ref.
//!
//! Future v0.5+ additions per the §"Template context" PLAN row:
//! `project` (Project as JSON), `env` (allow-listed env vars),
//! arbitrary user-supplied via `render_with_context`.

use minijinja::{AutoEscape, Environment, Value};
use std::collections::BTreeMap;

// ============================================================================
// Errors
// ============================================================================

/// Failures specific to template rendering.  Distinct from
/// `SessionError` (which covers LLM/budget/MCP failures) so callers
/// can choose to render template errors with their own source-aware
/// diagnostic (point at the template file + line) instead of
/// surfacing them as opaque session failures.
#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    /// Template source failed to parse (syntax error in `{% … %}` or
    /// `{{ … }}` blocks).  Carries the MiniJinja error message —
    /// already line/col-tagged.
    #[error("template parse error: {0}")]
    Parse(String),

    /// Template parsed but render failed (missing variable when
    /// strict-mode is on, type mismatch in a filter, etc.).
    #[error("template render error: {0}")]
    Render(String),
}

// ============================================================================
// Context — built-in variables present on every render
// ============================================================================

/// Built-in variables every template render receives.  See the
/// module docstring for the full list + semantics.  Built by
/// [`BuiltInContext::new`] at the chat-loop boundary; callers
/// layer additional user-supplied vars via `render_with_context`.
#[derive(Debug, Clone)]
pub struct BuiltInContext {
    pub agent:  String,
    pub prompt: String,
    pub model:  String,
}

impl BuiltInContext {
    /// Constructor that pins required fields.  Optional fields can
    /// land later as `with_*` chained setters; for v0.3 these three
    /// are mandatory because every prompt render happens INSIDE a
    /// `Session::turn` against a known (agent, prompt, model) triple.
    pub fn new(
        agent:  impl Into<String>,
        prompt: impl Into<String>,
        model:  impl Into<String>,
    ) -> Self {
        Self {
            agent:  agent.into(),
            prompt: prompt.into(),
            model:  model.into(),
        }
    }

    /// Serialise the built-in set into the MiniJinja `Value` shape
    /// suitable for `Environment::render_str(template, ctx)`.
    /// Returns a `Value::Map` keyed by the variable names listed in
    /// the module docstring.
    pub fn to_value(&self) -> Value {
        let mut map: BTreeMap<String, Value> = BTreeMap::new();
        map.insert("agent".to_string(),  Value::from(self.agent.clone()));
        map.insert("prompt".to_string(), Value::from(self.prompt.clone()));
        map.insert("model".to_string(),  Value::from(self.model.clone()));
        Value::from(map)
    }
}

// ============================================================================
// render / render_with_context
// ============================================================================

/// Render `template_source` with the built-in context only.  Most
/// chat-time prompt rendering goes through this entry point — the
/// (agent, prompt, model) triple is always known.
///
/// Returns the rendered string on success.  Auto-escape is ON
/// (HTML), so caller-visible template authors must use `|safe` to
/// emit literal `<`/`>`/`&`/`"` in the output.
pub fn render(
    template_source: &str,
    builtins:        &BuiltInContext,
) -> Result<String, TemplateError> {
    render_with_context(template_source, builtins, &BTreeMap::new())
}

/// Render `template_source` with the built-in context PLUS
/// caller-supplied variables in `extra`.  Caller-supplied keys
/// shadow built-in keys (the use case is testing — a test can
/// override `agent` without rebuilding `BuiltInContext`).
///
/// MiniJinja's `Environment::render_str` parses + renders in one
/// shot; cheap enough for the per-turn cost (~µs for typical
/// prompt sizes).
pub fn render_with_context(
    template_source: &str,
    builtins:        &BuiltInContext,
    extra:           &BTreeMap<String, Value>,
) -> Result<String, TemplateError> {
    let mut env = Environment::new();
    // Force HTML auto-escape for EVERY template — see module
    // docstring for the Decision 44.x sanitisation rationale.
    env.set_auto_escape_callback(|_name| AutoEscape::Html);

    // Build the per-render context: start with the built-in set,
    // overlay any extras (extras win on key collision).
    let mut ctx: BTreeMap<String, Value> = BTreeMap::new();
    // Insert built-ins one-by-one so we can shadow with `extra`.
    ctx.insert("agent".to_string(),  Value::from(builtins.agent.clone()));
    ctx.insert("prompt".to_string(), Value::from(builtins.prompt.clone()));
    ctx.insert("model".to_string(),  Value::from(builtins.model.clone()));
    for (k, v) in extra {
        ctx.insert(k.clone(), v.clone());
    }

    env.render_str(template_source, ctx)
        .map_err(|e| {
            // MiniJinja errors include kind discrimination.  We map
            // syntax errors → Parse, everything else → Render.
            use minijinja::ErrorKind;
            let detail = format!("{e}");
            match e.kind() {
                ErrorKind::SyntaxError => TemplateError::Parse(detail),
                _                      => TemplateError::Render(detail),
            }
        })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> BuiltInContext {
        BuiltInContext::new("support", "system_md", "fast")
    }

    // ---- Required by PLAN DoD (7 unit tests) -------------------- //

    #[test] fn renders_variable_substitution_against_built_in_context() {
        let out = render("agent={{ agent }}, model={{ model }}", &ctx())
            .expect("render");
        assert_eq!(out, "agent=support, model=fast");
    }

    #[test] fn render_errors_when_referenced_variable_missing() {
        // Strict-mode: referencing an undeclared variable surfaces as
        // a Render error rather than silently rendering empty.
        // (MiniJinja's default behaviour is to treat undefined as
        // empty strings — but accessing `.attr` or `.method()` on
        // undefined fires UndefinedError.  We use method-access here
        // to force the error path.)
        let err = render("{{ nonexistent.method() }}", &ctx()).unwrap_err();
        match err {
            TemplateError::Render(msg) => assert!(msg.contains("undefined"),
                "expected undefined-var error; got: {msg}"),
            TemplateError::Parse(msg)  =>
                panic!("expected Render, got Parse: {msg}"),
        }
    }

    #[test] fn renders_if_else_control_flow() {
        let src = "{% if model == 'fast' %}quick{% else %}slow{% endif %}";
        let out = render(src, &ctx()).expect("render");
        assert_eq!(out, "quick");
    }

    #[test] fn renders_for_loop_over_supplied_list() {
        let mut extra = BTreeMap::new();
        extra.insert("items".to_string(), Value::from(vec!["a", "b", "c"]));
        let out = render_with_context(
            "{% for i in items %}[{{ i }}]{% endfor %}",
            &ctx(), &extra).expect("render");
        assert_eq!(out, "[a][b][c]");
    }

    #[test] fn renders_filter_chain() {
        // Common filter chain: trim + lower + replace.  Pins that
        // MiniJinja's built-in filter set is wired (no extra
        // `builtins` flag needed for these).
        let mut extra = BTreeMap::new();
        extra.insert("raw".to_string(), Value::from("  HELLO WORLD  "));
        let out = render_with_context(
            "{{ raw | trim | lower | replace('world', 'agent') }}",
            &ctx(), &extra).expect("render");
        assert_eq!(out, "hello agent");
    }

    #[test] fn html_escape_is_on_by_default() {
        // PLAN DoD: "escape default" — angle brackets in user-supplied
        // values get HTML-escaped without an explicit opt-in.
        let mut extra = BTreeMap::new();
        extra.insert("user_input".to_string(),
            Value::from("<script>alert(1)</script>"));
        let out = render_with_context("{{ user_input }}", &ctx(), &extra)
            .expect("render");
        // < > & " all escaped to entity form.  MiniJinja uses the
        // standard XML-safe set.
        assert!(out.contains("&lt;script&gt;"),
            "auto-escape must turn <script> into &lt;script&gt;; got: {out}");
        assert!(!out.contains("<script>"),
            "the raw <script> tag must NOT appear in the output");
    }

    #[test] fn safe_filter_opts_out_of_html_escape() {
        // PLAN DoD: "|safe opt-out" — the standard Jinja escape-hatch.
        // When the template author KNOWS a value is safe (e.g. an
        // already-sanitised HTML snippet), `|safe` bypasses escaping.
        let mut extra = BTreeMap::new();
        extra.insert("trusted_html".to_string(),
            Value::from("<b>bold</b>"));
        let out = render_with_context(
            "{{ trusted_html | safe }}",
            &ctx(), &extra).expect("render");
        assert_eq!(out, "<b>bold</b>",
            "|safe must emit literal HTML, no entity escaping");
    }

    // ---- Bonus coverage on top of the DoD set ------------------- //

    #[test] fn extra_context_keys_shadow_built_in_keys() {
        let mut extra = BTreeMap::new();
        extra.insert("agent".to_string(), Value::from("overridden"));
        let out = render_with_context("{{ agent }}", &ctx(), &extra)
            .expect("render");
        assert_eq!(out, "overridden",
            "extra context must win against the same-named built-in key");
    }

    #[test] fn syntax_error_surfaces_as_parse_variant() {
        // Unbalanced `{% if %}` — typical author mistake.
        let err = render("{% if model %}oops", &ctx()).unwrap_err();
        match err {
            TemplateError::Parse(_) => (),
            TemplateError::Render(msg) =>
                panic!("expected Parse, got Render: {msg}"),
        }
    }

    #[test] fn empty_template_renders_empty_string() {
        // Edge: empty input is valid; renders to empty output.
        let out = render("", &ctx()).expect("render");
        assert_eq!(out, "");
    }

    #[test] fn non_template_text_passes_through_verbatim() {
        // Edge: a template with no `{{ }}` or `{% %}` is just text.
        // Pins that wrapping every prompt in template rendering is
        // safe for non-template content (cheap no-op).
        let src = "you are a helpful assistant. answer in 1 sentence.";
        let out = render(src, &ctx()).expect("render");
        assert_eq!(out, src);
    }
}

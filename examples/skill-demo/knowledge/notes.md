# Skill demo knowledge base

This file is read end-to-end by the `read_notes` MCP-delegate skill
in `skill-demo.ais` to demonstrate that `mcp = mcp_server.<x>.tool.
<y>` skills can drive the SAME filesystem MCP container that agents
use for `tools = [mcp_server.<x>]`.

Contents are arbitrary; the runtime just reads + returns them as
the skill's typed `string` output.

  - aislang v0.3 Week 18 Thu ships the `skill { … }` runtime.
  - Shell + MCP delegate variants both supported.
  - Sandbox: per-skill cwd + scrubbed PATH + denylist (shell only).

# MCP integration

`aislang mcp-serve` exposes the whole CLI surface as an
[MCP](https://modelcontextprotocol.io/) server, so an MCP host — Claude
Desktop, Cursor, or any other MCP-aware LLM app — can author, deploy, and chat
with `.ais` agents without you touching a terminal. The tools mirror the CLI:
`validate`, `plan`, `apply`, `destroy`, `status`, `chat_turn`, `eval`, and
more.

## Local (stdio) setup

Desktop hosts spawn `aislang mcp-serve --stdio` as a child process and talk to
it over stdin/stdout. Add the block below to your host's MCP config (a
ready-to-edit copy lives at
[`examples/claude_desktop_config.json`](https://github.com/aiSlang/cli/blob/main/examples/claude_desktop_config.json)):

```json
{
  "mcpServers": {
    "aislang": {
      "command": "/absolute/path/to/aislang",
      "args": ["mcp-serve", "--stdio"],
      "env": {
        "AISLANG_LITELLM_URL": "http://127.0.0.1:4000"
      }
    }
  }
}
```

### Config file location

For Claude Desktop, the config file lives at:

- **macOS:** `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Linux:** `~/.config/Claude/claude_desktop_config.json`
- **Windows:** `%APPDATA%\Claude\claude_desktop_config.json`

Other MCP hosts (Cursor, etc.) accept the same `mcpServers` block in their own
config location.

### Use an absolute `command` path

**This is the most common setup mistake.** GUI apps don't inherit your shell
`PATH`, so a bare `"aislang"` almost always fails with **"server
disconnected"**. Point `command` at the real binary — for example
`~/.local/bin/aislang` after an install, or `target/release/aislang` (or
`target/debug/aislang`) in a source checkout. Find the absolute path with:

```sh
command -v aislang          # if it's on your PATH
realpath target/release/aislang   # in a source checkout
```

### Environment and working directory

The `env` block is optional. The authoring tools (`validate`, `plan`,
`describe`, `lang_spec`) need nothing. Only the conversational tools
(`chat_turn`, `eval`) talk to a running LiteLLM — set `AISLANG_LITELLM_URL`
only if it isn't the `http://127.0.0.1:4000` default. Provider keys live in
LiteLLM, never in this config.

Because the host launches the server with an arbitrary working directory, pass
**absolute** `source_path`s to the tools (e.g.
`/home/you/project/support-agent.ais`, not `support-agent.ais`).

After editing the config, restart the host. The aislang tools then appear in
its tool list — try asking it to validate, apply, or run a chat turn against
one of your `.ais` files.

## HTTP transport

For network or remote MCP clients (in-cluster deployments, custom clients),
serve the same toolset over HTTP:

```sh
aislang mcp-serve --http 0.0.0.0:8080
```

The tools are then mounted at `http://host:8080/mcp`.

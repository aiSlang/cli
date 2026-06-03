# aiSlang overview

aiSlang is a declarative DSL for AI infrastructure.  A `.ais` source
file declares the resources an agent stack needs — models, prompts,
MCP servers, agents, deploy targets — and the `aislang` CLI compiles
that down to a runnable Compose stack (or a Helm chart, for
Kubernetes).

Core resource types: `project`, `budget`, `model`, `prompt`,
`mcp_server`, `agent`, `eval`, `secret`, `deploy`.

The runtime ships `aislang chat <agent> <project>` for interactive
testing + `aislang mcp-serve` for IDE / desktop-LLM integration
(Claude Desktop, Cursor, custom rmcp clients).

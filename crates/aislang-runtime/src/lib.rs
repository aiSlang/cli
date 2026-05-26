//! In-CLI agent runtime for v0.1 — the chat loop that `aislang chat`
//! runs in-process.
//!
//! Scope by week:
//!   - **Week 6 Tue (this file):** `mcp::StdioChild` — a thin facade
//!     over `rmcp`'s `TokioChildProcess` transport. Spawns the
//!     declared MCP server via `docker run -i --rm -v … <image> <args>`,
//!     lists its tools, calls them with JSON arguments, and shuts
//!     down cleanly. The facade isolates the rest of the runtime
//!     from `rmcp` API churn — Decision 5's v0.2 successor
//!     (`aislang-mcp-gateway` over SSE) swaps the transport without
//!     touching callers.
//!   - **Week 6 Wed:** `LiteLlmClient` + non-streaming chat loop.
//!   - **Week 6 Thu:** Tool-use loop, 8-iter cap, OpenAI/LiteLLM
//!     `tools` schema conversion.
//!
//! Async-first via `tokio`. The synchronous CLI (`aislang-cli`)
//! drives this crate via a per-command runtime built on demand —
//! we avoid making the whole CLI tokio-aware in v0.1 to keep the
//! existing sync subcommands (validate, plan, apply, destroy)
//! exactly as they are.

pub mod mcp;
pub mod litellm;
pub mod session;

pub use session::{Session, SessionError, load_system_prompt, hex_sha256};
pub use litellm::{LiteLlmClient, LiteLlmError, ChatRequest, ChatMessage, Role};

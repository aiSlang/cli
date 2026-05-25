//! Docker Compose emitter for aiSlang projects.
//!
//! Takes a resolved [`aislang_core::ast::Project`] (post type-check, post
//! model resolution, post lockfile pinning) and emits the matching
//! `docker-compose.yml` content.
//!
//! Implementation arrives in week 4 (skeleton) and week 6 (LiteLLM
//! sidecar + MCP server wiring). See PLAN-90-days.md.
//!
//! Design notes:
//!   - serialize from typed structs via `serde_yaml`; no string templating
//!   - LiteLLM runs as a sidecar service in the generated stack, never
//!     linked as a library (locked-in choice #11 in PLAN-90-days.md)
//!   - Jaeger is included by default for OTel-GenAI trace viewing (week 9)

use aislang_core::ast::Project;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmitError {
    #[error("compose emitter not implemented yet — scheduled for week 4")]
    NotImplemented,
}

/// Emit `docker-compose.yml` content for the given project.
pub fn emit(_project: &Project) -> Result<String, EmitError> {
    Err(EmitError::NotImplemented)
}

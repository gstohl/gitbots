//! Pure domain model for gitbots.
//!
//! No IO, no clocks, no randomness: callers supply ids and timestamps. This
//! keeps the crate buildable for `wasm32-unknown-unknown` so the same types
//! and folds run in the CLI, the MCP server and a Cloudflare Worker.

pub mod action;
pub mod board;
pub mod event;
pub mod id;
pub mod identity;
pub mod ledger;
pub mod manifest;
pub mod recipe;
pub mod redact;
pub mod stats;

pub use action::{ActionRun, JobResult, LogRef, RunStatus};
pub use board::{AttemptState, AttemptView, Board, LookupError, TaskStatus, TaskView};
pub use event::{Event, EventBody, kind};
pub use id::{AttemptId, EventId, IdError, ProjectId, RunId, SessionId, TaskId, Ulid};
pub use identity::{Actor, AgentDescriptor, Session, Via};
pub use manifest::{Authorization, Decision, Mandate, Manifest};
pub use recipe::Recipe;
pub use stats::{ActorStats, Stats};

#[cfg(test)]
mod tests;

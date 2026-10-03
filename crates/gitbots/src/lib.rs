//! gitbots: agent-native git.
//!
//! [`project::Project`] is the API shared by the `gitbots` CLI ([`cli`]) and the
//! MCP server ([`mcp`]) and the local web API ([`ui`]). [`cloud`] connects a
//! project to Cloudflare (Artifacts remotes and the gitbots Worker).

pub mod cli;
pub mod cloud;
pub mod mcp;
pub mod output;
pub mod project;
pub mod ui;

pub use project::{ActorCtx, Project};

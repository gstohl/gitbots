//! Shared, IO-free pieces of gitbots's Cloudflare control plane (`docs/CLOUD.md`).
//!
//! Used by both sides of the `/v1` API: the native `gitbots` CLI (client) and
//! `gitbots-worker` (server, wasm32). Like `gitbots-core`, this crate does no IO
//! and must build for `wasm32-unknown-unknown`. Git object reads go through
//! the [`source::TreeSource`] trait, which the Worker implements over the
//! Artifacts binding and the tests implement in memory.

pub mod api;
pub mod commits;
pub mod dashboard;
pub mod diff;
pub mod index;
pub mod naming;
pub mod source;

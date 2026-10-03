//! gitbots's git layer: ledger branches, workrooms (worktrees), hooks and sync.
//!
//! Object and ref plumbing goes through gix. Every ledger write is an
//! in-memory commit plus a compare-and-swap ref update; the working tree and
//! the index are never touched. Porcelain (worktrees, diffs, trailers,
//! fetch/push, merges) shells out to the `git` CLI.
//!
//! Public functions return [`anyhow::Result`]. Conditions callers branch on
//! are a [`LedgerError`] inside the `anyhow::Error`; detect them with
//! `err.downcast_ref::<LedgerError>()`.

mod activity;
mod binding;
mod cli;
mod hooks;
mod ledger;
mod logs;
mod objects;
mod repo;
mod sync;

pub use activity::{Activity, AppendOutcome, EventsRead};
pub use binding::{read_binding, remove_binding, write_binding};
pub use hooks::{HOOK_MARKER, HOOKS, HookReport};
pub use ledger::{Ledger, MergeOutcome, quarantine_path};
pub use logs::Logs;
pub use repo::Repo;
pub use sync::{
    BranchPush, PushStatus, RemoteSpec, SyncReport, fetch_branch, push_branches, sync, sync_with,
};

use gitbots_core::ledger::LedgerMeta;

/// Default ledger branches (short names). The manifest may override them.
pub const ACTIVITY_BRANCH: &str = "gitbots/activity";
pub const LOGS_BRANCH: &str = "gitbots/logs";

/// How often a ledger update re-reads the tip and retries after losing a
/// compare-and-swap race.
pub const CAS_RETRIES: usize = 5;

/// Conditions callers are expected to branch on.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// The tip gitbots last saw or wrote is no longer in the branch history.
    /// Ledgers are append-only, so this means someone rewrote or deleted it.
    /// [`Ledger::accept_rewrite`] acknowledges it.
    #[error(
        "ledger branch `{branch}` was rewritten: the last tip gitbots saw is no longer in its history"
    )]
    Rewritten { branch: String },
    /// `LEDGER.json` differs in project, kind, format or epoch.
    #[error("ledger branch `{branch}` is incompatible: ours is {ours:?}, theirs is {theirs:?}")]
    Incompatible { branch: String, ours: LedgerMeta, theirs: LedgerMeta },
    /// Lost the compare-and-swap race [`CAS_RETRIES`] times in a row.
    #[error("gave up after {CAS_RETRIES} compare-and-swap retries on a ledger branch")]
    CasExhausted,
    /// `merge_into` found conflicts; nothing was committed or moved.
    #[error("merge has conflicts in {}", paths.join(", "))]
    MergeConflict { paths: Vec<String> },
    /// The ledger branch does not exist yet; call [`Ledger::ensure`] first.
    #[error("ledger branch `{branch}` does not exist (run `gitbots init`)")]
    Missing { branch: String },
}

#[cfg(test)]
mod tests;

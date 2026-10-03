//! Results of actions runs, as recorded in the ledger.
//!
//! The workflow *spec* and the runners live in `gitbots-actions`; these are the
//! shared result types the board and stats folds need to read.

use serde::{Deserialize, Serialize};

use crate::id::{AttemptId, RunId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Success,
    Failure,
    TimedOut,
    Cancelled,
    /// Not run because a job it `needs` did not succeed.
    Skipped,
}

impl RunStatus {
    pub fn is_success(self) -> bool {
        self == Self::Success
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
        }
    }
}

/// Pointer to a blob on a ledger branch, e.g. `gitbots/logs:runs/run_x/test.log`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRef {
    pub branch: String,
    pub path: String,
}

impl std::fmt::Display for LogRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.branch, self.path)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    pub name: String,
    pub status: RunStatus,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Name (or index) of the first step that did not succeed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<LogRef>,
}

/// One workflow execution; payload of the `action.completed` event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRun {
    pub run: RunId,
    pub workflow: String,
    /// Event kind or `manual` that started the run.
    pub trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<AttemptId>,
    /// Commit the run executed against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Runner backend, e.g. `local`.
    pub runner: String,
    pub status: RunStatus,
    pub duration_ms: u64,
    pub jobs: Vec<JobResult>,
}

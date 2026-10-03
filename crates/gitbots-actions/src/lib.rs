//! gitbots Actions: a GitHub-Actions-like workflow engine driven by ledger events.
//!
//! - [`spec`]: workflow files (`.gitbots/actions/*.toml`), parsing and validation;
//! - [`runner`]: the [`Runner`] trait and its backends (`local`, `celesto`);
//! - [`engine`]: runs a workflow and folds the jobs into an [`ActionRun`].
//!
//! This crate does not touch git. The facade reads workflow files from the
//! trusted branch, passes them to [`load_workflows`], picks the ones that
//! match a trigger with [`matching`], runs them with [`run_workflow`] and
//! records the result (`action.completed`) and logs (through a [`LogSink`]).

pub mod engine;
pub mod runner;
pub mod spec;

pub use engine::{
    LogSink, MemorySink, NullSink, RunRequest, load_workflows, matching, run_workflow,
};
pub use gitbots_core::{ActionRun, JobResult, LogRef, RunStatus};
pub use runner::{
    BoxFuture, JobContext, JobOutcome, LocalRunner, Runner, RunnerRegistry, Source,
    UnavailableRunner,
};
#[cfg(feature = "celesto")]
pub use runner::{CelestoConfig, CelestoRunner};
pub use spec::{Job, SpecError, Step, Workflow};

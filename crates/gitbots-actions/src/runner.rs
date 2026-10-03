//! Runners execute one job each; the engine picks one by the job's `runs-on`.
//!
//! A runner gets a fully prepared [`JobContext`] and returns a [`JobOutcome`]
//! with the raw job log. Redaction, size caps and log storage are the
//! engine's job, so a runner only has to execute and capture output.
//!
//! - [`LocalRunner`] (`local`): `sh -c` per step on this machine.
//! - `CelestoRunner` (`celesto`, feature `celesto`): a hosted Celesto computer.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use gitbots_core::RunStatus;
use gitbots_core::id::RunId;

use crate::spec::Job;

#[cfg(feature = "celesto")]
pub mod celesto;
mod local;

#[cfg(feature = "celesto")]
pub use celesto::{CelestoConfig, CelestoRunner};
pub use local::LocalRunner;

/// A boxed `Send` future, so [`Runner`] stays object safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Everything a runner needs to execute one job.
#[derive(Debug)]
pub struct JobContext<'a> {
    pub run: &'a RunId,
    /// Workflow name.
    pub workflow: &'a str,
    /// Job name (key in `[jobs]`).
    pub name: &'a str,
    pub job: &'a Job,
    /// Checkout to run in (local runners). Step `workdir`s are relative to it.
    pub workdir: &'a Path,
    /// Injected `GITBOTS_*`/`CI` vars < request extras < workflow env < job env.
    /// Step env is not included; runners layer it on per step.
    pub env: BTreeMap<String, String>,
    /// Where remote runners fetch the code from.
    pub source: Option<&'a Source>,
}

/// A commit that a remote runner can fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Clone URL without credentials; runners add their own auth.
    pub clone_url: String,
    pub commit: String,
}

/// What a runner reports for one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutcome {
    pub status: RunStatus,
    /// Exit code of the failing step, or `0` on success; `None` if the job
    /// timed out, was killed by a signal or never started a step.
    pub exit_code: Option<i32>,
    /// Label of the first step that did not succeed (see [`crate::Step::label`]).
    pub failed_step: Option<String>,
    /// Raw combined output, unredacted.
    pub log: Vec<u8>,
    pub duration_ms: u64,
}

impl JobOutcome {
    /// A failed job that never got to run a step, with `message` as its log.
    pub fn error(message: impl Into<String>) -> Self {
        let mut log = message.into().into_bytes();
        if !log.ends_with(b"\n") {
            log.push(b'\n');
        }
        Self { status: RunStatus::Failure, exit_code: None, failed_step: None, log, duration_ms: 0 }
    }
}

/// An execution backend for jobs.
pub trait Runner: Send + Sync {
    /// The `runs-on` value this runner serves.
    fn name(&self) -> &str;

    /// Execute every step of `ctx.job`. Return `Err` only for internal
    /// failures; a failing step is an `Ok` outcome with a failure status.
    fn run_job<'a>(&'a self, ctx: JobContext<'a>) -> BoxFuture<'a, anyhow::Result<JobOutcome>>;
}

/// A runner that is configured but unusable; fails every job with `reason`.
///
/// Registered instead of silently dropping a misconfigured runner, so the
/// job log says what is wrong rather than "unknown runner".
#[derive(Debug, Clone)]
pub struct UnavailableRunner {
    pub name: String,
    pub reason: String,
}

impl Runner for UnavailableRunner {
    fn name(&self) -> &str {
        &self.name
    }

    fn run_job<'a>(&'a self, _ctx: JobContext<'a>) -> BoxFuture<'a, anyhow::Result<JobOutcome>> {
        Box::pin(async move {
            Ok(JobOutcome::error(format!("runner `{}` is unavailable: {}", self.name, self.reason)))
        })
    }
}

/// Runners by name.
#[derive(Clone, Default)]
pub struct RunnerRegistry {
    runners: BTreeMap<String, Arc<dyn Runner>>,
}

impl RunnerRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// `local`, plus `celesto` when built with the `celesto` feature and
    /// `CELESTO_API_KEY` is set.
    pub fn with_defaults() -> Self {
        let mut reg = Self::new();
        reg.register(Arc::new(LocalRunner));
        #[cfg(feature = "celesto")]
        match CelestoRunner::from_env() {
            Ok(Some(runner)) => reg.register(Arc::new(runner)),
            Ok(None) => {}
            Err(e) => reg.register(Arc::new(UnavailableRunner {
                name: "celesto".into(),
                reason: format!("{e:#}"),
            })),
        }
        reg
    }

    /// Add or replace the runner for `runner.name()`.
    pub fn register(&mut self, runner: Arc<dyn Runner>) {
        self.runners.insert(runner.name().to_owned(), runner);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Runner>> {
        self.runners.get(name).cloned()
    }

    /// Registered runner names, sorted.
    pub fn names(&self) -> Vec<String> {
        self.runners.keys().cloned().collect()
    }
}

impl fmt::Debug for RunnerRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunnerRegistry").field("runners", &self.names()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_registers_and_lists() {
        let mut reg = RunnerRegistry::new();
        assert!(reg.names().is_empty());
        reg.register(Arc::new(UnavailableRunner { name: "zed".into(), reason: "r".into() }));
        reg.register(Arc::new(LocalRunner));
        assert_eq!(reg.names(), ["local", "zed"]);
        assert_eq!(reg.get("local").unwrap().name(), "local");
        assert!(reg.get("nope").is_none());
        assert!(RunnerRegistry::with_defaults().get("local").is_some());
    }
}

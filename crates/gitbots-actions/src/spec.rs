//! Workflow definitions: `.gitbots/actions/*.toml`.
//!
//! ```toml
//! name = "ci"
//! on = ["attempt.submitted", "manual"]
//!
//! [env]
//! RUST_BACKTRACE = "1"
//!
//! [jobs.test]
//! runs-on = "local"
//! timeout-secs = 900
//! steps = [
//!   { name = "fmt",  run = "cargo fmt --check" },
//!   { name = "test", run = "cargo test" },
//! ]
//!
//! [jobs.lint]
//! needs = ["test"]
//! steps = [{ run = "cargo clippy -- -D warnings" }]
//! ```
//!
//! Parsing is strict (`deny_unknown_fields`): a typo in a workflow should fail
//! loudly rather than silently drop a step. [`Workflow::from_toml`] parses and
//! validates; the engine only ever sees validated workflows.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Job timeout when `timeout-secs` is not set.
pub const DEFAULT_TIMEOUT_SECS: u64 = 3600;

/// One workflow file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Workflow {
    pub name: String,
    /// Event kinds (e.g. `attempt.submitted`), `manual`, or `*` for any.
    pub on: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub jobs: BTreeMap<String, Job>,
}

/// A named group of steps that runs on one runner.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Job {
    /// Runner name, see [`crate::RunnerRegistry`].
    #[serde(default = "local")]
    pub runs_on: String,
    /// Jobs that must succeed before this one runs.
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Whole-job timeout; [`DEFAULT_TIMEOUT_SECS`] if unset.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    pub steps: Vec<Step>,
}

/// One shell command.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Step {
    #[serde(default)]
    pub name: Option<String>,
    /// Script passed to `sh -c`.
    pub run: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Relative to the job's working directory; no `..`.
    #[serde(default)]
    pub workdir: Option<String>,
    /// A failure of this step does not fail the job.
    #[serde(default)]
    pub continue_on_error: bool,
}

fn local() -> String {
    "local".to_owned()
}

/// Why a workflow file was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    #[error("workflow file is not valid UTF-8")]
    Encoding,
    #[error("invalid workflow TOML: {0}")]
    Parse(String),
    #[error("workflow `name` is empty")]
    EmptyName,
    #[error("workflow has no triggers (`on` is empty)")]
    NoTriggers,
    #[error("workflow has an empty trigger in `on`")]
    EmptyTrigger,
    #[error("workflow has no jobs")]
    NoJobs,
    #[error("invalid job name `{0}`: use only A-Z, a-z, 0-9, `_` and `-`")]
    InvalidJobName(String),
    #[error("job `{0}` has no steps")]
    NoSteps(String),
    #[error("job `{job}` step {step} has an empty `run`")]
    EmptyRun { job: String, step: usize },
    #[error("job `{0}` has an empty `runs-on`")]
    EmptyRunsOn(String),
    #[error("job `{0}` has `timeout-secs = 0`")]
    ZeroTimeout(String),
    #[error("job `{job}` needs unknown job `{need}`")]
    UnknownNeed { job: String, need: String },
    #[error("job dependency cycle among: {}", .0.join(", "))]
    Cycle(Vec<String>),
    #[error(
        "job `{job}` step {step}: workdir `{workdir}` must be relative and must not contain `..`"
    )]
    InvalidWorkdir { job: String, step: usize, workdir: String },
    #[error("workflow name `{name}` is already used by `{first}`")]
    DuplicateName { name: String, first: String },
}

impl Workflow {
    /// Parse and validate one workflow file.
    pub fn from_toml(src: &str) -> Result<Workflow, SpecError> {
        let wf: Workflow = toml::from_str(src).map_err(|e| SpecError::Parse(e.to_string()))?;
        wf.validate()?;
        Ok(wf)
    }

    /// Check everything the engine relies on. Runner names are checked at run
    /// time, since the available runners depend on the host.
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.name.trim().is_empty() {
            return Err(SpecError::EmptyName);
        }
        if self.on.is_empty() {
            return Err(SpecError::NoTriggers);
        }
        if self.on.iter().any(|t| t.trim().is_empty()) {
            return Err(SpecError::EmptyTrigger);
        }
        if self.jobs.is_empty() {
            return Err(SpecError::NoJobs);
        }
        for (name, job) in &self.jobs {
            if name.is_empty()
                || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            {
                return Err(SpecError::InvalidJobName(name.clone()));
            }
            if job.runs_on.trim().is_empty() {
                return Err(SpecError::EmptyRunsOn(name.clone()));
            }
            if job.timeout_secs == Some(0) {
                return Err(SpecError::ZeroTimeout(name.clone()));
            }
            if job.steps.is_empty() {
                return Err(SpecError::NoSteps(name.clone()));
            }
            for (i, step) in job.steps.iter().enumerate() {
                if step.run.trim().is_empty() {
                    return Err(SpecError::EmptyRun { job: name.clone(), step: i + 1 });
                }
                if let Some(dir) = &step.workdir
                    && !is_safe_relative(dir)
                {
                    return Err(SpecError::InvalidWorkdir {
                        job: name.clone(),
                        step: i + 1,
                        workdir: dir.clone(),
                    });
                }
            }
            for need in &job.needs {
                if !self.jobs.contains_key(need) {
                    return Err(SpecError::UnknownNeed { job: name.clone(), need: need.clone() });
                }
            }
        }
        self.job_order().map(drop)
    }

    /// Whether this workflow runs for `trigger` (exact match, or `*` in `on`).
    pub fn triggers_on(&self, trigger: &str) -> bool {
        self.on.iter().any(|t| t == trigger || t == "*")
    }

    /// Jobs in dependency order. Deterministic: among jobs that are ready at
    /// the same time, the alphabetically first runs first.
    pub fn job_order(&self) -> Result<Vec<&str>, SpecError> {
        let mut pending: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (name, job) in &self.jobs {
            let mut needs = BTreeSet::new();
            for need in &job.needs {
                if !self.jobs.contains_key(need) {
                    return Err(SpecError::UnknownNeed { job: name.clone(), need: need.clone() });
                }
                needs.insert(need.as_str());
            }
            pending.insert(name, needs);
        }
        let mut order = Vec::with_capacity(pending.len());
        // Kahn's algorithm; `BTreeMap` iteration gives the alphabetical tie-break.
        while let Some(next) =
            pending.iter().find(|(_, needs)| needs.is_empty()).map(|(name, _)| *name)
        {
            pending.remove(next);
            for needs in pending.values_mut() {
                needs.remove(next);
            }
            order.push(next);
        }
        if pending.is_empty() {
            Ok(order)
        } else {
            Err(SpecError::Cycle(pending.keys().map(|s| (*s).to_owned()).collect()))
        }
    }
}

impl Job {
    /// Effective whole-job timeout.
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS))
    }
}

impl Step {
    /// Display name: `name`, or `#<1-based index>` if unnamed.
    pub fn label(&self, index: usize) -> String {
        match &self.name {
            Some(name) if !name.trim().is_empty() => name.clone(),
            _ => format!("#{}", index + 1),
        }
    }
}

fn is_safe_relative(dir: &str) -> bool {
    !dir.is_empty()
        && !dir.starts_with('/')
        && !dir.starts_with('\\')
        && Path::new(dir)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
name = "ci"
on = ["attempt.submitted", "manual"]

[env]
RUST_BACKTRACE = "1"

[jobs.test]
runs-on = "local"
timeout-secs = 900
steps = [
  { name = "fmt",  run = "cargo fmt --check" },
  { name = "test", run = "cargo test" },
]

[jobs.lint]
needs = ["test"]
steps = [{ run = "cargo clippy -- -D warnings" }]
"#;

    fn wf(jobs: &[(&str, &[&str])]) -> Workflow {
        let mut src = String::from("name = \"w\"\non = [\"manual\"]\n");
        for (name, needs) in jobs {
            let needs: Vec<String> = needs.iter().map(|n| format!("\"{n}\"")).collect();
            src.push_str(&format!(
                "[jobs.{name}]\nneeds = [{}]\nsteps = [{{ run = \"true\" }}]\n",
                needs.join(", ")
            ));
        }
        toml::from_str(&src).unwrap()
    }

    #[test]
    fn parses_architecture_example() {
        let wf = Workflow::from_toml(EXAMPLE).unwrap();
        assert_eq!(wf.name, "ci");
        assert_eq!(wf.on, ["attempt.submitted", "manual"]);
        assert_eq!(wf.env["RUST_BACKTRACE"], "1");
        let test = &wf.jobs["test"];
        assert_eq!(test.runs_on, "local");
        assert_eq!(test.timeout_secs, Some(900));
        assert_eq!(test.timeout(), Duration::from_secs(900));
        assert_eq!(test.steps.len(), 2);
        assert_eq!(test.steps[1].name.as_deref(), Some("test"));
        let lint = &wf.jobs["lint"];
        assert_eq!(lint.runs_on, "local", "runs-on defaults to local");
        assert_eq!(lint.needs, ["test"]);
        assert_eq!(lint.timeout(), Duration::from_secs(DEFAULT_TIMEOUT_SECS));
        assert_eq!(lint.steps[0].label(0), "#1");
        assert!(!lint.steps[0].continue_on_error);
        assert_eq!(wf.job_order().unwrap(), ["test", "lint"]);
    }

    #[test]
    fn rejects_unknown_fields() {
        let typo = EXAMPLE.replace("timeout-secs", "timeout");
        assert!(matches!(Workflow::from_toml(&typo), Err(SpecError::Parse(_))));
        let snake = EXAMPLE.replace("runs-on", "runs_on");
        assert!(matches!(Workflow::from_toml(&snake), Err(SpecError::Parse(_))));
        let step =
            EXAMPLE.replace("run = \"cargo test\"", "run = \"cargo test\", shell = \"bash\"");
        assert!(matches!(Workflow::from_toml(&step), Err(SpecError::Parse(_))));
    }

    #[test]
    fn detects_cycles() {
        let w = wf(&[("a", &["b"]), ("b", &["c"]), ("c", &["a"]), ("d", &[])]);
        assert_eq!(w.job_order(), Err(SpecError::Cycle(vec!["a".into(), "b".into(), "c".into()])));
        assert!(matches!(w.validate(), Err(SpecError::Cycle(_))));
        let selfloop = wf(&[("a", &["a"])]);
        assert_eq!(selfloop.validate(), Err(SpecError::Cycle(vec!["a".into()])));
    }

    #[test]
    fn rejects_unknown_needs() {
        let w = wf(&[("a", &["nope"])]);
        assert_eq!(
            w.validate(),
            Err(SpecError::UnknownNeed { job: "a".into(), need: "nope".into() })
        );
        assert!(matches!(w.job_order(), Err(SpecError::UnknownNeed { .. })));
    }

    #[test]
    fn job_order_is_deterministic() {
        let w = wf(&[
            ("zeta", &[]),
            ("build", &[]),
            ("test", &["build"]),
            ("alpha", &["test", "zeta"]),
            ("docs", &[]),
        ]);
        let order = w.job_order().unwrap();
        assert_eq!(order, ["build", "docs", "test", "zeta", "alpha"]);
        for _ in 0..10 {
            assert_eq!(w.clone().job_order().unwrap(), order);
        }
    }

    #[test]
    fn triggers() {
        let w = Workflow::from_toml(EXAMPLE).unwrap();
        assert!(w.triggers_on("attempt.submitted"));
        assert!(w.triggers_on("manual"));
        assert!(!w.triggers_on("attempt"));
        assert!(!w.triggers_on("attempt.merged"));
        let any = Workflow::from_toml(
            &EXAMPLE.replace(r#"on = ["attempt.submitted", "manual"]"#, r#"on = ["*"]"#),
        )
        .unwrap();
        assert!(any.triggers_on("attempt.merged"));
    }

    #[test]
    fn validation_errors() {
        let check =
            |from: &str, to: &str| Workflow::from_toml(&EXAMPLE.replace(from, to)).unwrap_err();
        assert_eq!(check("name = \"ci\"", "name = \" \""), SpecError::EmptyName);
        assert_eq!(
            check(r#"on = ["attempt.submitted", "manual"]"#, "on = []"),
            SpecError::NoTriggers
        );
        assert_eq!(
            check("[jobs.lint]", "[jobs.\"li nt\"]"),
            SpecError::InvalidJobName("li nt".into())
        );
        assert_eq!(
            check("[{ run = \"cargo clippy -- -D warnings\" }]", "[]"),
            SpecError::NoSteps("lint".into())
        );
        assert_eq!(
            check("timeout-secs = 900", "timeout-secs = 0"),
            SpecError::ZeroTimeout("test".into())
        );
        assert_eq!(
            check("run = \"cargo test\"", "run = \"  \""),
            SpecError::EmptyRun { job: "test".into(), step: 2 }
        );
        for bad in ["../x", "a/../../b", "/etc", ""] {
            let err = check(
                "run = \"cargo test\"",
                &format!("run = \"cargo test\", workdir = \"{bad}\""),
            );
            assert!(matches!(err, SpecError::InvalidWorkdir { step: 2, .. }), "{bad}: {err}");
        }
        assert!(
            Workflow::from_toml(
                &EXAMPLE.replace("run = \"cargo test\"", "run = \"x\", workdir = \"./a/b\"")
            )
            .is_ok()
        );
        assert_eq!(
            Workflow::from_toml("name = \"x\"\non = [\"manual\"]\n[jobs]\n"),
            Err(SpecError::NoJobs)
        );
    }
}

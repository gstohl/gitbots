//! Runs a workflow: plans jobs, dispatches them to runners, stores their logs
//! and folds the results into an [`ActionRun`].
//!
//! The engine is source-agnostic: it never reads workflow files or writes
//! ledger branches itself. The facade loads workflow files (from the trusted
//! branch's git tree) with [`load_workflows`], and logs leave through a
//! [`LogSink`].
//!
//! Jobs run one at a time in [`Workflow::job_order`]. Each job goes through
//! [`execute_job`] (no shared mutable state) and then [`record`] (the sink),
//! so running independent jobs concurrently later only changes the loop in
//! [`run_workflow`].

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context as _;
use gitbots_core::id::{AttemptId, RunId};
use gitbots_core::ledger::{MAX_LOG_BYTES, run_log_path};
use gitbots_core::{ActionRun, JobResult, LogRef, RunStatus};

use crate::runner::{JobContext, JobOutcome, RunnerRegistry, Source};
use crate::spec::{Job, SpecError, Workflow};

/// Where job logs go, e.g. `gitbots/logs:runs/<run>/<job>.log`.
pub trait LogSink: Send {
    /// Store one job's (already redacted and capped) log. `Ok(None)` means
    /// the log was intentionally not kept.
    fn put(&mut self, run: &RunId, job: &str, log: &[u8]) -> anyhow::Result<Option<LogRef>>;
}

/// Discards logs.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl LogSink for NullSink {
    fn put(&mut self, _run: &RunId, _job: &str, _log: &[u8]) -> anyhow::Result<Option<LogRef>> {
        Ok(None)
    }
}

/// Keeps logs in memory as `(job, log)`; for tests and previews.
#[derive(Debug, Clone, Default)]
pub struct MemorySink {
    pub logs: Vec<(String, Vec<u8>)>,
}

impl MemorySink {
    /// The log stored for `job`, as text.
    pub fn get(&self, job: &str) -> Option<Cow<'_, str>> {
        self.logs.iter().find(|(j, _)| j == job).map(|(_, log)| String::from_utf8_lossy(log))
    }
}

impl LogSink for MemorySink {
    fn put(&mut self, run: &RunId, job: &str, log: &[u8]) -> anyhow::Result<Option<LogRef>> {
        self.logs.push((job.to_owned(), log.to_vec()));
        Ok(Some(LogRef { branch: "memory".into(), path: run_log_path(run, job) }))
    }
}

/// One request to run a workflow.
#[derive(Debug, Clone)]
pub struct RunRequest {
    pub run: RunId,
    /// Event kind or `manual`.
    pub trigger: String,
    /// Checkout the local runner works in.
    pub workdir: PathBuf,
    pub attempt: Option<AttemptId>,
    /// Commit being tested; exported as `GITBOTS_COMMIT`.
    pub commit: Option<String>,
    /// For remote runners that fetch the code themselves.
    pub source: Option<Source>,
    /// Extra env, above the injected `GITBOTS_*` vars and below workflow env.
    pub extra_env: BTreeMap<String, String>,
}

/// Run every job of `wf` and return the result to record as `action.completed`.
///
/// Fails only if `wf` is invalid or the sink fails; job failures are part of
/// the returned [`ActionRun`].
pub async fn run_workflow(
    wf: &Workflow,
    req: &RunRequest,
    runners: &RunnerRegistry,
    sink: &mut dyn LogSink,
) -> anyhow::Result<ActionRun> {
    let started = Instant::now();
    wf.validate().with_context(|| format!("workflow `{}` is invalid", wf.name))?;
    let order = wf.job_order()?;

    let mut statuses: HashMap<&str, RunStatus> = HashMap::new();
    let mut jobs = Vec::with_capacity(order.len());
    for name in &order {
        let job = &wf.jobs[*name];
        let outcome = match blocked_by(job, &statuses) {
            Some(reason) => skipped(reason),
            None => execute_job(wf, req, runners, name, job).await,
        };
        statuses.insert(name, outcome.status);
        jobs.push(record(sink, &req.run, name, outcome)?);
    }

    let mut runner_names: Vec<&str> = Vec::new();
    for name in &order {
        let runs_on = wf.jobs[*name].runs_on.as_str();
        if !runner_names.contains(&runs_on) {
            runner_names.push(runs_on);
        }
    }
    let status = match jobs.iter().find(|j| !j.status.is_success()) {
        None => RunStatus::Success,
        Some(j) if j.status == RunStatus::TimedOut => RunStatus::TimedOut,
        Some(_) => RunStatus::Failure,
    };
    Ok(ActionRun {
        run: req.run.clone(),
        workflow: wf.name.clone(),
        trigger: req.trigger.clone(),
        attempt: req.attempt.clone(),
        commit: req.commit.clone(),
        runner: runner_names.join(","),
        status,
        duration_ms: millis(started),
        jobs,
    })
}

/// Why `job` must be skipped, if any of its `needs` did not succeed.
fn blocked_by(job: &Job, statuses: &HashMap<&str, RunStatus>) -> Option<String> {
    let failed: Vec<String> = job
        .needs
        .iter()
        .filter_map(|n| match statuses.get(n.as_str()) {
            Some(s) if s.is_success() => None,
            Some(s) => Some(format!("`{n}` ({})", s.as_str())),
            None => Some(format!("`{n}` (not run)")),
        })
        .collect();
    (!failed.is_empty())
        .then(|| format!("skipped: needed job {} did not succeed", failed.join(", ")))
}

fn skipped(reason: String) -> JobOutcome {
    JobOutcome { status: RunStatus::Skipped, ..JobOutcome::error(reason) }
}

/// Env for a job: injected vars < `extra_env` < workflow env < job env.
pub fn job_env(wf: &Workflow, req: &RunRequest, name: &str, job: &Job) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let mut set = |k: &str, v: &str| {
        env.insert(k.to_owned(), v.to_owned());
    };
    set("CI", "true");
    set("GITBOTS", "1");
    set("GITBOTS_RUN_ID", req.run.as_str());
    set("GITBOTS_WORKFLOW", &wf.name);
    set("GITBOTS_JOB", name);
    set("GITBOTS_TRIGGER", &req.trigger);
    if let Some(attempt) = &req.attempt {
        set("GITBOTS_ATTEMPT", attempt.as_str());
    }
    if let Some(commit) = &req.commit {
        set("GITBOTS_COMMIT", commit);
    }
    for layer in [&req.extra_env, &wf.env, &job.env] {
        env.extend(layer.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    env
}

/// Run one job on its runner. Never fails: problems become a failed outcome
/// whose log explains them.
pub async fn execute_job(
    wf: &Workflow,
    req: &RunRequest,
    runners: &RunnerRegistry,
    name: &str,
    job: &Job,
) -> JobOutcome {
    let Some(runner) = runners.get(&job.runs_on) else {
        let available = runners.names();
        let available = if available.is_empty() { "none".to_owned() } else { available.join(", ") };
        return JobOutcome::error(format!(
            "unknown runner `{}` (runs-on); available runners: {available}",
            job.runs_on
        ));
    };
    let started = Instant::now();
    let ctx = JobContext {
        run: &req.run,
        workflow: &wf.name,
        name,
        job,
        workdir: &req.workdir,
        env: job_env(wf, req, name, job),
        source: req.source.as_ref(),
    };
    match runner.run_job(ctx).await {
        Ok(outcome) => outcome,
        Err(e) => JobOutcome {
            duration_ms: millis(started),
            ..JobOutcome::error(format!("runner `{}` failed: {e:#}", runner.name()))
        },
    }
}

/// Redact and cap a job's log, store it, and build its [`JobResult`].
pub fn record(
    sink: &mut dyn LogSink,
    run: &RunId,
    name: &str,
    outcome: JobOutcome,
) -> anyhow::Result<JobResult> {
    let log = prepare_log(&outcome.log);
    let log =
        sink.put(run, name, &log).with_context(|| format!("storing the log of job `{name}`"))?;
    Ok(JobResult {
        name: name.to_owned(),
        status: outcome.status,
        duration_ms: outcome.duration_ms,
        exit_code: outcome.exit_code,
        failed_step: outcome.failed_step,
        log,
    })
}

/// Redact secrets, then cap at [`MAX_LOG_BYTES`] (redacting first, so a cut
/// can't split a secret into an unrecognizable fragment).
pub fn prepare_log(raw: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(raw);
    let (redacted, _) = gitbots_core::redact::redact(&text);
    cap(&redacted, MAX_LOG_BYTES).into_owned().into_bytes()
}

/// Keep the head and the tail of `text` within `max` bytes, with a marker in
/// between. The tail usually holds the error, the head shows what ran.
fn cap(text: &str, max: usize) -> Cow<'_, str> {
    if text.len() <= max {
        return Cow::Borrowed(text);
    }
    let marker =
        format!("\n... [gitbots: log truncated, {} bytes omitted] ...\n", text.len() - max);
    let budget = max.saturating_sub(marker.len());
    let head = floor_char_boundary(text, budget / 2);
    let tail = ceil_char_boundary(text, text.len() - (budget - head));
    let mut out = String::with_capacity(max);
    out.push_str(&text[..head]);
    out.push_str(&marker);
    out.push_str(&text[tail..]);
    Cow::Owned(out)
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn millis(since: Instant) -> u64 {
    since.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

/// Parse workflow files given as `(path, contents)`. Files not ending in
/// `.toml` are ignored. Results are sorted by path; a workflow whose `name`
/// repeats an earlier one is reported as an error.
#[allow(clippy::type_complexity)] // plain tuples keep the facade free of extra types
pub fn load_workflows(
    files: &[(String, Vec<u8>)],
) -> (Vec<(String, Workflow)>, Vec<(String, SpecError)>) {
    let mut files: Vec<&(String, Vec<u8>)> =
        files.iter().filter(|(path, _)| path.ends_with(".toml")).collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut ok: Vec<(String, Workflow)> = Vec::new();
    let mut errors = Vec::new();
    for (path, bytes) in files {
        let parsed = std::str::from_utf8(bytes)
            .map_err(|_| SpecError::Encoding)
            .and_then(Workflow::from_toml);
        match parsed {
            Ok(wf) => match ok.iter().find(|(_, other)| other.name == wf.name) {
                Some((first, _)) => errors.push((
                    path.clone(),
                    SpecError::DuplicateName { name: wf.name, first: first.clone() },
                )),
                None => ok.push((path.clone(), wf)),
            },
            Err(e) => errors.push((path.clone(), e)),
        }
    }
    (ok, errors)
}

/// The workflows that run for `trigger`.
pub fn matching<'a>(
    workflows: &'a [(String, Workflow)],
    trigger: &str,
) -> Vec<&'a (String, Workflow)> {
    workflows.iter().filter(|(_, wf)| wf.triggers_on(trigger)).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gitbots_core::id::Ulid;

    use super::*;
    use crate::runner::{BoxFuture, LocalRunner, Runner};

    fn request(dir: &std::path::Path) -> RunRequest {
        RunRequest {
            run: RunId::from_ulid(Ulid::from_parts(1_791_023_460_000, 7)),
            trigger: "manual".into(),
            workdir: dir.to_path_buf(),
            attempt: Some(AttemptId::from_ulid(Ulid::from_parts(1, 2))),
            commit: Some("abc123".into()),
            source: None,
            extra_env: BTreeMap::new(),
        }
    }

    fn local() -> RunnerRegistry {
        let mut reg = RunnerRegistry::new();
        reg.register(Arc::new(LocalRunner));
        reg
    }

    #[tokio::test]
    async fn needs_failures_skip_dependents_transitively() {
        let wf = Workflow::from_toml(
            r#"
name = "ci"
on = ["manual"]
[jobs.build]
steps = [{ run = "exit 2" }]
[jobs.test]
needs = ["build"]
steps = [{ run = "touch test-ran" }]
[jobs.deploy]
needs = ["test"]
steps = [{ run = "touch deploy-ran" }]
[jobs.docs]
steps = [{ run = "echo docs" }]
"#,
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut sink = MemorySink::default();
        let run = run_workflow(&wf, &request(dir.path()), &local(), &mut sink).await.unwrap();

        let status: Vec<(&str, RunStatus)> =
            run.jobs.iter().map(|j| (j.name.as_str(), j.status)).collect();
        assert_eq!(
            status,
            [
                ("build", RunStatus::Failure),
                ("docs", RunStatus::Success),
                ("test", RunStatus::Skipped),
                ("deploy", RunStatus::Skipped)
            ]
        );
        assert_eq!(run.status, RunStatus::Failure);
        assert_eq!(run.jobs[0].exit_code, Some(2));
        assert_eq!(run.jobs[0].failed_step.as_deref(), Some("#1"));
        assert!(!dir.path().join("test-ran").exists() && !dir.path().join("deploy-ran").exists());
        assert_eq!(run.runner, "local");
        assert_eq!(run.workflow, "ci");
        assert_eq!(run.commit.as_deref(), Some("abc123"));

        // Every job, skipped ones included, has a stored log.
        assert_eq!(sink.logs.len(), 4);
        assert!(sink.get("test").unwrap().contains("needed job `build` (failure)"));
        assert!(sink.get("deploy").unwrap().contains("`test` (skipped)"));
        assert!(sink.get("docs").unwrap().contains("docs\n== exit 0"));
        let log = run.jobs[1].log.as_ref().unwrap();
        assert_eq!(log.branch, "memory");
        assert_eq!(log.path, run_log_path(&run.run, "docs"));
    }

    #[tokio::test]
    async fn logs_are_redacted_and_env_is_layered() {
        let wf = Workflow::from_toml(
            r#"
name = "env"
on = ["manual"]
env = { LAYER_W = "workflow", LAYER_J = "workflow", GITBOTS_JOB = "overridden" }
[jobs.show]
env = { LAYER_J = "job" }
steps = [{ run = 'echo "ci=$CI gitbots=$GITBOTS run=$GITBOTS_RUN_ID wf=$GITBOTS_WORKFLOW job=$GITBOTS_JOB trig=$GITBOTS_TRIGGER att=$GITBOTS_ATTEMPT commit=$GITBOTS_COMMIT x=$X w=$LAYER_W j=$LAYER_J"; echo ghp_0123456789abcdefghijklmnopqrstuvwxyzAB' }]
"#,
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut req = request(dir.path());
        req.extra_env.insert("X".into(), "extra".into());
        req.extra_env.insert("LAYER_W".into(), "extra".into());
        let mut sink = MemorySink::default();
        let run = run_workflow(&wf, &req, &local(), &mut sink).await.unwrap();
        assert_eq!(run.status, RunStatus::Success);

        let log = sink.get("show").unwrap();
        let expected = format!(
            "ci=true gitbots=1 run={} wf=env job=overridden trig=manual att={} commit=abc123 x=extra w=workflow j=job\n",
            req.run,
            req.attempt.as_ref().unwrap()
        );
        assert!(log.contains(&expected), "{log}");
        assert!(!log.contains("ghp_0123"), "{log}");
        assert!(log.contains("[REDACTED]"), "{log}");
    }

    #[tokio::test]
    async fn unknown_runner_fails_with_an_explanation() {
        let wf = Workflow::from_toml(
            "name = \"x\"\non = [\"manual\"]\n[jobs.a]\nruns-on = \"gpu\"\nsteps = [{ run = \"true\" }]\n\
             [jobs.b]\nsteps = [{ run = \"true\" }]\n",
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut sink = MemorySink::default();
        let run = run_workflow(&wf, &request(dir.path()), &local(), &mut sink).await.unwrap();
        assert_eq!(run.jobs[0].status, RunStatus::Failure);
        assert_eq!(run.jobs[1].status, RunStatus::Success);
        assert_eq!(run.status, RunStatus::Failure);
        assert_eq!(run.runner, "gpu,local");
        let log = sink.get("a").unwrap();
        assert!(
            log.contains("unknown runner `gpu`") && log.contains("available runners: local"),
            "{log}"
        );
    }

    struct Fixed(RunStatus);

    impl Runner for Fixed {
        fn name(&self) -> &str {
            "fixed"
        }

        fn run_job<'a>(&'a self, ctx: JobContext<'a>) -> BoxFuture<'a, anyhow::Result<JobOutcome>> {
            Box::pin(async move {
                if ctx.name == "boom" {
                    anyhow::bail!("backend exploded");
                }
                Ok(JobOutcome { status: self.0, ..JobOutcome::error("") })
            })
        }
    }

    #[tokio::test]
    async fn overall_status_follows_the_first_non_success() {
        let src = "name = \"x\"\non = [\"manual\"]\n[jobs.a]\nruns-on = \"fixed\"\nsteps = [{ run = \"true\" }]\n";
        let wf = Workflow::from_toml(src).unwrap();
        let dir = tempfile::tempdir().unwrap();
        for (job_status, run_status) in [
            (RunStatus::Success, RunStatus::Success),
            (RunStatus::TimedOut, RunStatus::TimedOut),
            (RunStatus::Cancelled, RunStatus::Failure),
        ] {
            let mut reg = RunnerRegistry::new();
            reg.register(Arc::new(Fixed(job_status)));
            let run = run_workflow(&wf, &request(dir.path()), &reg, &mut NullSink).await.unwrap();
            assert_eq!(run.status, run_status);
            assert_eq!(run.jobs[0].log, None);
        }

        let wf = Workflow::from_toml(&src.replace("jobs.a", "jobs.boom")).unwrap();
        let mut reg = RunnerRegistry::new();
        reg.register(Arc::new(Fixed(RunStatus::Success)));
        let mut sink = MemorySink::default();
        let run = run_workflow(&wf, &request(dir.path()), &reg, &mut sink).await.unwrap();
        assert_eq!(run.status, RunStatus::Failure);
        assert!(sink.get("boom").unwrap().contains("runner `fixed` failed: backend exploded"));
    }

    #[test]
    fn cap_keeps_head_and_tail() {
        let text = format!("{}{}", "a".repeat(600), "é".repeat(300));
        let capped = cap(&text, 400);
        assert!(capped.len() <= 400, "{}", capped.len());
        assert!(
            capped.starts_with("aaa") && capped.ends_with("éé") && capped.contains("log truncated")
        );
        assert!(matches!(cap("short", 400), Cow::Borrowed("short")));
        let big = vec![b'x'; MAX_LOG_BYTES + 10];
        assert!(prepare_log(&big).len() <= MAX_LOG_BYTES);
    }

    #[test]
    fn load_and_match() {
        let ci = b"name = \"ci\"\non = [\"attempt.submitted\", \"manual\"]\n[jobs.t]\nsteps = [{ run = \"true\" }]\n";
        let nightly =
            b"name = \"nightly\"\non = [\"manual\"]\n[jobs.t]\nsteps = [{ run = \"true\" }]\n";
        let files = vec![
            ("actions/z.toml".to_owned(), ci.to_vec()),
            ("actions/a.toml".to_owned(), nightly.to_vec()),
            ("actions/README.md".to_owned(), b"# not a workflow".to_vec()),
            ("actions/bad.toml".to_owned(), b"name = 1".to_vec()),
            ("actions/bin.toml".to_owned(), vec![0xff, 0xfe]),
            ("actions/dup.toml".to_owned(), ci.to_vec()),
        ];
        let (ok, errors) = load_workflows(&files);
        let names: Vec<&str> = ok.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(names, ["actions/a.toml", "actions/dup.toml"]);
        let errs: Vec<&str> = errors.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(errs, ["actions/bad.toml", "actions/bin.toml", "actions/z.toml"]);
        assert!(matches!(errors[0].1, SpecError::Parse(_)));
        assert_eq!(errors[1].1, SpecError::Encoding);
        assert!(
            matches!(&errors[2].1, SpecError::DuplicateName { first, .. } if first == "actions/dup.toml")
        );

        let hits: Vec<&str> =
            matching(&ok, "attempt.submitted").iter().map(|(_, wf)| wf.name.as_str()).collect();
        assert_eq!(hits, ["ci"]);
        assert_eq!(matching(&ok, "manual").len(), 2);
        assert!(matching(&ok, "attempt.merged").is_empty());
    }
}

//! `runs-on = "local"`: each step is `sh -c <run>` on this machine.
//!
//! - cwd is the job workdir joined with the step's `workdir`;
//! - env is the parent environment, then [`JobContext::env`], then step env;
//! - stdout and stderr share one pipe, so the log keeps their order;
//! - every step runs in its own process group (unix). When the step's shell
//!   exits, or the job timeout hits, the whole group is killed, so background
//!   processes never outlive their step and a timeout can't hang on a
//!   grandchild that holds the output pipe open.

use std::collections::BTreeMap;
use std::io::{PipeReader, PipeWriter, Read};
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gitbots_core::RunStatus;
use gitbots_core::ledger::MAX_LOG_BYTES;
use tokio::process::{Child, Command};
use tokio::sync::oneshot;

use super::{BoxFuture, JobContext, JobOutcome, Runner};

/// How long to keep reading output after a step's shell is gone.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Runs jobs on the local machine with `sh -c`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalRunner;

impl Runner for LocalRunner {
    fn name(&self) -> &str {
        "local"
    }

    fn run_job<'a>(&'a self, ctx: JobContext<'a>) -> BoxFuture<'a, anyhow::Result<JobOutcome>> {
        Box::pin(run_job(ctx))
    }
}

async fn run_job(ctx: JobContext<'_>) -> anyhow::Result<JobOutcome> {
    let started = Instant::now();
    let timeout = ctx.job.timeout();
    let deadline = tokio::time::Instant::now() + timeout;
    let mut out = JobOutcome {
        status: RunStatus::Success,
        exit_code: Some(0),
        failed_step: None,
        log: Vec::new(),
        duration_ms: 0,
    };

    for (i, step) in ctx.job.steps.iter().enumerate() {
        let label = step.label(i);
        if !out.status.is_success() {
            out.log.extend_from_slice(format!("== skipped step: {label}\n").as_bytes());
            continue;
        }
        out.log
            .extend_from_slice(format!("== step: {label}\n$ {}\n", step.run.trim_end()).as_bytes());

        let cwd = match &step.workdir {
            Some(dir) => ctx.workdir.join(dir),
            None => ctx.workdir.to_path_buf(),
        };
        let mut env = ctx.env.clone();
        env.extend(step.env.iter().map(|(k, v)| (k.clone(), v.clone())));

        let res = run_step(&step.run, &cwd, &env, deadline).await;
        out.log.extend_from_slice(&res.output);
        if !out.log.ends_with(b"\n") {
            out.log.push(b'\n');
        }
        if res.dropped > 0 {
            let note = format!(
                "== [gitbots: dropped {} bytes of output over the {MAX_LOG_BYTES}-byte limit]\n",
                res.dropped
            );
            out.log.extend_from_slice(note.as_bytes());
        }

        let ms = res.elapsed.as_millis();
        let (footer, code) = match res.end {
            StepEnd::Exited(0) => {
                out.log.extend_from_slice(format!("== exit 0 ({ms} ms)\n").as_bytes());
                continue;
            }
            StepEnd::TimedOut => {
                let msg = format!(
                    "== timed out: job exceeded its {} s timeout ({ms} ms in this step)\n",
                    timeout.as_secs()
                );
                out.log.extend_from_slice(msg.as_bytes());
                out.status = RunStatus::TimedOut;
                out.exit_code = None;
                out.failed_step = Some(label);
                continue;
            }
            StepEnd::Exited(code) => (format!("== exit {code} ({ms} ms)"), Some(code)),
            StepEnd::Signaled(Some(sig)) => (format!("== killed by signal {sig} ({ms} ms)"), None),
            StepEnd::Signaled(None) => (format!("== terminated abnormally ({ms} ms)"), None),
            StepEnd::Error(e) => (format!("== error: {e}"), None),
        };
        if step.continue_on_error {
            out.log.extend_from_slice(
                format!("{footer}, continuing (continue-on-error)\n").as_bytes(),
            );
        } else {
            out.log.extend_from_slice(format!("{footer}\n").as_bytes());
            out.status = RunStatus::Failure;
            out.exit_code = code;
            out.failed_step = Some(label);
        }
    }

    out.duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    Ok(out)
}

enum StepEnd {
    Exited(i32),
    Signaled(Option<i32>),
    TimedOut,
    /// The step could not be started or waited for.
    Error(String),
}

struct StepRun {
    end: StepEnd,
    output: Vec<u8>,
    dropped: usize,
    elapsed: Duration,
}

async fn run_step(
    script: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    deadline: tokio::time::Instant,
) -> StepRun {
    let started = Instant::now();
    let failed = |e: String| StepRun {
        end: StepEnd::Error(e),
        output: Vec::new(),
        dropped: 0,
        elapsed: started.elapsed(),
    };

    let (reader, writer) = match std::io::pipe() {
        Ok(pair) => pair,
        Err(e) => return failed(format!("could not create output pipe: {e}")),
    };
    let capture = match Capture::start(reader) {
        Ok(c) => c,
        Err(e) => return failed(format!("could not start log reader: {e}")),
    };
    let mut child = match spawn(script, cwd, env, writer) {
        Ok(child) => child,
        Err(e) => return failed(format!("could not start `sh` in {}: {e}", cwd.display())),
    };
    let pid = child.id();

    let end = match tokio::time::timeout_at(deadline, child.wait()).await {
        Ok(Ok(status)) => exit_end(status),
        Ok(Err(e)) => StepEnd::Error(format!("waiting for step: {e}")),
        Err(_) => StepEnd::TimedOut,
    };
    // Clean up the step's process group: background jobs after a normal exit,
    // everything after a timeout.
    kill_group(pid);
    if matches!(end, StepEnd::TimedOut) {
        let _ = child.kill().await;
    }
    let (output, dropped) = capture.finish(DRAIN_GRACE).await;
    StepRun { end, output, dropped, elapsed: started.elapsed() }
}

/// Spawns `sh -c script` writing stdout and stderr to `writer`. The parent's
/// copies of the write end are dropped on return, so the reader sees EOF once
/// the step's processes are gone.
fn spawn(
    script: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    writer: PipeWriter,
) -> std::io::Result<Child> {
    let stderr = writer.try_clone()?;
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(script)
        .current_dir(cwd)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(stderr)
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.spawn()
}

fn exit_end(status: ExitStatus) -> StepEnd {
    if let Some(code) = status.code() {
        return StepEnd::Exited(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        StepEnd::Signaled(status.signal())
    }
    #[cfg(not(unix))]
    StepEnd::Signaled(None)
}

#[cfg(unix)]
fn kill_group(pid: Option<u32>) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        // ESRCH (group already gone) is the common case after a clean exit.
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: Option<u32>) {}

/// Drains a pipe on a plain thread (blocking reads; a detached thread can't
/// hold up runtime shutdown the way `spawn_blocking` would).
struct Capture {
    buf: Arc<Mutex<Captured>>,
    done: oneshot::Receiver<()>,
}

#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    dropped: usize,
}

impl Capture {
    fn start(mut reader: PipeReader) -> std::io::Result<Self> {
        let buf = Arc::new(Mutex::new(Captured::default()));
        let (tx, done) = oneshot::channel();
        let sink = Arc::clone(&buf);
        std::thread::Builder::new().name("gitbots-step-log".into()).spawn(move || {
            let mut chunk = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut c = sink.lock().unwrap_or_else(|e| e.into_inner());
                        let room = MAX_LOG_BYTES.saturating_sub(c.bytes.len()).min(n);
                        c.bytes.extend_from_slice(&chunk[..room]);
                        c.dropped += n - room;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = tx.send(());
        })?;
        Ok(Self { buf, done })
    }

    /// Waits up to `grace` for EOF, then takes whatever was read.
    async fn finish(self, grace: Duration) -> (Vec<u8>, usize) {
        let _ = tokio::time::timeout(grace, self.done).await;
        let c = std::mem::take(&mut *self.buf.lock().unwrap_or_else(|e| e.into_inner()));
        (c.bytes, c.dropped)
    }
}

#[cfg(test)]
mod tests {
    use gitbots_core::id::{RunId, Ulid};

    use super::*;
    use crate::spec::{Job, Step};

    fn step(name: &str, run: &str) -> Step {
        Step {
            name: Some(name.into()),
            run: run.into(),
            env: BTreeMap::new(),
            workdir: None,
            continue_on_error: false,
        }
    }

    fn job(steps: Vec<Step>) -> Job {
        Job {
            runs_on: "local".into(),
            needs: vec![],
            env: BTreeMap::new(),
            timeout_secs: Some(30),
            steps,
        }
    }

    async fn run(job: &Job, dir: &Path, env: &[(&str, &str)]) -> (JobOutcome, String) {
        let run = RunId::from_ulid(Ulid::from_parts(1, 1));
        let ctx = JobContext {
            run: &run,
            workflow: "wf",
            name: "job",
            job,
            workdir: dir,
            env: env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
            source: None,
        };
        let out = LocalRunner.run_job(ctx).await.unwrap();
        let log = String::from_utf8_lossy(&out.log).into_owned();
        (out, log)
    }

    #[tokio::test]
    async fn success_logs_steps_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let j = job(vec![
            step("one", "echo out1; echo err1 >&2; echo out2"),
            step("two", "printf no-newline"),
        ]);
        let (out, log) = run(&j, dir.path(), &[]).await;
        assert_eq!(out.status, RunStatus::Success, "{log}");
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.failed_step, None);
        assert!(
            log.starts_with(
                "== step: one\n$ echo out1; echo err1 >&2; echo out2\nout1\nerr1\nout2\n== exit 0 ("
            ),
            "{log}"
        );
        assert!(
            log.contains("== step: two\n$ printf no-newline\nno-newline\n== exit 0 ("),
            "{log}"
        );
    }

    #[tokio::test]
    async fn failure_stops_later_steps() {
        let dir = tempfile::tempdir().unwrap();
        let j = job(vec![
            step("ok", "true"),
            step("bad", "echo boom; exit 3"),
            step("later", "touch ran"),
        ]);
        let (out, log) = run(&j, dir.path(), &[]).await;
        assert_eq!(out.status, RunStatus::Failure);
        assert_eq!(out.exit_code, Some(3));
        assert_eq!(out.failed_step.as_deref(), Some("bad"));
        assert!(log.contains("boom\n== exit 3 ("), "{log}");
        assert!(log.contains("== skipped step: later\n"), "{log}");
        assert!(!dir.path().join("ran").exists());
    }

    #[tokio::test]
    async fn continue_on_error_keeps_going() {
        let dir = tempfile::tempdir().unwrap();
        let mut flaky = step("flaky", "exit 1");
        flaky.continue_on_error = true;
        let j = job(vec![flaky, step("after", "touch ran")]);
        let (out, log) = run(&j, dir.path(), &[]).await;
        assert_eq!(out.status, RunStatus::Success, "{log}");
        assert_eq!(out.failed_step, None);
        assert!(log.contains("continue-on-error"), "{log}");
        assert!(dir.path().join("ran").exists());
    }

    #[tokio::test]
    async fn timeout_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        // The subshell's `sleep` keeps the output pipe open unless the whole
        // group is killed.
        let mut j = job(vec![
            step("slow", "echo start; (sleep 5; echo late); echo end"),
            step("next", "touch ran"),
        ]);
        j.timeout_secs = Some(1);
        let t = Instant::now();
        let (out, log) = run(&j, dir.path(), &[]).await;
        assert!(t.elapsed() < Duration::from_secs(4), "took {:?}", t.elapsed());
        assert_eq!(out.status, RunStatus::TimedOut, "{log}");
        assert_eq!(out.exit_code, None);
        assert_eq!(out.failed_step.as_deref(), Some("slow"));
        assert!(log.contains("\nstart\n== timed out") && !log.contains("\nlate\n"), "{log}");
        assert!(!dir.path().join("ran").exists());
    }

    #[tokio::test]
    async fn env_precedence_job_then_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = step("env", "echo \"A=$A B=$B\"; test -n \"$PATH\" && echo path-inherited");
        s.env.insert("B".into(), "step".into());
        let (out, log) = run(&job(vec![s]), dir.path(), &[("A", "job"), ("B", "job")]).await;
        assert_eq!(out.status, RunStatus::Success, "{log}");
        assert!(log.contains("A=job B=step\npath-inherited\n"), "{log}");
    }

    #[tokio::test]
    async fn step_workdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub/dir")).unwrap();
        let mut s = step("pwd", "pwd; touch here");
        s.workdir = Some("sub/dir".into());
        let mut missing = step("missing", "true");
        missing.workdir = Some("nope".into());
        let (out, log) = run(&job(vec![s, missing]), dir.path(), &[]).await;
        assert!(log.contains("/sub/dir\n"), "{log}");
        assert!(dir.path().join("sub/dir/here").exists());
        assert_eq!(out.status, RunStatus::Failure);
        assert_eq!(out.failed_step.as_deref(), Some("missing"));
        assert!(log.contains("== error: could not start `sh`"), "{log}");
    }
}

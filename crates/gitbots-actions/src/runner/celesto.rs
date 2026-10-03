//! `runs-on = "celesto"`: run a job on a hosted [Celesto](https://celesto.ai)
//! computer through its REST "Computers" API (there is no Rust SDK).
//!
//! One job = one computer: create it, wait until it runs, clone the source,
//! run each step as one `exec`, and always delete it (explicitly on every
//! path, plus a drop guard if the job future is cancelled).
//!
//! Configuration (env, read by [`CelestoConfig::from_env`]):
//! `CELESTO_API_KEY` (required), `CELESTO_API_URL` (default
//! `https://api.celesto.ai/v1`), `CELESTO_VCPUS` (2), `CELESTO_RAM_MB` (4096),
//! `CELESTO_DISK_MB` (10240, 512..=20480), `CELESTO_SIZE_ID`,
//! `CELESTO_TEMPLATE_ID`, and `GITBOTS_GIT_TOKEN` for private https clones.
//! Network policy is always `open` (the clone needs it).
//!
//! Verified from the API docs:
//! - `POST /computers` `{vcpus, ram_mb, disk_size_mb (512-20480), size_id?,
//!   template_id?, network_policy: {mode}}` returns an `id`;
//! - `POST /computers/{id}/exec` `{command (<= 10000 chars), timeout (1-300 s)}`
//!   returns `{exit_code, stdout, stderr, duration_ms, timed_out, command_id}`;
//! - `GET /computers/{id}`, `DELETE /computers/{id}`, bearer auth.
//!
//! Unverified (no account was available; nothing here has hit the real API):
//! - whether create blocks until the machine is ready. We poll `GET` until
//!   the status reads as running, parse the status leniently (`status`,
//!   `state` or `phase`; `running`/`ready`/`active`), give up after
//!   [`CelestoConfig::ready_timeout`], and go ahead if no status is reported;
//! - which `size_id`s exist (we send none unless `CELESTO_SIZE_ID` is set);
//! - whether the 300 s cap is per exec (assumed) or per computer;
//! - whether the default image has `git` and `sh`;
//! - private-repo auth: `GITBOTS_GIT_TOKEN` is injected as
//!   `https://x-access-token:<token>@host/...` (GitHub style). The token is
//!   redacted from the job log, but it does reach Celesto in the exec body;
//! - stdout and stderr come back separately, so the log cannot interleave them.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use gitbots_core::RunStatus;
use reqwest::{Client, Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{BoxFuture, JobContext, JobOutcome, Runner, Source};
use crate::spec::Step;

/// Production API base.
pub const DEFAULT_API_URL: &str = "https://api.celesto.ai/v1";
/// Longest `command` the exec endpoint accepts.
pub const MAX_COMMAND_CHARS: usize = 10_000;
/// Longest `timeout` the exec endpoint accepts.
pub const MAX_EXEC_SECS: u64 = 300;
/// Where the source is cloned on the computer.
pub const REPO_DIR: &str = "/work/repo";

const MASK: &str = "[REDACTED]";

/// Celesto runner settings. `Debug` hides the API key and git token.
#[derive(Clone)]
pub struct CelestoConfig {
    pub api_url: String,
    pub api_key: String,
    pub vcpus: u32,
    pub ram_mb: u32,
    pub disk_mb: u32,
    pub size_id: Option<String>,
    pub template_id: Option<String>,
    /// Injected into https clone URLs; redacted from logs.
    pub git_token: Option<String>,
    /// How long to wait for a new computer to report `running`.
    pub ready_timeout: Duration,
    pub poll_interval: Duration,
}

impl CelestoConfig {
    /// Defaults with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_url: DEFAULT_API_URL.to_owned(),
            api_key: api_key.into(),
            vcpus: 2,
            ram_mb: 4096,
            disk_mb: 10240,
            size_id: None,
            template_id: None,
            git_token: None,
            ready_timeout: Duration::from_secs(180),
            poll_interval: Duration::from_secs(2),
        }
    }

    /// Read the process environment; `Ok(None)` if `CELESTO_API_KEY` is unset.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Like [`Self::from_env`] with a custom variable lookup.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<Self>> {
        let get = |k: &str| lookup(k).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let Some(key) = get("CELESTO_API_KEY") else {
            return Ok(None);
        };
        let mut c = Self::new(key);
        if let Some(url) = get("CELESTO_API_URL") {
            c.api_url = url.trim_end_matches('/').to_owned();
        }
        c.vcpus = number(&get, "CELESTO_VCPUS", c.vcpus, 1..=256)?;
        c.ram_mb = number(&get, "CELESTO_RAM_MB", c.ram_mb, 128..=1_048_576)?;
        c.disk_mb = number(&get, "CELESTO_DISK_MB", c.disk_mb, 512..=20480)?;
        c.size_id = get("CELESTO_SIZE_ID");
        c.template_id = get("CELESTO_TEMPLATE_ID");
        c.git_token = get("GITBOTS_GIT_TOKEN");
        Ok(Some(c))
    }
}

fn number(
    get: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: u32,
    range: RangeInclusive<u32>,
) -> anyhow::Result<u32> {
    let Some(raw) = get(key) else {
        return Ok(default);
    };
    let n: u32 = raw.parse().with_context(|| format!("{key}=`{raw}` is not a whole number"))?;
    if !range.contains(&n) {
        bail!("{key}={n} is out of range ({}..={})", range.start(), range.end());
    }
    Ok(n)
}

impl fmt::Debug for CelestoConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CelestoConfig")
            .field("api_url", &self.api_url)
            .field("api_key", &MASK)
            .field("vcpus", &self.vcpus)
            .field("ram_mb", &self.ram_mb)
            .field("disk_mb", &self.disk_mb)
            .field("size_id", &self.size_id)
            .field("template_id", &self.template_id)
            .field("git_token", &self.git_token.as_ref().map(|_| MASK))
            .field("ready_timeout", &self.ready_timeout)
            .field("poll_interval", &self.poll_interval)
            .finish()
    }
}

/// Runs jobs on Celesto computers.
#[derive(Debug)]
pub struct CelestoRunner {
    config: CelestoConfig,
    api: Api,
}

impl CelestoRunner {
    pub fn new(config: CelestoConfig) -> anyhow::Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("gitbots/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the Celesto HTTP client")?;
        let api = Api {
            http,
            base: config.api_url.trim_end_matches('/').to_owned(),
            key: config.api_key.clone(),
        };
        Ok(Self { config, api })
    }

    /// `Ok(None)` if `CELESTO_API_KEY` is unset; `Err` if the config is invalid.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        CelestoConfig::from_env()?.map(Self::new).transpose()
    }

    pub fn config(&self) -> &CelestoConfig {
        &self.config
    }

    async fn run(&self, ctx: JobContext<'_>) -> anyhow::Result<JobOutcome> {
        let started = Instant::now();
        let Some(source) = ctx.source else {
            return Ok(JobOutcome::error(
                "celesto: this run has no source (clone URL + commit). Celesto computers have no file upload, \
                 so the runner clones the code; run against a commit that is pushed to a reachable remote.",
            ));
        };
        let timeout = ctx.job.timeout();
        let deadline = tokio::time::Instant::now() + timeout;
        let mut p = Progress::new(self.config.git_token.clone());
        let c = &self.config;
        p.line(format!(
            "== celesto: creating a computer ({} vCPU, {} MB RAM, {} MB disk)",
            c.vcpus, c.ram_mb, c.disk_mb
        ));

        let computer = match self.api.create(c).await {
            Ok(computer) => computer,
            Err(e) => {
                p.fail(RunStatus::Failure, format!("== error: could not create a computer: {e:#}"));
                return Ok(p.finish(started));
            }
        };
        p.line(format!("== celesto: computer {}", computer.id));
        let mut cleanup = Cleanup { api: self.api.clone(), id: Some(computer.id.clone()) };

        match tokio::time::timeout_at(
            deadline,
            self.drive(&computer, &ctx, source, deadline, &mut p),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => p.fail(RunStatus::Failure, format!("== error: {e:#}")),
            Err(_) => p.fail(
                RunStatus::TimedOut,
                format!("== timed out: job exceeded its {} s timeout", timeout.as_secs()),
            ),
        }
        match cleanup.delete().await {
            Ok(()) => p.line(format!("== celesto: deleted computer {}", computer.id)),
            Err(e) => p.line(format!(
                "== warning: could not delete computer {}: {e:#}. Delete it by hand to stop billing.",
                computer.id
            )),
        }
        Ok(p.finish(started))
    }

    async fn drive(
        &self,
        computer: &Computer,
        ctx: &JobContext<'_>,
        source: &Source,
        deadline: tokio::time::Instant,
        p: &mut Progress,
    ) -> anyhow::Result<()> {
        self.wait_ready(computer, p).await?;
        let shown = clone_command(&source.clone_url, &source.commit)?;
        let actual = clone_command(
            &with_token(&source.clone_url, self.config.git_token.as_deref()),
            &source.commit,
        )?;
        if !self.exec_step(&computer.id, "checkout", &shown, &actual, false, deadline, p).await? {
            return Ok(());
        }
        for (i, step) in ctx.job.steps.iter().enumerate() {
            let label = step.label(i);
            if !p.out.status.is_success() {
                p.line(format!("== skipped step: {label}"));
                continue;
            }
            match step_command(REPO_DIR, &ctx.env, step) {
                Ok(cmd) => {
                    self.exec_step(
                        &computer.id,
                        &label,
                        &step.run,
                        &cmd,
                        step.continue_on_error,
                        deadline,
                        p,
                    )
                    .await?;
                }
                Err(msg) => {
                    p.line(format!("== step: {label}"));
                    p.step_failed(&label, format!("== error: {msg}"), None, step.continue_on_error);
                }
            }
        }
        Ok(())
    }

    async fn wait_ready(&self, computer: &Computer, p: &mut Progress) -> anyhow::Result<()> {
        let limit = tokio::time::Instant::now() + self.config.ready_timeout;
        let mut status = computer.status.clone();
        let mut polled = false;
        loop {
            match &status {
                Some(ComputerStatus::Running) => return Ok(()),
                Some(ComputerStatus::Failed(s)) => bail!("computer {} is `{s}`", computer.id),
                None if polled => {
                    p.line("== celesto: no status reported; trying the computer anyway");
                    return Ok(());
                }
                _ => {}
            }
            if tokio::time::Instant::now() >= limit {
                let last =
                    status.as_ref().map_or_else(|| "unknown".to_owned(), ComputerStatus::to_string);
                bail!(
                    "computer {} is not running after {:?} (last status: {last})",
                    computer.id,
                    self.config.ready_timeout
                );
            }
            tokio::time::sleep(self.config.poll_interval).await;
            status = self.api.status(&computer.id).await?;
            polled = true;
        }
    }

    /// Run one command; returns whether the job is still succeeding.
    #[allow(clippy::too_many_arguments)]
    async fn exec_step(
        &self,
        id: &str,
        label: &str,
        shown: &str,
        command: &str,
        continue_on_error: bool,
        deadline: tokio::time::Instant,
        p: &mut Progress,
    ) -> anyhow::Result<bool> {
        p.push(&format!("== step: {label}\n$ {}\n", shown.trim_end()));
        p.current = Some(label.to_owned());
        let len = command.chars().count();
        if len > MAX_COMMAND_CHARS {
            let msg = format!(
                "== error: command is {len} chars, Celesto accepts at most {MAX_COMMAND_CHARS}; move the script into a file in the repo"
            );
            p.step_failed(label, msg, None, continue_on_error);
            return Ok(p.out.status.is_success());
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now()).as_secs();
        if remaining == 0 {
            p.fail(RunStatus::TimedOut, "== timed out: no time left for this step".to_owned());
            return Ok(false);
        }
        let secs = remaining.min(MAX_EXEC_SECS);
        let started = Instant::now();
        let r = self.api.exec(id, command, secs).await?;
        p.push(r.stdout.as_deref().unwrap_or_default());
        p.push(r.stderr.as_deref().unwrap_or_default());
        let ms = r
            .duration_ms
            .unwrap_or_else(|| started.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
        if r.timed_out {
            let why =
                if remaining > MAX_EXEC_SECS { "Celesto's per-exec limit" } else { "job timeout" };
            p.fail(RunStatus::TimedOut, format!("== timed out after {secs} s ({why})"));
            return Ok(false);
        }
        match r.exit_code {
            Some(0) => p.line(format!("== exit 0 ({ms} ms)")),
            Some(code) => p.step_failed(
                label,
                format!("== exit {code} ({ms} ms)"),
                Some(code),
                continue_on_error,
            ),
            None => p.step_failed(
                label,
                format!("== no exit code reported ({ms} ms)"),
                None,
                continue_on_error,
            ),
        }
        p.current = None;
        Ok(p.out.status.is_success())
    }
}

impl Runner for CelestoRunner {
    fn name(&self) -> &str {
        "celesto"
    }

    fn run_job<'a>(&'a self, ctx: JobContext<'a>) -> BoxFuture<'a, anyhow::Result<JobOutcome>> {
        Box::pin(self.run(ctx))
    }
}

/// Job state while it runs; the log redacts the git token on every write.
struct Progress {
    log: String,
    secret: Option<String>,
    /// Label of the step in flight, for `failed_step` on a timeout or error.
    current: Option<String>,
    out: JobOutcome,
}

impl Progress {
    fn new(secret: Option<String>) -> Self {
        let out = JobOutcome {
            status: RunStatus::Success,
            exit_code: Some(0),
            failed_step: None,
            log: Vec::new(),
            duration_ms: 0,
        };
        Self { log: String::new(), secret: secret.filter(|s| !s.is_empty()), current: None, out }
    }

    fn push(&mut self, text: &str) {
        match &self.secret {
            Some(secret) => self.log.push_str(&text.replace(secret.as_str(), MASK)),
            None => self.log.push_str(text),
        }
    }

    /// Push `text` on a line of its own.
    fn line(&mut self, text: impl AsRef<str>) {
        if !self.log.is_empty() && !self.log.ends_with('\n') {
            self.log.push('\n');
        }
        self.push(text.as_ref());
        self.log.push('\n');
    }

    fn step_failed(
        &mut self,
        label: &str,
        footer: String,
        code: Option<i32>,
        continue_on_error: bool,
    ) {
        if continue_on_error {
            self.line(format!("{footer}, continuing (continue-on-error)"));
        } else {
            self.line(footer);
            self.out.status = RunStatus::Failure;
            self.out.exit_code = code;
            self.out.failed_step = Some(label.to_owned());
        }
    }

    /// Fail the job at the step in flight (if any).
    fn fail(&mut self, status: RunStatus, message: String) {
        self.line(message);
        if self.out.status.is_success() {
            self.out.status = status;
            self.out.exit_code = None;
            self.out.failed_step = self.current.take();
        }
    }

    fn finish(mut self, started: Instant) -> JobOutcome {
        self.out.log = self.log.into_bytes();
        self.out.duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.out
    }
}

/// Deletes the computer: awaited by [`Cleanup::delete`] on every normal path,
/// and spawned from `Drop` if the job future is cancelled first.
struct Cleanup {
    api: Api,
    id: Option<String>,
}

impl Cleanup {
    async fn delete(&mut self) -> anyhow::Result<()> {
        let Some(id) = self.id.clone() else {
            return Ok(());
        };
        let mut last = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
            }
            match self.api.delete(&id).await {
                Ok(()) => {
                    self.id = None;
                    return Ok(());
                }
                Err(e) => last = Some(e),
            }
        }
        self.id = None;
        Err(last.expect("at least one attempt"))
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let (Some(id), Ok(handle)) = (self.id.take(), tokio::runtime::Handle::try_current()) {
            let api = self.api.clone();
            handle.spawn(async move {
                let _ = api.delete(&id).await;
            });
        }
    }
}

/// Thin client for the Computers API.
#[derive(Clone)]
struct Api {
    http: Client,
    base: String,
    key: String,
}

impl fmt::Debug for Api {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Api").field("base", &self.base).finish_non_exhaustive()
    }
}

impl Api {
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        timeout: Duration,
    ) -> anyhow::Result<(StatusCode, String)> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.base))
            .bearer_auth(&self.key)
            .timeout(timeout);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.with_context(|| format!("{method} {path}"))?;
        let status = resp.status();
        let text =
            resp.text().await.with_context(|| format!("{method} {path}: reading the response"))?;
        Ok((status, text))
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let (status, text) = self.send(method.clone(), path, body, timeout).await?;
        if !status.is_success() {
            bail!("{method} {path}: HTTP {status}: {}", snippet(&text));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text)
            .with_context(|| format!("{method} {path}: response is not JSON: {}", snippet(&text)))
    }

    async fn create(&self, config: &CelestoConfig) -> anyhow::Result<Computer> {
        let body = self
            .call(Method::POST, "/computers", Some(create_body(config)), Duration::from_secs(120))
            .await?;
        parse_computer(&body)
    }

    async fn status(&self, id: &str) -> anyhow::Result<Option<ComputerStatus>> {
        let body = self
            .call(Method::GET, &format!("/computers/{id}"), None, Duration::from_secs(30))
            .await?;
        Ok(parse_status(&body))
    }

    async fn exec(
        &self,
        id: &str,
        command: &str,
        timeout_secs: u64,
    ) -> anyhow::Result<ExecResponse> {
        let body = json!({ "command": command, "timeout": timeout_secs });
        // Leave the HTTP request some slack over the exec's own timeout.
        let http_timeout = Duration::from_secs(timeout_secs + 60);
        let resp = self
            .call(Method::POST, &format!("/computers/{id}/exec"), Some(body), http_timeout)
            .await?;
        parse_exec(resp)
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        let (status, text) = self
            .send(Method::DELETE, &format!("/computers/{id}"), None, Duration::from_secs(60))
            .await?;
        if status.is_success() || status == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            bail!("DELETE /computers/{id}: HTTP {status}: {}", snippet(&text))
        }
    }
}

fn snippet(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(500) {
        Some((i, _)) => format!("{}...", &text[..i]),
        None => text.to_owned(),
    }
}

fn create_body(config: &CelestoConfig) -> Value {
    let mut body = json!({
        "vcpus": config.vcpus,
        "ram_mb": config.ram_mb,
        "disk_size_mb": config.disk_mb,
        "network_policy": { "mode": "open" },
    });
    if let Some(size) = &config.size_id {
        body["size_id"] = json!(size);
    }
    if let Some(template) = &config.template_id {
        body["template_id"] = json!(template);
    }
    body
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Computer {
    id: String,
    status: Option<ComputerStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ComputerStatus {
    Running,
    Pending(String),
    Failed(String),
}

impl fmt::Display for ComputerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => f.write_str("running"),
            Self::Pending(s) | Self::Failed(s) => f.write_str(s),
        }
    }
}

/// The object itself, or one wrapped in `data` / `computer`.
fn unwrap_object(v: &Value) -> &Value {
    ["data", "computer"]
        .iter()
        .find_map(|k| v.get(k).filter(|inner| inner.is_object()))
        .unwrap_or(v)
}

fn parse_computer(body: &Value) -> anyhow::Result<Computer> {
    let v = unwrap_object(body);
    let id = match v.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => bail!("create response has no computer id: {}", snippet(&body.to_string())),
    };
    if id.is_empty()
        || !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        bail!("unexpected computer id `{}`", snippet(&id));
    }
    Ok(Computer { id, status: parse_status(v) })
}

fn parse_status(body: &Value) -> Option<ComputerStatus> {
    let v = unwrap_object(body);
    let field = ["status", "state", "phase"].iter().find_map(|k| v.get(k))?;
    let raw = match field {
        Value::String(s) => s.as_str(),
        Value::Object(_) => {
            ["state", "status", "phase"].iter().find_map(|k| field.get(k)?.as_str())?
        }
        _ => return None,
    };
    let s = raw.trim().to_ascii_lowercase();
    Some(match s.as_str() {
        "running" | "ready" | "active" | "started" | "up" => ComputerStatus::Running,
        "error" | "errored" | "failed" | "failure" | "stopped" | "stopping" | "terminated"
        | "terminating" | "deleted" | "deleting" | "destroyed" => ComputerStatus::Failed(s),
        _ => ComputerStatus::Pending(s),
    })
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct ExecResponse {
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    stdout: Option<String>,
    #[serde(default)]
    stderr: Option<String>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    timed_out: bool,
}

fn parse_exec(body: Value) -> anyhow::Result<ExecResponse> {
    let v = if body.get("exit_code").is_none() { unwrap_object(&body).clone() } else { body };
    serde_json::from_value(v).context("unexpected exec response")
}

/// Quote `s` as one POSIX shell word.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `https://host/...` -> `https://x-access-token:<token>@host/...`. Other URLs,
/// and URLs that already carry credentials, are returned unchanged.
fn with_token(url: &str, token: Option<&str>) -> String {
    match (url.strip_prefix("https://"), token) {
        (Some(rest), Some(token))
            if !token.is_empty() && !rest.split('/').next().unwrap_or_default().contains('@') =>
        {
            format!("https://x-access-token:{token}@{rest}")
        }
        _ => url.to_owned(),
    }
}

fn clone_command(url: &str, commit: &str) -> anyhow::Result<String> {
    if commit.is_empty() || commit.starts_with('-') || commit.chars().any(char::is_whitespace) {
        bail!("invalid commit `{commit}`");
    }
    Ok(format!(
        "git clone --filter=blob:none -- {url} {REPO_DIR} && cd {REPO_DIR} && git -c advice.detachedHead=false checkout {commit}",
        url = sh_quote(url),
        commit = sh_quote(commit),
    ))
}

/// `cd <root>/<workdir> && export K='v' ... && sh -c '<run>'`. The script
/// runs under its own `sh -c`, as with the local runner, so a multi-line
/// `run` can't escape the `&&` chain.
fn step_command(root: &str, env: &BTreeMap<String, String>, step: &Step) -> Result<String, String> {
    let dir =
        match step.workdir.as_deref().map(|d| d.trim_start_matches("./").trim_end_matches('/')) {
            Some(d) if !d.is_empty() && d != "." => format!("{root}/{d}"),
            _ => root.to_owned(),
        };
    let mut merged = env.clone();
    merged.extend(step.env.iter().map(|(k, v)| (k.clone(), v.clone())));
    let mut cmd = format!("cd {}", sh_quote(&dir));
    for (k, v) in &merged {
        let valid = k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(format!("env var name `{k}` is not a valid shell identifier"));
        }
        cmd.push_str(&format!(" && export {k}={}", sh_quote(v)));
    }
    cmd.push_str(" && sh -c ");
    cmd.push_str(&sh_quote(&step.run));
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use gitbots_core::id::{RunId, Ulid};

    use super::*;
    use crate::spec::Job;

    fn sh(script: &str) -> std::process::Output {
        std::process::Command::new("sh").arg("-c").arg(script).output().unwrap()
    }

    #[test]
    fn quoting_round_trips_through_sh() {
        assert_eq!(sh_quote(""), "''");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        for s in [
            "",
            "plain",
            "it's",
            "''",
            "a b\tc\nd",
            "$HOME `id` $(id) \\ \" ! * ? ~ ; & | < > # {}",
            "'; rm -rf / #",
        ] {
            let out = sh(&format!("printf %s {}", sh_quote(s)));
            assert_eq!(String::from_utf8(out.stdout).unwrap(), s);
        }
    }

    #[test]
    fn step_command_runs_in_workdir_with_env() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let root = dir.path().to_str().unwrap();
        let env: BTreeMap<String, String> =
            [("A", "job"), ("B", "job's $X")].map(|(k, v)| (k.into(), v.into())).into();
        let step = Step {
            name: None,
            run: "pwd\necho \"$A|$B\"\nexit 4".into(),
            env: [("A".to_owned(), "step".to_owned())].into(),
            workdir: Some("./sub/".into()),
            continue_on_error: false,
        };
        let cmd = step_command(root, &env, &step).unwrap();
        assert!(
            cmd.starts_with(&format!("cd '{root}/sub' && export A='step' && export B=")),
            "{cmd}"
        );
        let out = sh(&cmd);
        assert_eq!(out.status.code(), Some(4));
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(stdout.ends_with("/sub\nstep|job's $X\n"), "{stdout}");

        // A failing `cd` stops the whole command, multi-line script included.
        let missing = Step { workdir: Some("nope".into()), ..step.clone() };
        assert_ne!(sh(&step_command(root, &env, &missing).unwrap()).status.code(), Some(4));

        let bad: BTreeMap<String, String> = [("1BAD".to_owned(), "x".to_owned())].into();
        assert!(step_command(root, &bad, &step).unwrap_err().contains("1BAD"));
    }

    #[test]
    fn clone_command_and_token() {
        let url = "https://github.com/acme/app.git";
        assert_eq!(
            clone_command(url, "abc123").unwrap(),
            "git clone --filter=blob:none -- 'https://github.com/acme/app.git' /work/repo && cd /work/repo \
             && git -c advice.detachedHead=false checkout 'abc123'"
        );
        assert!(clone_command(url, "--upload-pack=x").is_err());
        assert!(clone_command(url, "").is_err());
        assert_eq!(
            with_token(url, Some("t0k")),
            "https://x-access-token:t0k@github.com/acme/app.git"
        );
        assert_eq!(with_token(url, None), url);
        assert_eq!(with_token(url, Some("")), url);
        assert_eq!(
            with_token("git@github.com:acme/app.git", Some("t0k")),
            "git@github.com:acme/app.git"
        );
        assert_eq!(with_token("https://u:p@host/x", Some("t0k")), "https://u:p@host/x");
    }

    #[test]
    fn parses_responses() {
        let c = parse_computer(
            &serde_json::from_str(r#"{"id":"cmp_1","status":"creating","vcpus":2}"#).unwrap(),
        )
        .unwrap();
        assert_eq!(
            c,
            Computer {
                id: "cmp_1".into(),
                status: Some(ComputerStatus::Pending("creating".into()))
            }
        );
        let c =
            parse_computer(&json!({"data": {"id": 42, "state": {"state": "RUNNING"}}})).unwrap();
        assert_eq!(c, Computer { id: "42".into(), status: Some(ComputerStatus::Running) });
        assert_eq!(parse_computer(&json!({"id": "x"})).unwrap().status, None);
        assert!(parse_computer(&json!({"name": "x"})).is_err());
        assert!(parse_computer(&json!({"id": "../etc"})).is_err());
        assert_eq!(parse_status(&json!({"status": "Ready"})), Some(ComputerStatus::Running));
        assert_eq!(
            parse_status(&json!({"status": "failed"})),
            Some(ComputerStatus::Failed("failed".into()))
        );
        assert_eq!(
            parse_status(&json!({"computer": {"phase": "booting"}})),
            Some(ComputerStatus::Pending("booting".into()))
        );

        let fixture = r#"{"exit_code":1,"stdout":"out\n","stderr":"err\n","duration_ms":12,"timed_out":false,"command_id":"c1","extra":true}"#;
        let r = parse_exec(serde_json::from_str(fixture).unwrap()).unwrap();
        assert_eq!(
            r,
            ExecResponse {
                exit_code: Some(1),
                stdout: Some("out\n".into()),
                stderr: Some("err\n".into()),
                duration_ms: Some(12),
                timed_out: false,
            }
        );
        let r = parse_exec(json!({"data": {"exit_code": null, "stdout": null, "timed_out": true}}))
            .unwrap();
        assert!(r.timed_out && r.exit_code.is_none() && r.stdout.is_none());

        let body = create_body(&CelestoConfig {
            template_id: Some("tpl".into()),
            ..CelestoConfig::new("k")
        });
        assert_eq!(
            body,
            json!({"vcpus": 2, "ram_mb": 4096, "disk_size_mb": 10240, "network_policy": {"mode": "open"}, "template_id": "tpl"})
        );
    }

    #[test]
    fn config_from_env_lookup() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(key, _)| *key == k).map(|(_, v)| (*v).to_owned())
        };
        assert!(CelestoConfig::from_lookup(env(&[])).unwrap().is_none());
        assert!(CelestoConfig::from_lookup(env(&[("CELESTO_API_KEY", " ")])).unwrap().is_none());
        let c = CelestoConfig::from_lookup(env(&[
            ("CELESTO_API_KEY", "key-123"),
            ("CELESTO_API_URL", "http://localhost:9/v1/"),
            ("CELESTO_VCPUS", "4"),
            ("GITBOTS_GIT_TOKEN", "ghs_secret"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(
            (c.api_url.as_str(), c.vcpus, c.ram_mb, c.disk_mb),
            ("http://localhost:9/v1", 4, 4096, 10240)
        );
        let debug = format!("{c:?}");
        assert!(!debug.contains("key-123") && !debug.contains("ghs_secret"), "{debug}");
        assert!(
            CelestoConfig::from_lookup(env(&[
                ("CELESTO_API_KEY", "k"),
                ("CELESTO_DISK_MB", "100")
            ]))
            .is_err()
        );
        assert!(
            CelestoConfig::from_lookup(env(&[
                ("CELESTO_API_KEY", "k"),
                ("CELESTO_RAM_MB", "lots")
            ]))
            .is_err()
        );
    }

    #[test]
    fn progress_redacts_the_token() {
        let mut p = Progress::new(Some("tok123".into()));
        p.push("fatal: https://x-access-token:tok123@github.com/x");
        p.line("done");
        assert_eq!(p.log, "fatal: https://x-access-token:[REDACTED]@github.com/x\ndone\n");
    }

    #[tokio::test]
    async fn missing_source_fails_without_calling_the_api() {
        let runner = CelestoRunner::new(CelestoConfig {
            api_url: "http://127.0.0.1:9".into(),
            ..CelestoConfig::new("k")
        })
        .unwrap();
        let job = Job {
            runs_on: "celesto".into(),
            needs: vec![],
            env: BTreeMap::new(),
            timeout_secs: None,
            steps: vec![],
        };
        let run = RunId::from_ulid(Ulid::from_parts(1, 1));
        let ctx = JobContext {
            run: &run,
            workflow: "w",
            name: "j",
            job: &job,
            workdir: std::path::Path::new("."),
            env: BTreeMap::new(),
            source: None,
        };
        let out = runner.run_job(ctx).await.unwrap();
        assert_eq!(out.status, RunStatus::Failure);
        assert!(String::from_utf8_lossy(&out.log).contains("no source"));
    }

    /// Creates a real (billed) computer. Run with
    /// `CELESTO_API_KEY=... cargo test -p gitbots-actions -- --ignored live`.
    #[tokio::test]
    #[ignore = "needs CELESTO_API_KEY; creates a real computer"]
    async fn live_hello_world() {
        let Some(runner) = CelestoRunner::from_env().unwrap() else {
            eprintln!("CELESTO_API_KEY is not set; skipping");
            return;
        };
        let step = Step {
            name: Some("look".into()),
            run: "git log -1 --oneline && ls".into(),
            ..step_default()
        };
        let job = Job {
            runs_on: "celesto".into(),
            needs: vec![],
            env: BTreeMap::new(),
            timeout_secs: Some(600),
            steps: vec![step],
        };
        let source = Source {
            clone_url: "https://github.com/octocat/Hello-World.git".into(),
            commit: "7fd1a60b01f91b314f59955a4e4d4e80d8edf11d".into(),
        };
        let run = RunId::from_ulid(Ulid::from_parts(1, 1));
        let ctx = JobContext {
            run: &run,
            workflow: "live",
            name: "look",
            job: &job,
            workdir: std::path::Path::new("."),
            env: [("CI".to_owned(), "true".to_owned())].into(),
            source: Some(&source),
        };
        let out = runner.run_job(ctx).await.unwrap();
        let log = String::from_utf8_lossy(&out.log);
        println!("{log}");
        assert_eq!(out.status, RunStatus::Success, "{log}");
        assert!(log.contains("README") && log.contains("deleted computer"), "{log}");
    }

    fn step_default() -> Step {
        Step {
            name: None,
            run: String::new(),
            env: BTreeMap::new(),
            workdir: None,
            continue_on_error: false,
        }
    }
}

//! `gitbots mcp`: gitbots's tools for AI clients, over MCP on stdio.
//!
//! Identity. The server never acts as a human: every mutating tool runs as an
//! agent session, recorded with `via: mcp`. The session of a connection is,
//! in order:
//!
//! 1. the `--session` flag passed to `gitbots mcp`,
//! 2. the `GITBOTS_SESSION` env var of the server process,
//! 3. a session started with the `session_start` tool on this connection,
//! 4. otherwise one auto-started on the first mutating call, seeded from the
//!    client's MCP `clientInfo` (model from `GITBOTS_MODEL`, else `unknown`).
//!
//! 1 and 2 pin the connection: `session_start` is refused. Every later
//! session on a connection becomes a child of the previous one, so an agent
//! can't start a fresh, unrelated session to approve its own work.
//!
//! Domain errors go back as tool-level errors (`isError: true`) so the agent
//! reads them; only malformed requests are protocol errors.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context, bail, ensure};
use rmcp::{
    Json, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use gitbots_core::event::{
    HandoffTarget, Report, ReportLevel, ReviewDecision, ToolCalled, handoff_label,
};
use gitbots_core::{AgentDescriptor, Session, Via};

use crate::project::{ActorCtx, CreateTask, EventFilter, ManifestSource, Project, StartSession};

const INSTRUCTIONS: &str = "\
gitbots records which agent did what in this git repo (sessions, tasks, attempts, commits) and \
enforces the project's mandate. How to work here:
1. Call `session_start` first, with your real `provider` and `model`. Otherwise gitbots starts a \
session for you from your client's name, with model \"unknown\".
2. Call `status` and `inbox` to see the tasks, the attempts and what awaits review.
3. To change code: `task_create` (or pick an existing task), then `attempt_start`. Do all the \
work inside the returned `workroom` directory: cd there, edit, and commit with plain git (hooks \
attribute the commits to your session). Then call `attempt_submit` with the attempt id.
4. Use `report` to tell your human about results, questions and blockers.
5. Never edit `.gitbots/`, `.github/`, `CLAUDE.md` or `AGENTS.md`: the mandate denies those paths \
and `attempt_submit` will refuse the attempt.
6. Humans make the decisions the mandate reserves for them (`whoami` shows which). When a tool \
says a decision needs a human, `report` it and move on; don't work around it.";

/// Where this connection's session came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Flag,
    Env,
    Tool,
    Auto,
}

impl Origin {
    fn label(self) -> &'static str {
        match self {
            Self::Flag => "--session flag",
            Self::Env => "GITBOTS_SESSION",
            Self::Tool => "session_start",
            Self::Auto => "auto-started from MCP clientInfo",
        }
    }
}

/// The agent session of one connection (stdio serves exactly one).
#[derive(Default)]
struct Conn {
    session: Option<Session>,
    origin: Option<Origin>,
    ended: bool,
}

impl Conn {
    fn pinned(&self) -> Option<Origin> {
        self.origin.filter(|o| matches!(o, Origin::Flag | Origin::Env))
    }

    /// The live session, `Ok(None)` when there is none yet.
    fn live(&self) -> anyhow::Result<Option<&Session>> {
        let Some(s) = &self.session else { return Ok(None) };
        if self.ended {
            match self.pinned() {
                Some(o) => bail!(
                    "session {} (from {}) has ended; restart the MCP server with a new session",
                    s.id,
                    o.label()
                ),
                None => {
                    bail!("session {} has ended; call `session_start` to begin a new one", s.id)
                }
            }
        }
        Ok(Some(s))
    }
}

fn lock(conn: &Mutex<Conn>) -> MutexGuard<'_, Conn> {
    conn.lock().unwrap_or_else(PoisonError::into_inner)
}

fn agent_ctx(session: &Session) -> ActorCtx {
    ActorCtx { actor: session.actor(), via: Via::Mcp }
}

/// `Claude Code` -> `claude-code`.
fn normalize_client(name: &str) -> String {
    name.trim().to_lowercase().replace(' ', "-")
}

fn infer_provider(client: &str) -> &'static str {
    if client.contains("claude") {
        "anthropic"
    } else if ["codex", "chatgpt", "openai"].iter().any(|n| client.contains(n)) {
        "openai"
    } else {
        "unknown"
    }
}

fn env_var(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// The agent behind an MCP client that never called `session_start`.
fn auto_agent(client: Option<&Implementation>) -> AgentDescriptor {
    let name = client.map_or_else(|| "unknown".to_owned(), |c| normalize_client(&c.name));
    let model = env_var("GITBOTS_MODEL").unwrap_or_else(|| "unknown".to_owned());
    let mut agent = AgentDescriptor::new(infer_provider(&name), model, name);
    agent.client_version = client.map(|c| c.version.clone()).filter(|v| !v.is_empty());
    agent
}

fn text(s: impl Into<String>) -> ContentBlock {
    ContentBlock::text(s.into())
}

fn fail(e: anyhow::Error) -> CallToolResult {
    CallToolResult::error(vec![text(format!("{e:#}"))])
}

/// Pretty JSON, then any guidance or notices as separate text blocks.
fn ok_json(value: &impl Serialize, notes: impl IntoIterator<Item = String>) -> ToolResult {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| fail(anyhow::Error::from(e).context("serializing the result")))?;
    let mut content = vec![text(json)];
    content.extend(notes.into_iter().map(text));
    Ok(CallToolResult::success(content))
}

fn enum_str(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(e) => format!("<{e}>"),
    }
}

fn some_str(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

type ToolResult = Result<CallToolResult, CallToolResult>;

// ---- parameters ---------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct SessionStartParams {
    /// Model vendor, e.g. `anthropic`, `openai`, `google`.
    provider: String,
    /// Your exact model id, e.g. `claude-opus-5-5`, `gpt-5-codex`.
    model: String,
    /// Harness driving you, e.g. `claude-code`, `codex`, `chatgpt`. Defaults to the MCP client name.
    client: Option<String>,
    /// Free-form role, e.g. `implementer`, `reviewer`, `planner`.
    role: Option<String>,
    /// Short label for this session, e.g. what you're working on.
    label: Option<String>,
    /// Parent session id, if a parent agent spawned you. Ignored after this connection already
    /// had a session: later sessions are always children of it.
    parent: Option<String>,
    /// Your client's own id for this chat or run.
    external_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SessionEndParams {
    /// What this session achieved, for the activity log.
    summary: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TaskCreateParams {
    /// One-line title, e.g. "Fix the login redirect".
    title: String,
    /// Details: context, acceptance criteria.
    body: Option<String>,
    /// Labels, e.g. ["bug"].
    labels: Option<Vec<String>>,
    /// Render the task from `.gitbots/recipes/<name>` on the trusted branch.
    recipe: Option<String>,
    /// Recipe inputs (needs `recipe`).
    inputs: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TaskParams {
    /// Task id or unique prefix of it.
    task: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AttemptStartParams {
    /// Task id or unique prefix of it.
    task: String,
    /// Branch to start from and later merge into. Defaults to the trusted branch.
    base: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AttemptSubmitParams {
    /// Attempt id (from `attempt_start` or `status`). Required over MCP.
    attempt: Option<String>,
    /// What changed and why, for the reviewer.
    summary: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AttemptParams {
    /// Attempt id, unique prefix, or its branch name.
    attempt: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct HandoffParams {
    /// Attempt id, unique prefix, or its branch name.
    attempt: String,
    /// Hand to this agent session (its workroom commits are then attributed to it).
    to_session: Option<String>,
    /// Hand to whoever picks up next with this role, e.g. `reviewer`.
    to_role: Option<String>,
    /// Hand to this human (handle, `@` optional).
    to_human: Option<String>,
    /// Context for the next holder.
    note: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum DecisionParam {
    Accept,
    Reject,
    #[serde(alias = "changes")]
    ChangesRequested,
}

impl From<DecisionParam> for ReviewDecision {
    fn from(d: DecisionParam) -> Self {
        match d {
            DecisionParam::Accept => Self::Accept,
            DecisionParam::Reject => Self::Reject,
            DecisionParam::ChangesRequested => Self::ChangesRequested,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReviewParams {
    /// Attempt id, unique prefix, or its branch name. Must be submitted.
    attempt: String,
    /// The decision.
    decision: DecisionParam,
    /// Why; required in spirit for reject and changes_requested.
    reason: Option<String>,
    /// After accepting, merge into the attempt's base branch.
    merge: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum LevelParam {
    #[default]
    Info,
    Warning,
    /// You are stuck until a human acts.
    Blocker,
}

impl From<LevelParam> for ReportLevel {
    fn from(l: LevelParam) -> Self {
        match l {
            LevelParam::Info => Self::Info,
            LevelParam::Warning => Self::Warning,
            LevelParam::Blocker => Self::Blocker,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReportParams {
    /// One-line headline, e.g. "Login fix ready for review".
    title: String,
    /// Details: results, links, what you need from the human.
    body: Option<String>,
    /// `info` (default), `warning`, or `blocker` when you can't continue without a human.
    level: Option<LevelParam>,
    /// Task this is about (id or prefix).
    task: Option<String>,
    /// Attempt this is about (id, prefix or branch).
    attempt: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TraceParams {
    /// Tool name, e.g. `bash`, `edit`, `web_search`.
    tool: String,
    /// Short summary of the input (secrets are redacted before storing).
    input: Option<String>,
    /// Whether the call succeeded.
    ok: bool,
    /// Wall time of the call.
    duration_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ActivityParams {
    /// Event kind or kind prefix, e.g. `attempt` or `review.decided`.
    kind: Option<String>,
    /// Only events by this session (id or prefix).
    session: Option<String>,
    /// Only events about this task (id or prefix).
    task: Option<String>,
    /// Only events about this attempt (id, prefix or branch).
    attempt: Option<String>,
    /// Newest N events (default 50).
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ActionsRunParams {
    /// Only this workflow (by name).
    workflow: Option<String>,
    /// Trigger to run, default `manual`.
    trigger: Option<String>,
    /// Run in this attempt's workroom instead of the main worktree.
    attempt: Option<String>,
}

// ---- structured results -------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
struct WhoAmI {
    /// The agent session this connection acts as; null until `session_start` or the first
    /// mutating call.
    session: Option<SessionInfo>,
    /// The MCP client as it identified itself (`name/version`).
    client: Option<String>,
    project: ProjectInfo,
    mandate: MandateInfo,
    /// Where the mandate was read from.
    manifest_source: String,
    /// Branch the mandate, workflows and recipes are read from.
    trusted_branch: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SessionInfo {
    id: String,
    /// `provider/model@client`.
    agent: String,
    role: Option<String>,
    parent: Option<String>,
    /// How this connection got the session.
    origin: String,
    ended: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ProjectInfo {
    id: String,
    name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MandateInfo {
    goal: Option<String>,
    /// `supervised`, `assisted` or `autonomous`.
    autonomy: String,
    /// Who may make each decision: `any` (agents too) or the minimum human role.
    approvals: BTreeMap<String, String>,
    /// Paths agents may change.
    allowed_paths: Vec<String>,
    /// Paths agents must never change.
    denied_paths: Vec<String>,
    /// Branches only humans may merge into (unless `merge_protected` is `any`).
    protected_branches: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct StatusInfo {
    project: ProjectInfo,
    goal: Option<String>,
    tasks: Vec<TaskSummary>,
    /// Number of submitted attempts waiting for a review decision.
    awaiting_review: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
struct TaskSummary {
    id: String,
    short: String,
    title: String,
    /// `open`, `in_progress`, `accepted` or `done`.
    status: String,
    labels: Vec<String>,
    attempts: Vec<AttemptSummary>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct AttemptSummary {
    id: String,
    short: String,
    /// `active`, `submitted`, `changes_requested`, `accepted`, `rejected`, `merged`, `abandoned`.
    state: String,
    branch: String,
    /// Session the workroom is bound to.
    session: Option<String>,
    /// Latest run of every workflow passed; null when none ran.
    checks_passed: Option<bool>,
    diff: Option<Diff>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct Diff {
    files: u32,
    insertions: u32,
    deletions: u32,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SessionEnded {
    session: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct TaskCreated {
    task: String,
    short: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    notice: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct HandedOff {
    attempt: String,
    to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    notice: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct Reviewed {
    attempt: String,
    /// `accept`, `reject` or `changes_requested`.
    decision: String,
    /// Merge commit, when merged.
    merged: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notice: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct Recorded {
    /// Ledger event id.
    event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    notice: Option<String>,
}

// ---- server -------------------------------------------------------------

#[derive(Clone)]
struct GitbotsServer {
    project: Arc<Project>,
    conn: Arc<Mutex<Conn>>,
    tool_router: ToolRouter<Self>,
}

impl GitbotsServer {
    /// Run blocking git work off the async workers.
    async fn blocking<T, F>(&self, f: F) -> Result<T, CallToolResult>
    where
        T: Send + 'static,
        F: FnOnce(&Project) -> anyhow::Result<T> + Send + 'static,
    {
        let project = self.project.clone();
        match tokio::task::spawn_blocking(move || f(&project)).await {
            Ok(result) => result.map_err(fail),
            Err(e) => Err(fail(anyhow::anyhow!("internal error: {e}"))),
        }
    }

    /// The agent this connection acts as. Auto-starts a session from the
    /// client's MCP identity when there is none yet; the returned notice
    /// then asks the agent to call `session_start` itself next time.
    async fn agent(
        &self,
        ctx: &RequestContext<RoleServer>,
    ) -> Result<(ActorCtx, Option<String>), CallToolResult> {
        let live = lock(&self.conn).live().map(|s| s.cloned()).map_err(fail)?;
        if let Some(s) = live {
            return Ok((agent_ctx(&s), None));
        }
        let agent = auto_agent(ctx.client_info().as_ref());
        let conn = self.conn.clone();
        let (session, created) = self
            .blocking(move |project| {
                // Held while starting, so concurrent calls start one session.
                let mut conn = lock(&conn);
                if let Some(s) = conn.live()? {
                    return Ok((s.clone(), false));
                }
                let session = project.start_session(StartSession {
                    agent: Some(agent),
                    via: Some(Via::Mcp),
                    ..StartSession::default()
                })?;
                conn.session = Some(session.clone());
                conn.origin = Some(Origin::Auto);
                Ok((session, true))
            })
            .await?;
        let notice = created.then(|| {
            eprintln!("gitbots mcp: auto-started session {} as {}", session.id, session.agent.key());
            format!(
                "gitbots started session {} for you as `{}`, guessed from your MCP client. Next time, \
                 call `session_start` first with your real provider and model so your work is \
                 attributed correctly.",
                session.id,
                session.agent.key()
            )
        });
        Ok((agent_ctx(&session), notice))
    }

    /// Resolve an attempt query to its id.
    async fn attempt_id(&self, query: String) -> Result<gitbots_core::AttemptId, CallToolResult> {
        self.blocking(move |p| Ok(p.board()?.find_attempt(&query)?.id.clone())).await
    }
}

#[tool_router]
impl GitbotsServer {
    fn new(project: Arc<Project>, conn: Conn) -> Self {
        Self { project, conn: Arc::new(Mutex::new(conn)), tool_router: Self::tool_router() }
    }

    /// Who you are to gitbots: your session (if any), the project, and the mandate (goal, autonomy,
    /// which decisions need a human, paths you may not touch, protected branches). Call it when
    /// unsure what you're allowed to do.
    #[tool(annotations(read_only_hint = true))]
    async fn whoami(&self, ctx: RequestContext<RoleServer>) -> Json<WhoAmI> {
        let session = {
            let conn = lock(&self.conn);
            conn.session.as_ref().map(|s| SessionInfo {
                id: s.id.to_string(),
                agent: s.agent.key(),
                role: s.role.clone(),
                parent: s.parent.as_ref().map(ToString::to_string),
                origin: conn.origin.map_or("", Origin::label).to_owned(),
                ended: conn.ended,
            })
        };
        let p = &self.project;
        let m = p.manifest();
        let rules = &m.mandate.agents;
        let manifest_source = match p.manifest_source() {
            ManifestSource::Trusted { branch, oid } => format!("{branch} (blob {oid})"),
            ManifestSource::WorkingTree => {
                "working tree (not committed to the trusted branch yet)".to_owned()
            }
        };
        let approvals = match serde_json::to_value(&m.mandate.approvals) {
            Ok(Value::Object(map)) => map.into_iter().map(|(k, v)| (k, enum_str(&v))).collect(),
            _ => BTreeMap::new(),
        };
        Json(WhoAmI {
            session,
            client: ctx.client_info().map(|c| format!("{}/{}", c.name, c.version)),
            project: ProjectInfo { id: m.project.id.to_string(), name: m.project.name.clone() },
            mandate: MandateInfo {
                goal: m.mandate.goal.clone(),
                autonomy: enum_str(&m.mandate.autonomy),
                approvals,
                allowed_paths: rules.allowed_paths.clone(),
                denied_paths: rules.denied_paths.clone(),
                protected_branches: rules.protected_branches.clone(),
            },
            manifest_source,
            trusted_branch: p.trusted_branch().to_owned(),
        })
    }

    /// Start your agent session. Call this FIRST, once per chat or run, with your real provider
    /// and model: everything you do is attributed to it, and gitbots benchmarks agents by it. A
    /// subagent passes its parent's session id as `parent`.
    #[tool]
    async fn session_start(
        &self,
        Parameters(p): Parameters<SessionStartParams>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let (provider, model) = (p.provider.trim().to_owned(), p.model.trim().to_owned());
        if provider.is_empty() || model.is_empty() {
            return Err(fail(anyhow::anyhow!("`provider` and `model` must not be empty")));
        }
        let info = ctx.client_info();
        let mut agent = match some_str(p.client) {
            Some(client) => AgentDescriptor::new(provider, model, client),
            None => {
                let client = info.as_ref().map(|i| normalize_client(&i.name));
                let client = client.or_else(|| env_var("GITBOTS_CLIENT"));
                let client = client.unwrap_or_else(|| "unknown".to_owned());
                let mut a = AgentDescriptor::new(provider, model, client);
                a.client_version = info.map(|i| i.version).filter(|v| !v.is_empty());
                a
            }
        };
        agent.client = normalize_client(&agent.client);
        let conn = self.conn.clone();
        let (role, label, external_id) = (p.role, p.label, p.external_id);
        let explicit_parent = some_str(p.parent);
        let session = self
            .blocking(move |project| {
                let mut conn = lock(&conn);
                if let (Some(origin), Some(s)) = (conn.pinned(), &conn.session) {
                    bail!(
                        "this connection is pinned to session {} by the {}; keep using it",
                        s.id,
                        origin.label()
                    );
                }
                // One connection is one session family: later sessions are
                // children of the previous one.
                let parent = match &conn.session {
                    Some(prev) => Some(prev.id.to_string()),
                    None => explicit_parent,
                };
                let session = project.start_session(StartSession {
                    agent: Some(agent),
                    parent,
                    role,
                    label,
                    external_id,
                    bind: false,
                    via: Some(Via::Mcp),
                })?;
                *conn = Conn {
                    session: Some(session.clone()),
                    origin: Some(Origin::Tool),
                    ended: false,
                };
                Ok(session)
            })
            .await?;
        eprintln!("gitbots mcp: session {} as {}", session.id, session.agent.key());
        ok_json(
            &session,
            [format!(
                "You are session {} ({}). This connection acts as it from now on.",
                session.id,
                session.agent.key()
            )],
        )
    }

    /// End your session when your chat or run is done, with a short summary of what you did.
    #[tool]
    async fn session_end(
        &self,
        Parameters(p): Parameters<SessionEndParams>,
    ) -> Result<Json<SessionEnded>, CallToolResult> {
        let conn = self.conn.clone();
        let id = self
            .blocking(move |project| {
                let mut conn = lock(&conn);
                let session = conn.live()?.cloned().context("no session on this connection")?;
                let id = project.end_session(&agent_ctx(&session), some_str(p.summary))?;
                conn.ended = true;
                Ok(id)
            })
            .await?;
        Ok(Json(SessionEnded { session: id.to_string() }))
    }

    /// The board: every task with its status and attempts (state, branch, checks, diff), and how
    /// many attempts await review. Start here to see what exists before creating work.
    #[tool(annotations(read_only_hint = true))]
    async fn status(&self) -> Result<Json<StatusInfo>, CallToolResult> {
        self.blocking(|p| {
            let board = p.board()?;
            let m = p.manifest();
            let tasks = board
                .tasks
                .values()
                .map(|t| TaskSummary {
                    id: t.id.to_string(),
                    short: t.id.short(),
                    title: t.title.clone(),
                    status: enum_str(&board.task_status(t)),
                    labels: t.labels.clone(),
                    attempts: t
                        .attempts
                        .iter()
                        .filter_map(|id| board.attempts.get(id))
                        .map(|a| AttemptSummary {
                            id: a.id.to_string(),
                            short: a.id.short(),
                            state: a.state.as_str().to_owned(),
                            branch: a.branch.clone(),
                            session: a.session.as_ref().map(ToString::to_string),
                            checks_passed: a.checks_passed(),
                            diff: a.diff.map(|d| Diff {
                                files: d.files,
                                insertions: d.insertions,
                                deletions: d.deletions,
                            }),
                        })
                        .collect(),
                })
                .collect();
            Ok(Json(StatusInfo {
                project: ProjectInfo { id: m.project.id.to_string(), name: m.project.name.clone() },
                goal: m.mandate.goal.clone(),
                tasks,
                awaiting_review: board.awaiting_review().count(),
            }))
        })
        .await
    }

    /// Create a task: a unit of work you or another agent will attempt. Check `status` first so
    /// you don't duplicate an existing task.
    #[tool]
    async fn task_create(
        &self,
        Parameters(p): Parameters<TaskCreateParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<TaskCreated>, CallToolResult> {
        let (actor, notice) = self.agent(&ctx).await?;
        let title = p.title.clone();
        let req = CreateTask {
            title: p.title,
            body: p.body,
            labels: p.labels.unwrap_or_default(),
            recipe: some_str(p.recipe),
            inputs: p.inputs.unwrap_or_default(),
        };
        let id = self.blocking(move |project| project.create_task(&actor, req)).await?;
        Ok(Json(TaskCreated { task: id.to_string(), short: id.short(), title, notice }))
    }

    /// One task in full: body, status and all its attempts.
    #[tool(annotations(read_only_hint = true))]
    async fn task_show(&self, Parameters(p): Parameters<TaskParams>) -> ToolResult {
        let value = self
            .blocking(move |project| {
                let board = project.board()?;
                let t = board.find_task(&p.task)?;
                let attempts: Vec<_> =
                    t.attempts.iter().filter_map(|id| board.attempts.get(id)).collect();
                Ok(json!({ "task": t, "status": board.task_status(t), "attempts": attempts }))
            })
            .await?;
        ok_json(&value, [])
    }

    /// Start an attempt at a task: gitbots creates a branch and a private workroom (a git worktree)
    /// bound to your session. Do all the work for the task inside that workroom, then call
    /// `attempt_submit`.
    #[tool]
    async fn attempt_start(
        &self,
        Parameters(p): Parameters<AttemptStartParams>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let (actor, notice) = self.agent(&ctx).await?;
        let base = some_str(p.base);
        let info = self
            .blocking(move |project| project.start_attempt(&actor, &p.task, base.as_deref(), None))
            .await?;
        let wr = info.workroom.display();
        let guide = format!(
            "Next: do ALL the work for this attempt inside the workroom `{wr}` (branch `{}`). cd \
             there for every shell command (`cd {wr} && ...`), edit files there, and commit with \
             plain `git add` / `git commit`: gitbots's hooks attribute those commits to your session. \
             Don't touch the main checkout, `.gitbots/`, `.github/`, `CLAUDE.md` or `AGENTS.md`. When \
             the work is committed, call `attempt_submit` with attempt `{}`.",
            info.branch, info.attempt
        );
        ok_json(&info, std::iter::once(guide).chain(notice))
    }

    /// Submit your attempt for review once its commits are in the workroom. Runs the project's
    /// `attempt.submitted` checks and refuses changes to paths the mandate denies.
    #[tool]
    async fn attempt_submit(
        &self,
        Parameters(p): Parameters<AttemptSubmitParams>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let Some(attempt) = some_str(p.attempt) else {
            return Err(fail(anyhow::anyhow!(
                "pass `attempt` (the id from `attempt_start` or `status`): this MCP server doesn't \
                 run inside your workroom, so it can't tell which attempt you mean"
            )));
        };
        let (actor, notice) = self.agent(&ctx).await?;
        let outcome = self
            .project
            .submit_attempt(&actor, Some(&attempt), some_str(p.summary))
            .await
            .map_err(fail)?;
        let next = "Submitted. Someone outside your session family (another agent or a human, as \
                    the mandate says) reviews it next; see `inbox`."
            .to_owned();
        ok_json(&outcome, std::iter::once(next).chain(notice))
    }

    /// One attempt in full: state, review, checks, its workroom path, and any mandate violations
    /// (paths it changes that agents may not).
    #[tool(annotations(read_only_hint = true))]
    async fn attempt_show(&self, Parameters(p): Parameters<AttemptParams>) -> ToolResult {
        let value = self
            .blocking(move |project| {
                let board = project.board()?;
                let a = board.find_attempt(&p.attempt)?;
                let violations: Vec<Value> = project
                    .violations(a)?
                    .into_iter()
                    .map(|v| json!({ "path": v.path, "reason": v.reason }))
                    .collect();
                Ok(json!({
                    "attempt": a,
                    "task": board.tasks.get(&a.task).map(|t| &t.title),
                    "workroom": project.workroom_of(a)?,
                    "violations": violations,
                }))
            })
            .await?;
        ok_json(&value, [])
    }

    /// Pass an open attempt to another agent session, a role, or a human. Give exactly one
    /// target, and a note with what the next holder needs to know.
    #[tool]
    async fn handoff(
        &self,
        Parameters(p): Parameters<HandoffParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<HandedOff>, CallToolResult> {
        let (to_session, to_role, to_human) =
            (some_str(p.to_session), some_str(p.to_role), some_str(p.to_human));
        let targets = [&to_session, &to_role, &to_human].iter().filter(|t| t.is_some()).count();
        if targets != 1 {
            return Err(fail(anyhow::anyhow!(
                "pass exactly one of `to_session`, `to_role`, `to_human`"
            )));
        }
        let (actor, notice) = self.agent(&ctx).await?;
        let attempt = p.attempt;
        let (attempt, to) = self
            .blocking(move |project| {
                let to = match (to_session, to_role, to_human) {
                    (Some(s), _, _) => HandoffTarget::Session { session: project.session(&s)?.id },
                    (_, Some(role), _) => HandoffTarget::Role { role },
                    (_, _, Some(h)) => {
                        HandoffTarget::Human { handle: h.trim_start_matches('@').to_owned() }
                    }
                    _ => bail!("no handoff target"),
                };
                let label = handoff_label(&to);
                let id = project.board()?.find_attempt(&attempt)?.id.clone();
                project.handoff(&actor, id.as_str(), to, p.note)?;
                Ok((id, label))
            })
            .await?;
        Ok(Json(HandedOff { attempt: attempt.to_string(), to, notice }))
    }

    /// Decide on a submitted attempt: accept, reject, or changes_requested (give a reason). You
    /// can't review work from your own session family, and the mandate may reserve the decision
    /// (or merging into a protected branch) for a human; then this fails and the attempt waits
    /// in the human's inbox.
    #[tool]
    async fn review(
        &self,
        Parameters(p): Parameters<ReviewParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Reviewed>, CallToolResult> {
        let (actor, notice) = self.agent(&ctx).await?;
        let decision = ReviewDecision::from(p.decision);
        let (attempt, reason, merge) = (p.attempt, some_str(p.reason), p.merge.unwrap_or(false));
        let outcome = self
            .blocking(move |project| project.review(&actor, &attempt, decision, reason, merge))
            .await?;
        Ok(Json(Reviewed {
            attempt: outcome.attempt.to_string(),
            decision: enum_str(&outcome.decision),
            merged: outcome.merged,
            notice,
        }))
    }

    /// Tell your human something: results, a question, or a blocker. It lands in their inbox.
    /// Use it whenever a human should know or decide something.
    #[tool]
    async fn report(
        &self,
        Parameters(p): Parameters<ReportParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Recorded>, CallToolResult> {
        let (actor, notice) = self.agent(&ctx).await?;
        let event = self
            .blocking(move |project| {
                let board = project.board()?;
                let task = match some_str(p.task) {
                    Some(q) => Some(board.find_task(&q)?.id.clone()),
                    None => None,
                };
                let attempt = match some_str(p.attempt) {
                    Some(q) => Some(board.find_attempt(&q)?.id.clone()),
                    None => None,
                };
                let level = p.level.unwrap_or_default().into();
                project
                    .report(&actor, Report { title: p.title, body: p.body, level, task, attempt })
            })
            .await?;
        Ok(Json(Recorded { event: event.to_string(), notice }))
    }

    /// Record one of your own tool calls (shell command, edit, search) in the activity ledger,
    /// for observability. Secrets in `input` are redacted.
    #[tool]
    async fn trace(
        &self,
        Parameters(p): Parameters<TraceParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Recorded>, CallToolResult> {
        let (actor, notice) = self.agent(&ctx).await?;
        let input = p.input.map(|i| gitbots_core::redact::redact(&i).0.into_owned());
        let call =
            ToolCalled { tool: p.tool, input, ok: p.ok, duration_ms: p.duration_ms, log: None };
        let event = self.blocking(move |project| project.trace(&actor, call)).await?;
        Ok(Json(Recorded { event: event.to_string(), notice }))
    }

    /// Activity ledger events (oldest first), optionally filtered: who did what, when. Use it to
    /// catch up on a task or attempt.
    #[tool(annotations(read_only_hint = true))]
    async fn activity(&self, Parameters(p): Parameters<ActivityParams>) -> ToolResult {
        let filter = EventFilter {
            kind: some_str(p.kind),
            session: some_str(p.session),
            task: some_str(p.task),
            attempt: some_str(p.attempt),
            limit: Some(p.limit.unwrap_or(50)),
        };
        let events = self.blocking(move |project| project.query_events(&filter)).await?;
        ok_json(&events, [])
    }

    /// What waits for a decision: submitted attempts awaiting review, and recent reports.
    #[tool(annotations(read_only_hint = true))]
    async fn inbox(&self) -> ToolResult {
        let inbox = self.blocking(|p| p.inbox()).await?;
        ok_json(&inbox, [])
    }

    /// Per-agent numbers: sessions, attempts, acceptance rate, check pass rate, commits, lines.
    #[tool(annotations(read_only_hint = true))]
    async fn stats(&self) -> ToolResult {
        let stats = self.blocking(|p| p.stats()).await?;
        ok_json(&stats, [])
    }

    /// Workflows in `.gitbots/actions` on the trusted branch, with their triggers.
    #[tool(annotations(read_only_hint = true))]
    async fn actions_list(&self) -> ToolResult {
        let (ok, bad) = self.blocking(|p| p.workflows()).await?;
        let workflows: Vec<Value> =
            ok.iter().map(|(path, wf)| json!({ "path": path, "workflow": wf })).collect();
        let invalid: Vec<Value> =
            bad.iter().map(|(path, err)| json!({ "path": path, "error": err })).collect();
        ok_json(&json!({ "workflows": workflows, "invalid": invalid }), [])
    }

    /// Run workflows now (default trigger `manual`), in an attempt's workroom or the main
    /// worktree. Manual runs on hosted runners may need a human per the mandate.
    #[tool]
    async fn actions_run(
        &self,
        Parameters(p): Parameters<ActionsRunParams>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let (actor, notice) = self.agent(&ctx).await?;
        let attempt = match some_str(p.attempt) {
            Some(q) => Some(self.attempt_id(q).await?),
            None => None,
        };
        let trigger = some_str(p.trigger).unwrap_or_else(|| "manual".to_owned());
        let workflow = some_str(p.workflow);
        let outcome = self
            .project
            .run_actions(&actor, &trigger, attempt.as_ref(), workflow.as_deref())
            .await
            .map_err(fail)?;
        ok_json(&outcome, notice)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GitbotsServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("gitbots", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// The session `gitbots mcp` was started with, if any (`--session`, then `GITBOTS_SESSION`).
fn pinned_session(project: &Project, flag: Option<String>) -> anyhow::Result<Conn> {
    let (query, origin) = match (some_str(flag), env_var("GITBOTS_SESSION")) {
        (Some(q), _) => (q, Origin::Flag),
        (None, Some(q)) => (q, Origin::Env),
        (None, None) => return Ok(Conn::default()),
    };
    let session = project
        .session(&query)
        .with_context(|| format!("resolving the session from the {}", origin.label()))?;
    let ended = project.board()?.sessions.get(&session.id).is_some_and(|v| v.ended);
    ensure!(!ended, "session {} (from the {}) has ended", session.id, origin.label());
    Ok(Conn { session: Some(session), origin: Some(origin), ended: false })
}

/// Serve gitbots over MCP on stdio until the client closes the connection.
pub async fn serve(project: Project, session: Option<String>) -> anyhow::Result<()> {
    let conn = pinned_session(&project, session)?;
    let m = project.manifest();
    match &conn.session {
        Some(s) => eprintln!(
            "gitbots mcp: serving {} ({}) as session {} ({})",
            m.project.name,
            m.project.id,
            s.id,
            s.agent.key()
        ),
        None => eprintln!(
            "gitbots mcp: serving {} ({}); no session yet (session_start, or auto-start on first write)",
            m.project.name, m.project.id
        ),
    }
    let server = GitbotsServer::new(Arc::new(project), conn);
    let service = server.serve(stdio()).await.context("starting the MCP server")?;
    let reason = service.waiting().await.context("serving MCP")?;
    eprintln!("gitbots mcp: stopped ({reason:?})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_identity() {
        assert_eq!(normalize_client("Claude Code"), "claude-code");
        assert_eq!(infer_provider("claude-code"), "anthropic");
        assert_eq!(infer_provider("codex-mcp-client"), "openai");
        assert_eq!(infer_provider("chatgpt"), "openai");
        assert_eq!(infer_provider("cursor"), "unknown");
    }
}

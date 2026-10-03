//! `Project`: the one API the CLI and the MCP server share.
//!
//! It wires the pure domain (`gitbots-core`) to git (`gitbots-git`) and to the
//! actions engine (`gitbots-actions`). Every mutation is recorded as a ledger
//! event attributed to a resolved [`ActorCtx`].

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail, ensure};
use time::OffsetDateTime;

use gitbots_actions::{RunRequest, RunnerRegistry, Source};
use gitbots_core::board::{ReportView, SessionView};
use gitbots_core::event::*;
use gitbots_core::ledger::{self, LedgerKind, LedgerMeta};
use gitbots_core::manifest::{
    Autonomy, GITBOTS_DIR, MANIFEST_PATH, Mandate, Manifest, Owner, OwnerKind, PathViolation,
    Principal, ProjectMeta, Role,
};
use gitbots_core::{
    ActionRun, Actor, AgentDescriptor, AttemptId, AttemptState, AttemptView, Authorization, Board,
    Decision, Event, EventBody, EventId, LogRef, ProjectId, Recipe, RunId, Session, SessionId,
    Stats, TaskId, Ulid, Via, kind,
};
use gitbots_git::{Activity, HookReport, Ledger, Logs, Repo};

pub const PRODUCER: &str = concat!("gitbots/", env!("CARGO_PKG_VERSION"));
const RECIPES_DIR: &str = ".gitbots/recipes/";
const ACTIONS_DIR: &str = ".gitbots/actions/";
/// Per-worktree binding of the session auto-started for an agent harness.
const AUTO_SESSION_BINDING: &str = "auto-session";
/// Never follow a ledger clock more than this far ahead of ours.
const MAX_SKEW_MS: u64 = 5 * 60 * 1000;

/// A resolved actor plus how it was resolved.
#[derive(Clone, Debug)]
pub struct ActorCtx {
    pub actor: Actor,
    pub via: Via,
}

impl ActorCtx {
    pub fn system(component: &str) -> Self {
        Self { actor: Actor::system(component), via: Via::System }
    }
}

/// The mandate refused a decision, or wants a human for it. A refusal
/// stays a refusal on retry; callers tell it apart with `downcast_ref`.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// The idempotency key of a keyed write is already in the ledger.
#[derive(Debug)]
pub struct AlreadyRecorded {
    pub key: String,
}

impl std::fmt::Display for AlreadyRecorded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "already recorded (idempotency key `{}`)", self.key)
    }
}

impl std::error::Error for AlreadyRecorded {}

/// Context of a merge error after the review itself was recorded as `event`.
#[derive(Debug)]
pub struct NotMerged {
    pub event: EventId,
}

impl std::fmt::Display for NotMerged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the review is recorded ({}), but the merge failed", self.event)
    }
}

/// Where the mandate was loaded from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestSource {
    /// `refs/heads/<trusted>:.gitbots/manifest.json` (blob oid).
    Trusted { branch: String, oid: String },
    /// Not committed to the trusted branch yet.
    WorkingTree,
}

pub struct Project {
    repo: Repo,
    manifest: Manifest,
    source: ManifestSource,
    trusted_branch: String,
    ids: Mutex<ulid::Generator>,
}

#[derive(Clone, Debug)]
pub struct InitOptions {
    pub name: Option<String>,
    pub goal: Option<String>,
    pub autonomy: Autonomy,
    pub owner_kind: OwnerKind,
    pub hooks: bool,
    pub gitbots_bin: Option<PathBuf>,
    /// `git add .gitbots && git commit` after writing the manifest.
    pub commit: bool,
}

#[derive(Debug)]
pub struct InitReport {
    pub project: ProjectId,
    pub manifest: PathBuf,
    pub created_manifest: bool,
    pub created_ledgers: Vec<String>,
    pub hooks: Option<HookReport>,
    pub committed: bool,
    pub trusted_branch: String,
}

#[derive(Clone, Debug, Default)]
pub struct StartSession {
    pub agent: Option<AgentDescriptor>,
    pub parent: Option<String>,
    pub role: Option<String>,
    pub label: Option<String>,
    pub external_id: Option<String>,
    /// Bind the session to the current worktree.
    pub bind: bool,
    /// How the session was started (default: `flag`).
    pub via: Option<Via>,
}

#[derive(Clone, Debug, Default)]
pub struct CreateTask {
    pub title: String,
    pub body: Option<String>,
    pub labels: Vec<String>,
    pub recipe: Option<String>,
    pub inputs: BTreeMap<String, String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct AttemptInfo {
    pub attempt: AttemptId,
    pub task: TaskId,
    pub branch: String,
    pub workroom: PathBuf,
    pub base: String,
    pub base_commit: String,
    pub session: Option<SessionId>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SubmitOutcome {
    pub attempt: AttemptId,
    pub branch: String,
    pub head: String,
    pub diff: DiffStat,
    pub runs: Vec<ActionRun>,
    pub invalid_workflows: Vec<(String, String)>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ReviewOutcome {
    pub attempt: AttemptId,
    pub decision: ReviewDecision,
    /// The `review.decided` event.
    pub event: EventId,
    pub merged: Option<String>,
}

/// Workflows that parsed, by path.
pub type Workflows = Vec<(String, gitbots_actions::Workflow)>;
/// Workflow files that didn't parse: `(path, error)`.
pub type InvalidWorkflows = Vec<(String, String)>;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ActionsOutcome {
    pub runs: Vec<ActionRun>,
    pub invalid_workflows: Vec<(String, String)>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Inbox {
    pub awaiting_review: Vec<AttemptView>,
    pub reports: Vec<ReportView>,
}

#[derive(Clone, Debug, Default)]
pub struct EventFilter {
    pub kind: Option<String>,
    pub session: Option<String>,
    pub task: Option<String>,
    pub attempt: Option<String>,
    pub limit: Option<usize>,
}

impl Project {
    /// Create or join an gitbots project in the repo at `path` (running
    /// `git init` if needed). Idempotent: re-running on an initialized repo
    /// only ensures ledgers, hooks and local config.
    pub fn init(path: &Path, opts: InitOptions) -> Result<(Project, InitReport)> {
        let repo = match Repo::discover(path) {
            Ok(repo) => repo,
            Err(_) => Repo::init(path)?,
        };
        let root = repo.main_workdir()?;
        let manifest_path = root.join(MANIFEST_PATH);
        let trusted_branch = match repo.config_get("gitbots.trustedBranch")? {
            Some(b) => b,
            None => repo.current_branch()?.unwrap_or_else(|| "main".to_owned()),
        };

        // Adopt existing ledgers from origin before creating our own, so two
        // clones never start unrelated ledger histories.
        let has_origin = repo.config_get("remote.origin.url")?.is_some();

        let created_manifest = !manifest_path.exists();
        let manifest = if created_manifest {
            let (handle, email) = repo.user_identity().context(
                "set `git config user.name` (and user.email) first: it becomes the project owner",
            )?;
            let name = opts.name.clone().unwrap_or_else(|| {
                root.file_name().map_or("project".into(), |n| n.to_string_lossy().into_owned())
            });
            let owner = Principal { handle: handle.clone(), email, role: Role::Owner };
            let mut mandate = Mandate::new(opts.autonomy, owner);
            mandate.goal = opts.goal.clone();
            let manifest = Manifest::new(
                ProjectMeta { id: ProjectId::from_ulid(Ulid::generate()), name, description: None },
                Owner { kind: opts.owner_kind, handle },
                mandate,
            );
            manifest.validate()?;
            std::fs::create_dir_all(root.join(ACTIONS_DIR))?;
            std::fs::create_dir_all(root.join(RECIPES_DIR))?;
            std::fs::write(root.join(GITBOTS_DIR).join("README.md"), GITBOTS_README)?;
            write_json(&manifest_path, &manifest)?;
            manifest
        } else {
            read_manifest_file(&manifest_path)?
        };

        repo.config_set_local("gitbots.trustedBranch", &trusted_branch)?;

        let mut created_ledgers = Vec::new();
        for (branch, kind) in [
            (&manifest.ledger.activity_branch, LedgerKind::Activity),
            (&manifest.ledger.logs_branch, LedgerKind::Logs),
        ] {
            if has_origin && !repo.branch_exists(branch)? {
                let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
                // Missing on the remote is fine.
                let _ = repo.git(&["fetch", "--quiet", "origin", &refspec]);
            }
            let ledger = Ledger::new(&repo, branch, kind);
            if ledger.ensure(&LedgerMeta::new(kind, manifest.project.id.clone()))? {
                created_ledgers.push(branch.clone());
            }
        }

        let hooks = if opts.hooks {
            let bin = match &opts.gitbots_bin {
                Some(b) => b.clone(),
                None => std::env::current_exe()?,
            };
            Some(repo.install_hooks(&bin)?)
        } else {
            None
        };

        let mut committed = false;
        if opts.commit && created_manifest {
            repo.git(&["add", GITBOTS_DIR])?;
            repo.git(&[
                "commit",
                "--quiet",
                "-m",
                "gitbots: initialize project",
                "--",
                GITBOTS_DIR,
            ])?;
            committed = true;
        }

        let project = Project::open(&root)?;
        if created_manifest {
            let ctx = project.human_ctx()?;
            project.record(
                &ctx,
                EventBody::ProjectInitialized(ProjectInitialized {
                    project: manifest.project.id.clone(),
                    name: manifest.project.name.clone(),
                }),
            )?;
        }
        let report = InitReport {
            project: manifest.project.id.clone(),
            manifest: manifest_path,
            created_manifest,
            created_ledgers,
            hooks,
            committed,
            trusted_branch,
        };
        Ok((project, report))
    }

    pub fn open(path: &Path) -> Result<Project> {
        let repo = Repo::discover(path).context("not inside a git repository")?;
        let trusted_branch =
            repo.config_get("gitbots.trustedBranch")?.unwrap_or_else(|| "main".to_owned());
        let trusted_ref = format!("refs/heads/{trusted_branch}");
        let (manifest, source) = match repo.read_blob_at(&trusted_ref, MANIFEST_PATH)? {
            Some((oid, bytes)) => {
                let m: Manifest = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing {trusted_branch}:{MANIFEST_PATH}"))?;
                (m, ManifestSource::Trusted { branch: trusted_branch.clone(), oid })
            }
            None => {
                let path = repo.main_workdir()?.join(MANIFEST_PATH);
                ensure!(path.exists(), "not an gitbots project (run `gitbots init`)");
                (read_manifest_file(&path)?, ManifestSource::WorkingTree)
            }
        };
        manifest.validate()?;
        let project = Project {
            repo,
            manifest,
            source,
            trusted_branch,
            ids: Mutex::new(ulid::Generator::new()),
        };
        // Root commits are deterministic per project, so a clone that ensures
        // before its first sync still fast-forwards onto the remote ledger.
        let id = &project.manifest.project.id;
        project.activity().ensure(&LedgerMeta::new(LedgerKind::Activity, id.clone()))?;
        project.logs().ensure(&LedgerMeta::new(LedgerKind::Logs, id.clone()))?;
        Ok(project)
    }

    pub fn repo(&self) -> &Repo {
        &self.repo
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn manifest_source(&self) -> &ManifestSource {
        &self.source
    }

    pub fn trusted_branch(&self) -> &str {
        &self.trusted_branch
    }

    fn mandate_oid(&self) -> Option<String> {
        match &self.source {
            ManifestSource::Trusted { oid, .. } => Some(oid.clone()),
            ManifestSource::WorkingTree => None,
        }
    }

    pub fn activity(&self) -> Activity<'_> {
        Activity(Ledger::new(
            &self.repo,
            &self.manifest.ledger.activity_branch,
            LedgerKind::Activity,
        ))
    }

    pub fn logs(&self) -> Logs<'_> {
        Logs(Ledger::new(&self.repo, &self.manifest.ledger.logs_branch, LedgerKind::Logs))
    }

    // ---- identity -------------------------------------------------------

    /// Resolve who is acting: `--session` flag, then `GITBOTS_SESSION`, then
    /// the session bound to this worktree, then (when an agent harness runs
    /// this process) the agent's auto-started session, then the git-config
    /// human. Agent work is never attributed to the human.
    pub fn resolve_actor(&self, explicit_session: Option<&str>) -> Result<ActorCtx> {
        if let Some(q) = explicit_session {
            return Ok(ActorCtx { actor: self.session(q)?.actor(), via: Via::Flag });
        }
        if let Some(q) = std::env::var("GITBOTS_SESSION").ok().filter(|s| !s.trim().is_empty()) {
            return Ok(ActorCtx { actor: self.session(q.trim())?.actor(), via: Via::Env });
        }
        if let Some(q) = gitbots_git::read_binding(&self.repo.git_dir(), "session")? {
            return Ok(ActorCtx { actor: self.session(&q)?.actor(), via: Via::Worktree });
        }
        if let Some(marker) = agent_marker() {
            let session = self.auto_session(&marker)?;
            return Ok(ActorCtx { actor: session.actor(), via: Via::Env });
        }
        self.human_ctx()
    }

    /// The session of an agent harness that named none (`marker` says one
    /// runs this process): the one started for it in this worktree before
    /// (binding `auto-session`), while the same agent runs and it hasn't
    /// ended; else a new one, labelled `auto: <marker>`.
    fn auto_session(&self, marker: &str) -> Result<Session> {
        let agent =
            detect_agent().unwrap_or_else(|| AgentDescriptor::new("unknown", "unknown", "unknown"));
        let git_dir = self.repo.git_dir();
        if let Some(id) = gitbots_git::read_binding(&git_dir, AUTO_SESSION_BINDING)?
            && let Ok(session) = self.session(&id)
            && session.agent.key() == agent.key()
            && !self.board()?.sessions.get(&session.id).is_some_and(|s| s.ended)
        {
            return Ok(session);
        }
        let session = self.start_session(StartSession {
            agent: Some(agent),
            label: Some(format!("auto: {marker}")),
            via: Some(Via::Env),
            ..StartSession::default()
        })?;
        gitbots_git::write_binding(&git_dir, AUTO_SESSION_BINDING, session.id.as_str())?;
        eprintln!(
            "gitbots: `{marker}` says an agent runs this process, but no session was given: \
             started {} as {}; this worktree reuses it (or `export GITBOTS_SESSION={}`)",
            session.id,
            session.agent.key(),
            session.id
        );
        Ok(session)
    }

    fn human_ctx(&self) -> Result<ActorCtx> {
        self.human_via(Via::GitConfig)
    }

    /// The git-config human, attributed with an explicit `via`. Front ends
    /// that authenticate the human themselves (the local UI's launch token)
    /// use this instead of the git-config fallback.
    pub fn human_via(&self, via: Via) -> Result<ActorCtx> {
        let (handle, email) =
            self.repo.user_identity().context("no session and no git user configured")?;
        Ok(ActorCtx { actor: Actor::human(handle, email), via })
    }

    /// Upgrade a git-config human to a confirmed one, or refuse. Decisions
    /// the mandate reserves for humans must not be reachable by an agent
    /// that simply unsets `GITBOTS_SESSION`.
    fn confirm_human(&self, ctx: &ActorCtx) -> Result<ActorCtx> {
        if ctx.via != Via::GitConfig {
            return Ok(ctx.clone());
        }
        if let Some(marker) = agent_marker() {
            return Err(Refused(format!(
                "refusing: this decision needs a human, but `{marker}` says an agent is running this \
                 process. Start a session (`gitbots session start`) or ask your human."
            ))
            .into());
        }
        if !std::io::stdin().is_terminal() {
            return Err(Refused(
                "refusing: this decision needs a human at an interactive terminal".into(),
            )
            .into());
        }
        Ok(ActorCtx { actor: ctx.actor.clone(), via: Via::Tty })
    }

    /// Look up a session by id or short id.
    pub fn session(&self, query: &str) -> Result<Session> {
        if let Ok(id) = query.parse::<SessionId>()
            && let Some(bytes) = self.activity().0.read(&ledger::session_path(&id))?
        {
            return Ok(serde_json::from_slice(&bytes)?);
        }
        let hits: Vec<Session> =
            self.activity().sessions()?.into_iter().filter(|s| s.id.matches(query)).collect();
        match hits.len() {
            1 => Ok(hits.into_iter().next().expect("one hit")),
            0 => bail!("no session matches `{query}` (start one with `gitbots session start`)"),
            _ => bail!("session `{query}` is ambiguous"),
        }
    }

    pub fn start_session(&self, req: StartSession) -> Result<Session> {
        let agent = match req.agent {
            Some(a) => a,
            None => detect_agent().context(
                "can't tell which agent this is: pass --provider/--model/--client or set GITBOTS_PROVIDER/GITBOTS_MODEL/GITBOTS_CLIENT",
            )?,
        };
        // Subagents inherit GITBOTS_SESSION from the agent that spawned them.
        let parent =
            match req.parent.as_deref().or(std::env::var("GITBOTS_SESSION").ok().as_deref()) {
                Some(q) if !q.trim().is_empty() => Some(self.session(q.trim())?.id),
                _ => None,
            };
        let session = Session {
            id: SessionId::from_ulid(self.next_ulid()),
            agent,
            parent,
            role: req.role,
            operator: self.repo.user_identity().map(|(h, _)| h),
            external_id: req.external_id,
            label: req.label,
            started_at: OffsetDateTime::now_utc(),
        };
        let ctx = ActorCtx { actor: session.actor(), via: req.via.unwrap_or(Via::Flag) };
        let event = self.new_event(
            &ctx,
            EventBody::SessionStarted(SessionStarted { session: session.clone() }),
        )?;
        self.activity().put_session(&session, &event)?;
        if req.bind {
            gitbots_git::write_binding(&self.repo.git_dir(), "session", session.id.as_str())?;
        }
        Ok(session)
    }

    pub fn end_session(&self, ctx: &ActorCtx, summary: Option<String>) -> Result<SessionId> {
        let session = ctx.actor.session().cloned().context("no active agent session")?;
        self.record(
            ctx,
            EventBody::SessionEnded(SessionEnded { session: session.clone(), summary }),
        )?;
        for key in ["session", AUTO_SESSION_BINDING] {
            if gitbots_git::read_binding(&self.repo.git_dir(), key)?.as_deref()
                == Some(session.as_str())
            {
                gitbots_git::remove_binding(&self.repo.git_dir(), key)?;
            }
        }
        Ok(session)
    }

    // ---- events ---------------------------------------------------------

    fn next_ulid(&self) -> Ulid {
        let mut g = self.ids.lock().unwrap_or_else(|e| e.into_inner());
        g.generate().unwrap_or_else(|overflow| overflow.commit_overflow_random())
    }

    /// A new event id, bumped past the newest event at the ledger tip so a
    /// slow clock can't order our event before one we've already seen.
    fn next_event_id(&self) -> Result<EventId> {
        let mut ulid = self.next_ulid();
        if let Some(latest) = self.activity().latest_event_id()? {
            let latest = latest.ulid();
            if ulid <= latest && latest.timestamp_ms() <= ulid.timestamp_ms() + MAX_SKEW_MS {
                ulid = Ulid::from_parts(latest.timestamp_ms() + 1, ulid.random());
            }
        }
        Ok(EventId::from_ulid(ulid))
    }

    fn new_event(&self, ctx: &ActorCtx, body: EventBody) -> Result<Event> {
        let mut e =
            Event::new(self.next_event_id()?, OffsetDateTime::now_utc(), ctx.actor.clone(), body)
                .with_via(ctx.via);
        e.producer = Some(PRODUCER.to_owned());
        Ok(e)
    }

    /// Append one event as `ctx`.
    pub fn record(&self, ctx: &ActorCtx, body: EventBody) -> Result<Event> {
        let event = self.new_event(ctx, body)?;
        self.activity().append(std::slice::from_ref(&event))?;
        Ok(event)
    }

    /// Append one event; with `idem`, fail if that idempotency key is
    /// already in the ledger (see [`Project::event_with_idem`]).
    fn record_keyed(&self, ctx: &ActorCtx, body: EventBody, idem: Option<&str>) -> Result<Event> {
        let Some(key) = idem else { return self.record(ctx, body) };
        let event = self.new_event(ctx, body)?.with_idem(key);
        let out = self.activity().append(std::slice::from_ref(&event))?;
        if out.written.is_empty() {
            return Err(AlreadyRecorded { key: key.to_owned() }.into());
        }
        Ok(event)
    }

    /// The event recorded with idempotency key `key`, if any.
    pub fn event_with_idem(&self, key: &str) -> Result<Option<EventId>> {
        Ok(self.events()?.into_iter().find(|e| e.idem.as_deref() == Some(key)).map(|e| e.id))
    }

    /// Append one event unless an event with the same idempotency key exists.
    pub fn record_once(
        &self,
        ctx: &ActorCtx,
        body: EventBody,
        idem: String,
    ) -> Result<Option<Event>> {
        let event = self.new_event(ctx, body)?.with_idem(idem);
        let out = self.activity().append(std::slice::from_ref(&event))?;
        Ok((!out.written.is_empty()).then_some(event))
    }

    pub fn events(&self) -> Result<Vec<Event>> {
        Ok(self.activity().events()?.events)
    }

    pub fn query_events(&self, filter: &EventFilter) -> Result<Vec<Event>> {
        let board = self.board()?;
        let session = filter.session.as_deref().map(|q| self.session(q)).transpose()?.map(|s| s.id);
        let task =
            filter.task.as_deref().map(|q| board.find_task(q)).transpose()?.map(|t| t.id.clone());
        let attempt = filter
            .attempt
            .as_deref()
            .map(|q| board.find_attempt(q))
            .transpose()?
            .map(|a| a.id.clone());
        let mut out: Vec<Event> = self
            .events()?
            .into_iter()
            .filter(|e| {
                filter
                    .kind
                    .as_deref()
                    .is_none_or(|k| e.kind() == k || e.kind().starts_with(&format!("{k}.")))
            })
            .filter(|e| session.as_ref().is_none_or(|s| e.actor.session() == Some(s)))
            .filter(|e| {
                task.as_ref().is_none_or(|t| {
                    e.body.task() == Some(t)
                        || e.body
                            .attempt()
                            .and_then(|a| board.attempts.get(a))
                            .is_some_and(|a| &a.task == t)
                })
            })
            .filter(|e| attempt.as_ref().is_none_or(|a| e.body.attempt() == Some(a)))
            .collect();
        if let Some(limit) = filter.limit {
            let skip = out.len().saturating_sub(limit);
            out.drain(..skip);
        }
        Ok(out)
    }

    pub fn board(&self) -> Result<Board> {
        Ok(Board::from_events(&self.events()?))
    }

    pub fn stats(&self) -> Result<Stats> {
        Ok(Stats::from_events(&self.events()?))
    }

    pub fn sessions(&self) -> Result<Vec<SessionView>> {
        Ok(self.board()?.sessions.into_values().collect())
    }

    pub fn inbox(&self) -> Result<Inbox> {
        let board = self.board()?;
        let mut reports = board.reports.clone();
        reports.sort_by(|a, b| {
            b.report.level.cmp_rank().cmp(&a.report.level.cmp_rank()).then(b.id.cmp(&a.id))
        });
        reports.truncate(20);
        Ok(Inbox { awaiting_review: board.awaiting_review().cloned().collect(), reports })
    }

    // ---- tasks & attempts ----------------------------------------------

    pub fn create_task(&self, ctx: &ActorCtx, req: CreateTask) -> Result<TaskId> {
        Ok(self.create_task_keyed(ctx, req, None)?.0)
    }

    /// [`Project::create_task`] recorded under idempotency key `idem`; fails
    /// if the key was used before. Returns the `task.created` event too.
    pub fn create_task_once(
        &self,
        ctx: &ActorCtx,
        req: CreateTask,
        idem: &str,
    ) -> Result<(TaskId, EventId)> {
        self.create_task_keyed(ctx, req, Some(idem))
    }

    fn create_task_keyed(
        &self,
        ctx: &ActorCtx,
        req: CreateTask,
        idem: Option<&str>,
    ) -> Result<(TaskId, EventId)> {
        let (body, recipe) = match &req.recipe {
            Some(name) => {
                let recipe = self.recipe(name)?;
                let rendered = recipe.render(&req.inputs)?;
                let body = match req.body {
                    Some(extra) => format!("{rendered}\n\n{extra}"),
                    None => rendered,
                };
                (Some(body), Some(recipe.id()))
            }
            None => {
                ensure!(req.inputs.is_empty(), "--input needs --recipe");
                (req.body, None)
            }
        };
        let task = TaskId::from_ulid(self.next_ulid());
        let event = self.record_keyed(
            ctx,
            EventBody::TaskCreated(TaskCreated {
                task: task.clone(),
                title: req.title,
                body,
                recipe,
                labels: req.labels,
            }),
            idem,
        )?;
        Ok((task, event.id))
    }

    pub fn workrooms_dir(&self) -> Result<PathBuf> {
        if let Some(dir) = self.repo.config_get("gitbots.workrooms")? {
            return Ok(PathBuf::from(dir));
        }
        Ok(gitbots_home()?.join("workrooms").join(self.manifest.project.id.as_str()))
    }

    pub fn start_attempt(
        &self,
        ctx: &ActorCtx,
        task_query: &str,
        base: Option<&str>,
        session: Option<&str>,
    ) -> Result<AttemptInfo> {
        let board = self.board()?;
        let task = board.find_task(task_query)?;
        let base = base.unwrap_or(&self.trusted_branch).to_owned();
        let base_commit = self
            .repo
            .resolve(&base)?
            .with_context(|| format!("base `{base}` has no commits yet; commit something first"))?;

        let attempt = AttemptId::from_ulid(self.next_ulid());
        let name = format!("{}-{}", slugify(&task.title), attempt.short());
        let branch = format!("{}/{name}", self.manifest.workrooms.branch_prefix);
        let workroom = self.workrooms_dir()?.join(&name);
        std::fs::create_dir_all(workroom.parent().expect("workroom has a parent"))?;
        self.repo.add_worktree(&workroom, &branch, &base_commit)?;

        let session = match session {
            Some(q) => Some(self.session(q)?.id),
            None => ctx.actor.session().cloned(),
        };
        let wt_git_dir = self.repo.worktree_git_dir(&workroom)?;
        gitbots_git::write_binding(&wt_git_dir, "attempt", attempt.as_str())?;
        if let Some(s) = &session {
            gitbots_git::write_binding(&wt_git_dir, "session", s.as_str())?;
        }

        self.record(
            ctx,
            EventBody::AttemptStarted(AttemptStarted {
                task: task.id.clone(),
                attempt: attempt.clone(),
                branch: branch.clone(),
                base: base.clone(),
                base_commit: base_commit.clone(),
                session: session.clone(),
            }),
        )?;
        Ok(AttemptInfo {
            attempt,
            task: task.id.clone(),
            branch,
            workroom,
            base,
            base_commit,
            session,
        })
    }

    /// The attempt bound to the current worktree, if any.
    pub fn current_attempt(&self) -> Result<Option<String>> {
        gitbots_git::read_binding(&self.repo.git_dir(), "attempt")
    }

    fn attempt_view(&self, board: &Board, query: Option<&str>) -> Result<AttemptView> {
        let query = match query {
            Some(q) => q.to_owned(),
            None => {
                self.current_attempt()?.context("not inside a workroom: pass the attempt id")?
            }
        };
        Ok(board.find_attempt(&query)?.clone())
    }

    pub fn workroom_of(&self, attempt: &AttemptView) -> Result<Option<PathBuf>> {
        self.repo.checked_out_in(&attempt.branch)
    }

    /// Paths an agent's attempt changes that the mandate forbids.
    pub fn violations(&self, attempt: &AttemptView) -> Result<Vec<PathViolation>> {
        let head = format!("refs/heads/{}", attempt.branch);
        let base = format!("refs/heads/{}", attempt.base);
        let changed = self.repo.changed_paths(&base, &head)?;
        Ok(self.manifest.mandate.agents.violations(changed.iter().map(String::as_str))?)
    }

    pub async fn submit_attempt(
        &self,
        ctx: &ActorCtx,
        attempt_query: Option<&str>,
        summary: Option<String>,
    ) -> Result<SubmitOutcome> {
        let board = self.board()?;
        let attempt = self.attempt_view(&board, attempt_query)?;
        ensure!(attempt.state.is_open(), "attempt is {}, not open", attempt.state.as_str());
        let head = self
            .repo
            .resolve(&format!("refs/heads/{}", attempt.branch))?
            .context("attempt branch is gone")?;
        ensure!(head != attempt.base_commit, "attempt has no commits yet");

        if !ctx.actor.is_human() {
            let violations = self.violations(&attempt)?;
            if !violations.is_empty() {
                let list: Vec<String> =
                    violations.iter().map(|v| format!("  {} ({})", v.path, v.reason)).collect();
                bail!("mandate violation, submit refused:\n{}", list.join("\n"));
            }
        }

        let base = format!("refs/heads/{}", attempt.base);
        let diff = self.repo.diffstat(&base, &head)?;
        self.record(
            ctx,
            EventBody::AttemptSubmitted(AttemptSubmitted {
                attempt: attempt.id.clone(),
                head: head.clone(),
                summary,
                diff: Some(diff),
                mandate: self.mandate_oid(),
            }),
        )?;
        let actions =
            self.run_actions(ctx, kind::ATTEMPT_SUBMITTED, Some(&attempt.id), None).await?;
        Ok(SubmitOutcome {
            attempt: attempt.id,
            branch: attempt.branch,
            head,
            diff,
            runs: actions.runs,
            invalid_workflows: actions.invalid_workflows,
        })
    }

    pub fn handoff(
        &self,
        ctx: &ActorCtx,
        attempt_query: &str,
        to: HandoffTarget,
        note: Option<String>,
    ) -> Result<()> {
        let board = self.board()?;
        let attempt = board.find_attempt(attempt_query)?.clone();
        ensure!(attempt.state.is_open(), "attempt is {}, not open", attempt.state.as_str());
        let to = match to {
            HandoffTarget::Session { session } => {
                let session = self.session(session.as_str())?.id;
                // The workroom now attributes commits to the new holder.
                if let Some(wt) = self.workroom_of(&attempt)? {
                    let git_dir = self.repo.worktree_git_dir(&wt)?;
                    gitbots_git::write_binding(&git_dir, "session", session.as_str())?;
                }
                HandoffTarget::Session { session }
            }
            other => other,
        };
        self.record(
            ctx,
            EventBody::AttemptHandoff(AttemptHandoff { attempt: attempt.id, to, note }),
        )?;
        Ok(())
    }

    pub fn abandon(
        &self,
        ctx: &ActorCtx,
        attempt_query: &str,
        reason: Option<String>,
        remove_workroom: bool,
    ) -> Result<()> {
        let board = self.board()?;
        let attempt = board.find_attempt(attempt_query)?.clone();
        ensure!(attempt.state.is_open(), "attempt is {}, not open", attempt.state.as_str());
        self.record(
            ctx,
            EventBody::AttemptAbandoned(AttemptAbandoned { attempt: attempt.id.clone(), reason }),
        )?;
        if remove_workroom && let Some(wt) = self.workroom_of(&attempt)? {
            self.repo.remove_worktree(&wt, true)?;
        }
        Ok(())
    }

    fn authorize(&self, ctx: &ActorCtx, decision: Decision) -> Result<ActorCtx> {
        let ctx = if self.manifest.mandate.approvals.get(decision)
            == gitbots_core::manifest::Approver::Any
        {
            ctx.clone()
        } else {
            self.confirm_human(ctx)?
        };
        match self.manifest.mandate.authorize(&ctx.actor, decision) {
            Authorization::Allowed => Ok(ctx),
            Authorization::NeedsHuman { role } => Err(Refused(format!(
                "{decision} needs a human with role {role}; the attempt is waiting in `gitbots inbox`"
            ))
            .into()),
            Authorization::Denied { reason } => Err(Refused(format!("denied: {reason}")).into()),
        }
    }

    pub fn review(
        &self,
        ctx: &ActorCtx,
        attempt_query: &str,
        decision: ReviewDecision,
        reason: Option<String>,
        merge: bool,
    ) -> Result<ReviewOutcome> {
        self.review_keyed(ctx, attempt_query, decision, reason, merge, None)
    }

    /// [`Project::review`] with the `review.decided` event recorded under
    /// idempotency key `idem`; fails if the key was used before.
    pub fn review_once(
        &self,
        ctx: &ActorCtx,
        attempt_query: &str,
        decision: ReviewDecision,
        reason: Option<String>,
        merge: bool,
        idem: &str,
    ) -> Result<ReviewOutcome> {
        self.review_keyed(ctx, attempt_query, decision, reason, merge, Some(idem))
    }

    fn review_keyed(
        &self,
        ctx: &ActorCtx,
        attempt_query: &str,
        decision: ReviewDecision,
        reason: Option<String>,
        merge: bool,
        idem: Option<&str>,
    ) -> Result<ReviewOutcome> {
        let board = self.board()?;
        let attempt = board.find_attempt(attempt_query)?.clone();
        ensure!(
            attempt.state == AttemptState::Submitted,
            "attempt is {}; only submitted attempts can be reviewed",
            attempt.state.as_str()
        );
        if let Some(me) = ctx.actor.session() {
            let family = board.session_family(me);
            let authors =
                [attempt.session.as_ref(), attempt.submitted_by.as_ref().and_then(Actor::session)];
            ensure!(
                !authors.into_iter().flatten().any(|s| family.contains(s)),
                "an agent can't review work from its own session tree"
            );
        }
        let ctx = self.authorize(ctx, Decision::AcceptAttempt)?;
        let ctx = if merge && self.manifest.mandate.is_protected_branch(&attempt.base) {
            self.authorize(&ctx, Decision::MergeProtected)?
        } else {
            ctx
        };
        ensure!(!merge || decision == ReviewDecision::Accept, "--merge only applies to accept");

        let decided = self.record_keyed(
            &ctx,
            EventBody::ReviewDecided(ReviewDecided {
                attempt: attempt.id.clone(),
                decision,
                reason,
                mandate: self.mandate_oid(),
            }),
            idem,
        )?;

        let merge_it = || -> Result<String> {
            let head = attempt.head.clone().context("attempt has no head")?;
            let source_commits =
                self.repo.commits_between(&format!("refs/heads/{}", attempt.base), &head)?;
            let task_title = board.tasks.get(&attempt.task).map_or("", |t| t.title.as_str());
            let message = format!(
                "Merge attempt {}: {task_title}\n\nGitbots-Attempt: {}\nGitbots-Task: {}",
                attempt.id.short(),
                attempt.id,
                attempt.task
            );
            let commit = self.repo.merge_into(&attempt.base, &head, &message)?;
            let mut e = self.new_event(
                &ctx,
                EventBody::AttemptMerged(AttemptMerged {
                    attempt: attempt.id.clone(),
                    into: attempt.base.clone(),
                    commit: commit.clone(),
                    source_commits,
                }),
            )?;
            e.on = Some(decided.id.clone());
            self.activity().append(std::slice::from_ref(&e))?;
            Ok(commit)
        };
        let merged = match merge {
            true => {
                Some(merge_it().map_err(|e| e.context(NotMerged { event: decided.id.clone() }))?)
            }
            false => None,
        };
        Ok(ReviewOutcome { attempt: attempt.id, decision, event: decided.id, merged })
    }

    pub fn report(&self, ctx: &ActorCtx, report: Report) -> Result<EventId> {
        Ok(self.record(ctx, EventBody::Report(report))?.id)
    }

    pub fn trace(&self, ctx: &ActorCtx, call: ToolCalled) -> Result<EventId> {
        Ok(self.record(ctx, EventBody::ToolCalled(call))?.id)
    }

    /// Store a log on the logs branch under the actor's session.
    pub fn put_session_log(&self, ctx: &ActorCtx, name: &str, bytes: &[u8]) -> Result<LogRef> {
        let session = ctx.actor.session().context("session logs need an agent session")?;
        let path = ledger::session_log_path(session, self.next_ulid(), name);
        self.logs().put(&path, bytes)
    }

    // ---- recipes & actions ---------------------------------------------

    fn trusted_files(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let trusted = format!("refs/heads/{}", self.trusted_branch);
        let files = self.repo.list_blobs_at(&trusted, prefix)?;
        if files.is_empty() && self.source == ManifestSource::WorkingTree {
            return read_dir_files(&self.repo.main_workdir()?, prefix);
        }
        Ok(files)
    }

    pub fn recipes(&self) -> Result<Vec<(String, Result<Recipe>)>> {
        Ok(self
            .trusted_files(RECIPES_DIR)?
            .into_iter()
            .filter(|(p, _)| p.ends_with("/recipe.toml"))
            .map(|(p, bytes)| {
                let parsed = std::str::from_utf8(&bytes)
                    .map_err(anyhow::Error::from)
                    .and_then(|s| Ok(toml::from_str::<Recipe>(s)?))
                    .and_then(|r| {
                        r.validate()?;
                        Ok(r)
                    });
                (p, parsed)
            })
            .collect())
    }

    pub fn recipe(&self, name: &str) -> Result<Recipe> {
        let path = format!("{RECIPES_DIR}{name}/recipe.toml");
        self.recipes()?
            .into_iter()
            .find(|(p, _)| p.ends_with(&path) || p == &path)
            .map(|(_, r)| r)
            .with_context(|| format!("no recipe `{name}` on {}", self.trusted_branch))?
    }

    pub fn workflows(&self) -> Result<(Workflows, InvalidWorkflows)> {
        let files: Vec<_> = self
            .trusted_files(ACTIONS_DIR)?
            .into_iter()
            .filter(|(p, _)| p.ends_with(".toml"))
            .collect();
        let (ok, bad) = gitbots_actions::load_workflows(&files);
        Ok((ok, bad.into_iter().map(|(p, e)| (p, e.to_string())).collect()))
    }

    /// Run every workflow on `trigger` (optionally only `only`), against
    /// the attempt's workroom or the main worktree.
    ///
    /// Workflows come from the trusted branch, so event-triggered runs are
    /// pre-approved by whoever merged them. A *manual* run on a hosted
    /// runner (it costs money) needs the `run_hosted_action` decision.
    pub async fn run_actions(
        &self,
        ctx: &ActorCtx,
        trigger: &str,
        attempt: Option<&AttemptId>,
        only: Option<&str>,
    ) -> Result<ActionsOutcome> {
        let (workflows, invalid_workflows) = self.workflows()?;
        let selected: Vec<_> = gitbots_actions::matching(&workflows, trigger)
            .into_iter()
            .filter(|(_, wf)| only.is_none_or(|n| wf.name == n))
            .cloned()
            .collect();
        if let Some(name) = only {
            ensure!(!selected.is_empty(), "no workflow `{name}` triggers on `{trigger}`");
        }
        if selected.is_empty() {
            return Ok(ActionsOutcome { runs: vec![], invalid_workflows });
        }
        let hosted = selected.iter().any(|(_, wf)| wf.jobs.values().any(|j| j.runs_on != "local"));
        if trigger == "manual" && hosted {
            self.authorize(ctx, Decision::RunHostedAction)?;
        }

        let view = match attempt {
            Some(id) => Some(self.board()?.attempts.get(id).cloned().context("unknown attempt")?),
            None => None,
        };
        let (workdir, commit) = match &view {
            Some(a) => {
                let wt = self.workroom_of(a)?.context("attempt's workroom is gone")?;
                (wt, self.repo.resolve(&format!("refs/heads/{}", a.branch))?)
            }
            None => (self.repo.workdir().context("bare repository")?, self.repo.resolve("HEAD")?),
        };
        let source = match (self.repo.config_get("remote.origin.url")?, &commit) {
            (Some(url), Some(c)) => Some(Source { clone_url: url, commit: c.clone() }),
            _ => None,
        };

        let runners = RunnerRegistry::with_defaults();
        let system = ActorCtx::system("actions");
        let mut runs = Vec::new();
        for (_, wf) in selected {
            let req = RunRequest {
                run: RunId::from_ulid(self.next_ulid()),
                trigger: trigger.to_owned(),
                workdir: workdir.clone(),
                attempt: attempt.cloned(),
                commit: commit.clone(),
                source: source.clone(),
                extra_env: BTreeMap::new(),
            };
            let mut sink = LedgerSink { logs: self.logs() };
            let run = gitbots_actions::run_workflow(&wf, &req, &runners, &mut sink).await?;
            self.record(&system, EventBody::ActionCompleted(run.clone()))?;
            runs.push(run);
        }
        Ok(ActionsOutcome { runs, invalid_workflows })
    }

    // ---- git hooks ------------------------------------------------------

    /// `prepare-commit-msg`: attribute agent commits with trailers.
    pub fn hook_prepare_commit_msg(&self, msg_file: &Path) -> Result<()> {
        let ctx = self.resolve_actor(None)?;
        let Actor::Agent { session, agent, parent } = &ctx.actor else { return Ok(()) };
        // An editor commit starts from an empty template; adding trailers
        // there would stop git from aborting when the message is left empty.
        let msg = std::fs::read_to_string(msg_file)?;
        if !msg.lines().any(|l| !l.trim().is_empty() && !l.starts_with('#')) {
            return Ok(());
        }
        let mut trailers = vec![
            ("Gitbots-Session".to_owned(), session.to_string()),
            ("Gitbots-Provider".to_owned(), agent.provider.clone()),
            ("Gitbots-Model".to_owned(), agent.model.clone()),
            ("Gitbots-Client".to_owned(), agent.client.clone()),
        ];
        if let Some(p) = parent {
            trailers.push(("Gitbots-Parent-Session".to_owned(), p.to_string()));
        }
        self.repo.add_trailers(msg_file, &trailers)
    }

    /// `post-commit`: record agent commits (and any commit in a workroom).
    pub fn hook_post_commit(&self) -> Result<()> {
        let ctx = self.resolve_actor(None)?;
        let attempt = self.current_attempt()?.and_then(|a| a.parse::<AttemptId>().ok());
        if ctx.actor.is_human() && attempt.is_none() {
            return Ok(());
        }
        let sha = self.repo.resolve("HEAD")?.context("no HEAD after commit")?;
        let body = EventBody::CommitRecorded(CommitRecorded {
            subject: self.repo.commit_subject(&sha)?,
            branch: self.repo.current_branch()?,
            attempt,
            diff: Some(self.repo.commit_diffstat(&sha)?),
            sha: sha.clone(),
        });
        self.record_once(&ctx, body, format!("commit:{sha}"))?;
        Ok(())
    }

    pub fn install_hooks(&self, gitbots_bin: &Path) -> Result<HookReport> {
        self.repo.install_hooks(gitbots_bin)
    }

    pub fn sync(
        &self,
        remote: &str,
        include_logs: bool,
        push: bool,
    ) -> Result<Vec<gitbots_git::SyncReport>> {
        let mut branches = vec![self.manifest.ledger.activity_branch.as_str()];
        if include_logs {
            branches.push(self.manifest.ledger.logs_branch.as_str());
        }
        gitbots_git::sync(&self.repo, remote, &branches, push)
    }
}

/// Writes job logs to the logs branch.
struct LedgerSink<'r> {
    logs: Logs<'r>,
}

impl gitbots_actions::LogSink for LedgerSink<'_> {
    fn put(&mut self, run: &RunId, job: &str, log: &[u8]) -> Result<Option<LogRef>> {
        Ok(Some(self.logs.put(&ledger::run_log_path(run, job), log)?))
    }
}

trait LevelRank {
    fn cmp_rank(&self) -> u8;
}

impl LevelRank for ReportLevel {
    fn cmp_rank(&self) -> u8 {
        match self {
            ReportLevel::Info => 0,
            ReportLevel::Warning => 1,
            ReportLevel::Blocker => 2,
        }
    }
}

/// Env vars that show an agent harness is driving this process.
pub fn agent_marker() -> Option<String> {
    const EXACT: &[&str] =
        &["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "GITBOTS_SESSION", "CURSOR_AGENT"];
    std::env::vars()
        .map(|(k, _)| k)
        .find(|k| EXACT.contains(&k.as_str()) || k.starts_with("CODEX_"))
}

/// Best-effort agent identity from the environment.
pub fn detect_agent() -> Option<AgentDescriptor> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let client = var("GITBOTS_CLIENT").or_else(|| {
        if var("CLAUDECODE").is_some() {
            Some("claude-code".into())
        } else if std::env::vars().any(|(k, _)| k.starts_with("CODEX_")) {
            Some("codex".into())
        } else {
            None
        }
    })?;
    let provider = var("GITBOTS_PROVIDER").unwrap_or_else(|| match client.as_str() {
        "claude-code" => "anthropic".into(),
        "codex" | "chatgpt" => "openai".into(),
        _ => "unknown".into(),
    });
    let model =
        var("GITBOTS_MODEL").or_else(|| var("ANTHROPIC_MODEL")).unwrap_or_else(|| "unknown".into());
    Some(AgentDescriptor::new(provider, model, client))
}

fn gitbots_home() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("GITBOTS_HOME") {
        return Ok(PathBuf::from(home));
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(data).join("gitbots"));
    }
    let home =
        std::env::home_dir().ok_or_else(|| anyhow!("no home directory; set GITBOTS_HOME"))?;
    Ok(home.join(".local/share/gitbots"))
}

pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 32 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() { "task".to_owned() } else { slug.to_owned() }
}

fn read_manifest_file(path: &Path) -> Result<Manifest> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let mut json = serde_json::to_string_pretty(value)?;
    json.push('\n');
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
}

fn read_dir_files(root: &Path, prefix: &str) -> Result<Vec<(String, Vec<u8>)>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(root, &path, out)?;
            } else {
                let rel = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
                out.push((rel, std::fs::read(&path)?));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    let dir = root.join(prefix);
    if dir.is_dir() {
        walk(root, &dir, &mut out)?;
    }
    out.sort();
    Ok(out)
}

const GITBOTS_README: &str = "\
# .gitbots

This directory makes the repo an gitbots project.

- `manifest.json`: project meta, tenancy and the agentic mandate. gitbots reads it
  from the trusted branch (git config `gitbots.trustedBranch`), so changes take
  effect once merged there.
- `actions/*.toml`: workflows run on ledger events (`attempt.submitted`, `manual`, ...).
- `recipes/<name>/recipe.toml`: reusable task recipes.

Agent activity lives on the `gitbots/activity` and `gitbots/logs` branches.
";

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Fix the Login bug!"), "fix-the-login-bug");
        assert_eq!(slugify("???"), "task");
        assert!(slugify(&"long title ".repeat(10)).len() <= 32);
    }
}

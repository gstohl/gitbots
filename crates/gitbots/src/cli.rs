//! The `gitbots` command line. Every command takes `--json` for agents.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

use gitbots_cloud::api::TokenScope;
use gitbots_core::event::{
    HandoffTarget, Report, ReportLevel, ReviewDecision, ToolCalled, short_sha,
};
use gitbots_core::manifest::{Autonomy, OwnerKind};
use gitbots_core::{AgentDescriptor, Via};

use crate::cloud::{self, AutoSync, Cloud, CloudConfig, SyncOptions};
use crate::output::{self, Out};
use crate::project::{ActorCtx, CreateTask, EventFilter, InitOptions, Project, StartSession};

#[derive(Parser, Debug)]
#[command(
    name = "gitbots",
    version,
    about = "Agent-native git: identity, activity ledger, workrooms, actions"
)]
pub struct Cli {
    /// Run as if started in this directory.
    #[arg(short = 'C', global = true, value_name = "PATH")]
    pub dir: Option<PathBuf>,
    /// Act as this session (overrides GITBOTS_SESSION and the workroom binding).
    #[arg(long, global = true, value_name = "SESSION")]
    pub session: Option<String>,
    /// Machine-readable output.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Make this repo an gitbots project (the agentic `git init`).
    Init(InitArgs),
    /// Tasks, attempts and what needs a decision.
    Status,
    /// Who gitbots thinks you are, and why.
    Whoami,
    /// Decisions and reports waiting for a human.
    Inbox,
    /// Agent sessions (one per chat or run).
    #[command(subcommand)]
    Session(SessionCmd),
    /// Units of work.
    #[command(subcommand)]
    Task(TaskCmd),
    /// Isolated tries at a task (branch + workroom).
    #[command(subcommand)]
    Attempt(AttemptCmd),
    /// Pass an attempt to another session, role or human.
    Handoff(HandoffArgs),
    /// Accept, reject or request changes on a submitted attempt.
    Review(ReviewArgs),
    /// Tell your human something (shows up in `gitbots inbox`).
    Report(ReportArgs),
    /// Record one agent tool call.
    Trace(TraceArgs),
    /// Activity ledger events.
    Log(LogArgs),
    /// Per-agent observability and benchmark numbers.
    Stats,
    /// Workflows in .gitbots/actions.
    #[command(subcommand)]
    Actions(ActionsCmd),
    /// Task recipes in .gitbots/recipes.
    #[command(subcommand)]
    Recipe(RecipeCmd),
    /// Fetch, union-merge and push the ledger branches. With gitbots cloud set
    /// up, also push code branches and apply the dashboard's decisions.
    Sync(SyncArgs),
    /// Host the project on Cloudflare: Artifacts remotes plus the gitbots Worker.
    #[command(subcommand)]
    Cloud(CloudCmd),
    /// Install the git hooks that attribute and record commits.
    Hooks,
    /// Serve gitbots to an AI client over MCP (stdio).
    Mcp,
    /// Serve the human web UI and its JSON API on localhost.
    Ui {
        #[arg(long, default_value_t = 7777)]
        port: u16,
        /// Built frontend directory (default: web/dist of this source tree, or GITBOTS_UI_ASSETS).
        #[arg(long)]
        assets: Option<PathBuf>,
    },
    /// Git hook entry points (installed by `gitbots init`).
    #[command(subcommand, hide = true)]
    Hook(HookCmd),
}

#[derive(Args, Debug)]
pub struct InitArgs {
    #[arg(long)]
    pub name: Option<String>,
    /// What agents in this repo are here to achieve.
    #[arg(long)]
    pub goal: Option<String>,
    #[arg(long, value_enum, default_value_t = AutonomyArg::Assisted)]
    pub autonomy: AutonomyArg,
    /// The owner is an organization, not a user.
    #[arg(long)]
    pub org: bool,
    #[arg(long)]
    pub no_hooks: bool,
    /// Commit .gitbots/ to the current branch.
    #[arg(long)]
    pub commit: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AutonomyArg {
    Supervised,
    Assisted,
    Autonomous,
}

#[derive(Subcommand, Debug)]
pub enum SessionCmd {
    /// Start a session for the calling agent.
    Start(SessionStartArgs),
    /// End the current session.
    End {
        #[arg(long)]
        summary: Option<String>,
    },
    List,
}

#[derive(Args, Debug)]
pub struct SessionStartArgs {
    /// Model vendor (default: GITBOTS_PROVIDER or inferred from the client).
    #[arg(long)]
    pub provider: Option<String>,
    /// Model id (default: GITBOTS_MODEL).
    #[arg(long)]
    pub model: Option<String>,
    /// Harness, e.g. claude-code, codex, chatgpt (default: GITBOTS_CLIENT or detected).
    #[arg(long)]
    pub client: Option<String>,
    /// Parent session, for subagents (default: GITBOTS_SESSION).
    #[arg(long)]
    pub parent: Option<String>,
    #[arg(long)]
    pub role: Option<String>,
    #[arg(long)]
    pub label: Option<String>,
    /// The client's own id for this chat.
    #[arg(long)]
    pub external_id: Option<String>,
    /// Bind the session to this worktree so commits here are attributed to it.
    #[arg(long)]
    pub bind: bool,
}

#[derive(Subcommand, Debug)]
pub enum TaskCmd {
    Create(TaskCreateArgs),
    List,
    Show { task: String },
}

#[derive(Args, Debug)]
pub struct TaskCreateArgs {
    pub title: String,
    #[arg(long)]
    pub body: Option<String>,
    #[arg(long = "label")]
    pub labels: Vec<String>,
    /// Render the task from .gitbots/recipes/<name>.
    #[arg(long)]
    pub recipe: Option<String>,
    /// Recipe input, `key=value`.
    #[arg(long = "input", value_parser = parse_kv)]
    pub inputs: Vec<(String, String)>,
}

#[derive(Subcommand, Debug)]
pub enum AttemptCmd {
    /// Create a branch + workroom for a task.
    Start {
        task: String,
        /// Branch to start from and merge into (default: trusted branch).
        #[arg(long)]
        base: Option<String>,
        /// Bind the workroom to this session instead of the caller's.
        #[arg(long = "for")]
        for_session: Option<String>,
    },
    /// Submit for review; runs `attempt.submitted` actions.
    Submit {
        /// Default: the attempt of the current workroom.
        attempt: Option<String>,
        #[arg(long)]
        summary: Option<String>,
    },
    List,
    Show {
        attempt: String,
    },
    Abandon {
        attempt: String,
        #[arg(long)]
        reason: Option<String>,
        /// Also delete the workroom (the branch is kept).
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Args, Debug)]
#[command(group(clap::ArgGroup::new("target").required(true)))]
pub struct HandoffArgs {
    pub attempt: String,
    #[arg(long, group = "target")]
    pub to_session: Option<String>,
    #[arg(long, group = "target")]
    pub to_role: Option<String>,
    #[arg(long, group = "target")]
    pub to_human: Option<String>,
    #[arg(long)]
    pub note: Option<String>,
}

#[derive(Args, Debug)]
pub struct ReviewArgs {
    pub attempt: String,
    #[arg(value_enum)]
    pub decision: DecisionArg,
    #[arg(long)]
    pub reason: Option<String>,
    /// Merge into the base branch after accepting.
    #[arg(long)]
    pub merge: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum DecisionArg {
    Accept,
    Reject,
    Changes,
}

#[derive(Args, Debug)]
pub struct ReportArgs {
    pub title: String,
    #[arg(long)]
    pub body: Option<String>,
    #[arg(long, value_enum, default_value_t = LevelArg::Info)]
    pub level: LevelArg,
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long)]
    pub attempt: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum LevelArg {
    Info,
    Warning,
    Blocker,
}

#[derive(Args, Debug)]
pub struct TraceArgs {
    pub tool: String,
    #[arg(long)]
    pub input: Option<String>,
    #[arg(long)]
    pub failed: bool,
    #[arg(long)]
    pub duration_ms: Option<u64>,
    /// Attach this file as the call's full log (stored on gitbots/logs).
    #[arg(long)]
    pub log_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct LogArgs {
    /// Event kind or kind prefix (`attempt`, `review.decided`).
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long = "of-session")]
    pub of_session: Option<String>,
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long)]
    pub attempt: Option<String>,
    #[arg(short = 'n', long, default_value_t = 30)]
    pub limit: usize,
}

#[derive(Subcommand, Debug)]
pub enum ActionsCmd {
    List,
    /// Run workflows now.
    Run {
        /// Only this workflow.
        workflow: Option<String>,
        #[arg(long, default_value = "manual")]
        trigger: String,
        /// Run against this attempt's workroom.
        #[arg(long)]
        attempt: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum RecipeCmd {
    List,
}

#[derive(Args, Debug)]
pub struct SyncArgs {
    /// Default: gitbots-main when gitbots cloud is set up, else origin.
    #[arg(long)]
    pub remote: Option<String>,
    /// Also sync gitbots/logs (large; off by default).
    #[arg(long)]
    pub logs: bool,
    #[arg(long)]
    pub no_push: bool,
    /// Keep syncing every --interval seconds (steward mode); network errors
    /// are reported and retried.
    #[arg(long)]
    pub watch: bool,
    /// Seconds between rounds with --watch.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
    pub interval: u64,
}

#[derive(Subcommand, Debug)]
pub enum CloudCmd {
    /// Provision the project on an gitbots Worker (or join or verify it) and push everything.
    Init {
        /// The Worker's base URL.
        #[arg(long)]
        url: String,
        /// File with the deployment's admin key (default: GITBOTS_ADMIN_KEY).
        #[arg(long)]
        admin_key_file: Option<PathBuf>,
    },
    /// Config, project info and pending dashboard decisions.
    Status,
    /// Mint an Artifacts token for the current session (or the human), e.g. for a hosted agent.
    Token {
        /// `main`, `logs` or a fork repo name.
        #[arg(long, default_value = "main")]
        repo: String,
        #[arg(long, value_enum, default_value_t = ScopeArg::Write)]
        scope: ScopeArg,
        /// Lifetime in seconds (default: the Worker's).
        #[arg(long)]
        ttl: Option<u64>,
    },
    /// Create a fork repo for an attempt (for hosted agents); prints its remote and a token.
    Fork { attempt: String },
    /// Print the hosted dashboard link. It contains the owner key: keep it private.
    Dashboard,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ScopeArg {
    Read,
    Write,
}

#[derive(Subcommand, Debug)]
pub enum HookCmd {
    PrepareCommitMsg {
        file: PathBuf,
        #[arg(trailing_var_arg = true)]
        rest: Vec<String>,
    },
    PostCommit {
        #[arg(trailing_var_arg = true)]
        rest: Vec<String>,
    },
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .ok_or_else(|| format!("expected key=value, got `{s}`"))
}

pub async fn run(cli: Cli) -> Result<()> {
    let cwd = match &cli.dir {
        Some(d) => d.clone(),
        None => std::env::current_dir()?,
    };
    let out = Out::new(cli.json);

    if let Command::Init(args) = &cli.command {
        let opts = InitOptions {
            name: args.name.clone(),
            goal: args.goal.clone(),
            autonomy: match args.autonomy {
                AutonomyArg::Supervised => Autonomy::Supervised,
                AutonomyArg::Assisted => Autonomy::Assisted,
                AutonomyArg::Autonomous => Autonomy::Autonomous,
            },
            owner_kind: if args.org { OwnerKind::Org } else { OwnerKind::User },
            hooks: !args.no_hooks,
            gitbots_bin: None,
            commit: args.commit,
        };
        let (_, report) = Project::init(&cwd, opts)?;
        return output::init(&out, &report);
    }

    // Hooks must never block a commit.
    if let Command::Hook(hook) = &cli.command {
        let result = Project::open(&cwd).and_then(|p| match hook {
            HookCmd::PrepareCommitMsg { file, .. } => p.hook_prepare_commit_msg(file),
            HookCmd::PostCommit { .. } => p.hook_post_commit(),
        });
        if let Err(e) = result {
            eprintln!("gitbots hook: {e:#}");
        }
        return Ok(());
    }

    let project = Project::open(&cwd)?;
    if let Command::Sync(a) = &cli.command {
        return sync(&cwd, project, cli.session.as_deref(), a, &out).await;
    }
    if let Command::Cloud(cmd) = &cli.command {
        return cloud_command(&project, cli.session.as_deref(), cmd, &out).await;
    }
    if let Command::Mcp = cli.command {
        return crate::mcp::serve(project, cli.session).await;
    }
    if let Command::Ui { port, assets } = cli.command {
        let root = project.repo().main_workdir()?;
        drop(project);
        return crate::ui::serve(root, crate::ui::UiOptions { port, assets }).await;
    }
    let actor = || project.resolve_actor(cli.session.as_deref());
    // What a mutating command publishes to gitbots cloud afterwards.
    let mut publish: Option<AutoSync> = None;

    let result = match cli.command {
        Command::Init(_)
        | Command::Hook(_)
        | Command::Mcp
        | Command::Ui { .. }
        | Command::Sync(_)
        | Command::Cloud(_) => unreachable!("handled above"),
        Command::Status => output::status(&out, &project, &project.board()?),
        Command::Whoami => output::whoami(&out, &project, &actor()?),
        Command::Inbox => output::inbox(&out, &project.inbox()?),
        Command::Session(cmd) => match cmd {
            SessionCmd::Start(a) => {
                let explicit = [&a.provider, &a.model, &a.client].iter().any(|x| x.is_some());
                let agent = if explicit {
                    let detected = crate::project::detect_agent();
                    let pick = |v: Option<String>, f: fn(&AgentDescriptor) -> String| {
                        v.or_else(|| detected.as_ref().map(f)).unwrap_or_else(|| "unknown".into())
                    };
                    Some(AgentDescriptor::new(
                        pick(a.provider, |d| d.provider.clone()),
                        pick(a.model, |d| d.model.clone()),
                        pick(a.client, |d| d.client.clone()),
                    ))
                } else {
                    None
                };
                let session = project.start_session(StartSession {
                    agent,
                    parent: a.parent,
                    role: a.role,
                    label: a.label,
                    external_id: a.external_id,
                    bind: a.bind,
                    via: None,
                })?;
                let ctx = ActorCtx { actor: session.actor(), via: Via::Flag };
                publish = job(&ctx, vec![]);
                output::session_started(&out, &session, a.bind)
            }
            SessionCmd::End { summary } => {
                let ctx = actor()?;
                let id = project.end_session(&ctx, summary)?;
                publish = job(&ctx, vec![]);
                out.done(&format!("ended {id}"), &serde_json::json!({ "session": id }))
            }
            SessionCmd::List => output::sessions(&out, &project.sessions()?),
        },
        Command::Task(cmd) => match cmd {
            TaskCmd::Create(a) => {
                let ctx = actor()?;
                let id = project.create_task(
                    &ctx,
                    CreateTask {
                        title: a.title,
                        body: a.body,
                        labels: a.labels,
                        recipe: a.recipe,
                        inputs: a.inputs.into_iter().collect::<BTreeMap<_, _>>(),
                    },
                )?;
                publish = job(&ctx, vec![]);
                out.done(
                    &format!("created task {} ({})", id.short(), id),
                    &serde_json::json!({ "task": id }),
                )
            }
            TaskCmd::List => output::tasks(&out, &project.board()?),
            TaskCmd::Show { task } => output::task(&out, &project.board()?, &task),
        },
        Command::Attempt(cmd) => match cmd {
            AttemptCmd::Start { task, base, for_session } => {
                let ctx = actor()?;
                let info =
                    project.start_attempt(&ctx, &task, base.as_deref(), for_session.as_deref())?;
                publish = job(&ctx, vec![info.branch.clone()]);
                output::attempt_started(&out, &info)
            }
            AttemptCmd::Submit { attempt, summary } => {
                let ctx = actor()?;
                let outcome = project.submit_attempt(&ctx, attempt.as_deref(), summary).await?;
                publish = job(&ctx, vec![outcome.branch.clone()]);
                output::submitted(&out, &outcome)
            }
            AttemptCmd::List => output::attempts(&out, &project, &project.board()?),
            AttemptCmd::Show { attempt } => {
                output::attempt(&out, &project, &project.board()?, &attempt)
            }
            AttemptCmd::Abandon { attempt, reason, remove } => {
                let ctx = actor()?;
                project.abandon(&ctx, &attempt, reason, remove)?;
                publish = job(&ctx, vec![]);
                out.done(
                    &format!("abandoned {attempt}"),
                    &serde_json::json!({ "attempt": attempt }),
                )
            }
        },
        Command::Handoff(a) => {
            let to = if let Some(s) = a.to_session {
                // Validated (and resolved from a short id) by the project.
                HandoffTarget::Session { session: project.session(&s)?.id }
            } else if let Some(role) = a.to_role {
                HandoffTarget::Role { role }
            } else if let Some(handle) = a.to_human {
                HandoffTarget::Human { handle: handle.trim_start_matches('@').to_owned() }
            } else {
                bail!("pass --to-session, --to-role or --to-human");
            };
            let ctx = actor()?;
            project.handoff(&ctx, &a.attempt, to, a.note)?;
            publish = job(&ctx, vec![]);
            out.done(
                &format!("handed off {}", a.attempt),
                &serde_json::json!({ "attempt": a.attempt }),
            )
        }
        Command::Review(a) => {
            let decision = match a.decision {
                DecisionArg::Accept => ReviewDecision::Accept,
                DecisionArg::Reject => ReviewDecision::Reject,
                DecisionArg::Changes => ReviewDecision::ChangesRequested,
            };
            let ctx = actor()?;
            let outcome = project.review(&ctx, &a.attempt, decision, a.reason, a.merge)?;
            // A merge into the trusted branch is published too (fast-forward only).
            let mut branches = vec![];
            // Best effort: the review is recorded, so this must not fail the command.
            let into_trusted = || {
                let board = project.board().ok()?;
                Some(board.attempts.get(&outcome.attempt)?.base == project.trusted_branch())
            };
            if outcome.merged.is_some() && into_trusted() == Some(true) {
                branches.push(project.trusted_branch().to_owned());
            }
            publish = job(&ctx, branches);
            let mut msg =
                format!("{:?} {}", outcome.decision, outcome.attempt.short()).to_lowercase();
            if let Some(c) = &outcome.merged {
                msg.push_str(&format!(", merged as {}", short_sha(c)));
            }
            out.done(&msg, &outcome)
        }
        Command::Report(a) => {
            let board = project.board()?;
            let task =
                a.task.as_deref().map(|q| board.find_task(q)).transpose()?.map(|t| t.id.clone());
            let attempt = match a.attempt.as_deref() {
                Some(q) => Some(board.find_attempt(q)?.id.clone()),
                None => project.current_attempt()?.and_then(|s| s.parse().ok()),
            };
            let level = match a.level {
                LevelArg::Info => ReportLevel::Info,
                LevelArg::Warning => ReportLevel::Warning,
                LevelArg::Blocker => ReportLevel::Blocker,
            };
            let ctx = actor()?;
            let id = project
                .report(&ctx, Report { title: a.title, body: a.body, level, task, attempt })?;
            publish = job(&ctx, vec![]);
            out.done("reported", &serde_json::json!({ "event": id }))
        }
        Command::Trace(a) => {
            let ctx = actor()?;
            let log = match &a.log_file {
                Some(f) => Some(project.put_session_log(&ctx, &a.tool, &std::fs::read(f)?)?),
                None => None,
            };
            let input = a.input.map(|i| gitbots_core::redact::redact(&i).0.into_owned());
            let id = project.trace(
                &ctx,
                ToolCalled { tool: a.tool, input, ok: !a.failed, duration_ms: a.duration_ms, log },
            )?;
            publish = job(&ctx, vec![]);
            out.done("traced", &serde_json::json!({ "event": id }))
        }
        Command::Log(a) => {
            let events = project.query_events(&EventFilter {
                kind: a.kind,
                session: a.of_session,
                task: a.task,
                attempt: a.attempt,
                limit: Some(a.limit),
            })?;
            output::events(&out, &events)
        }
        Command::Stats => output::stats(&out, &project.stats()?),
        Command::Actions(cmd) => match cmd {
            ActionsCmd::List => {
                let (ok, bad) = project.workflows()?;
                output::workflows(&out, &ok, &bad)
            }
            ActionsCmd::Run { workflow, trigger, attempt } => {
                let attempt = match attempt {
                    Some(q) => Some(project.board()?.find_attempt(&q)?.id.clone()),
                    None => project.current_attempt()?.and_then(|s| s.parse().ok()),
                };
                let outcome = project
                    .run_actions(&actor()?, &trigger, attempt.as_ref(), workflow.as_deref())
                    .await?;
                output::runs(&out, &outcome.runs, &outcome.invalid_workflows)
            }
        },
        Command::Recipe(RecipeCmd::List) => output::recipes(&out, &project.recipes()?),
        Command::Hooks => {
            let report = project.install_hooks(&std::env::current_exe()?)?;
            output::hooks(&out, &report)
        }
    };
    result?;
    if let Some(job) = &publish {
        cloud::auto_sync(&project, job).await;
    }
    Ok(())
}

fn job(ctx: &ActorCtx, branches: Vec<String>) -> Option<AutoSync> {
    Some(AutoSync { ctx: ctx.clone(), branches })
}

/// `gitbots sync`, once or (`--watch`) forever.
async fn sync(
    cwd: &Path,
    project: Project,
    session: Option<&str>,
    a: &SyncArgs,
    out: &Out,
) -> Result<()> {
    if !a.watch {
        return sync_once(&project, session, a, out).await;
    }
    let out = Out::new(out.is_json()).lines();
    let mut first = Some(project);
    loop {
        // Re-open every round: a merge can change the trusted manifest.
        let round = match first.take() {
            Some(project) => Ok(project),
            None => Project::open(cwd),
        };
        let result = match round {
            Ok(project) => sync_once(&project, session, a, &out).await,
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            eprintln!("gitbots sync: {e:#} (retrying in {}s)", a.interval);
        }
        tokio::time::sleep(Duration::from_secs(a.interval)).await;
    }
}

async fn sync_once(
    project: &Project,
    session: Option<&str>,
    a: &SyncArgs,
    out: &Out,
) -> Result<()> {
    let cloud_config = CloudConfig::load(project.repo())?;
    let default = if cloud_config.is_some() { cloud::MAIN_REMOTE } else { "origin" };
    let remote = a.remote.as_deref().unwrap_or(default);
    if cloud_config.is_some() && remote == cloud::MAIN_REMOTE {
        let cloud = Cloud::open(project)?.context("gitbots cloud is not set up")?;
        let ctx = project.resolve_actor(session)?;
        let report = cloud.sync(&ctx, SyncOptions { logs: a.logs, push: !a.no_push }).await?;
        return output::cloud_sync(out, &report);
    }
    let reports = project.sync(remote, a.logs, !a.no_push)?;
    output::sync(out, &reports)
}

async fn cloud_command(
    project: &Project,
    session: Option<&str>,
    cmd: &CloudCmd,
    out: &Out,
) -> Result<()> {
    let configured = || -> Result<Cloud<'_>> {
        Cloud::open(project)?
            .context("gitbots cloud is not set up here: `gitbots cloud init --url <worker-url>`")
    };
    match cmd {
        CloudCmd::Init { url, admin_key_file } => {
            // It provisions and publishes everything with the human's token.
            let ctx = project.resolve_actor(session)?;
            if let Some(why) = cloud::not_the_human(&ctx) {
                bail!("`gitbots cloud init` acts as the human: {why}");
            }
            let admin_key = match admin_key_file {
                Some(path) => Some(
                    std::fs::read_to_string(path)
                        .with_context(|| format!("reading {}", path.display()))?
                        .trim()
                        .to_owned(),
                ),
                None => std::env::var(cloud::ADMIN_KEY_ENV).ok(),
            }
            .filter(|k| !k.trim().is_empty());
            let report = cloud::init(project, url, admin_key).await?;
            output::cloud_init(out, &report)
        }
        CloudCmd::Status => output::cloud_status(out, &cloud::status(project).await?),
        CloudCmd::Token { repo, scope, ttl } => {
            let cloud = configured()?;
            let ctx = project.resolve_actor(session)?;
            let scope = match scope {
                ScopeArg::Read => TokenScope::Read,
                ScopeArg::Write => TokenScope::Write,
            };
            let issued =
                cloud.mint(cloud::parse_repo(repo), scope, *ttl, ctx.actor.session()).await?;
            output::cloud_token(out, &issued)
        }
        CloudCmd::Fork { attempt } => {
            let cloud = configured()?;
            let ctx = project.resolve_actor(session)?;
            let report = cloud.fork(attempt, ctx.actor.session()).await?;
            output::cloud_fork(out, &report)
        }
        CloudCmd::Dashboard => output::cloud_dashboard(out, &configured()?.dashboard_link()),
    }
}

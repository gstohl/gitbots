//! gitbots on Cloudflare (`docs/CLOUD.md`): Artifacts repos as git remotes and
//! the gitbots Worker as the control plane.
//!
//! - **Config.** Local git config holds `gitbots.cloud.url` (the Worker),
//!   `gitbots.cloud.project` and two plain remotes, `gitbots-main` (`<prj>`: code
//!   and `gitbots/activity`) and `gitbots-logs` (`<prj>-logs`: `gitbots/logs`). The
//!   owner key is in `<config dir>/credentials.toml` (0600) under
//!   `<url>#<project>`, or in `GITBOTS_CLOUD_KEY`. The config dir is
//!   `GITBOTS_CONFIG_DIR`, else `$XDG_CONFIG_HOME/gitbots`, else `~/.config/gitbots`.
//! - **Tokens.** Artifacts tokens are per repo and per actor: an agent
//!   session's token is minted with its `session` id, which is what makes its
//!   pushes attested; the human's is minted without. Every push uses the
//!   token of the actor it runs as. Tokens are cached in
//!   `<git common dir>/gitbots/tokens/<repo>-<session|human>.json` (0600) until
//!   60 s before they expire, and reach git only as
//!   `-c http.extraHeader=...` on the command line.
//! - **Sync.** `gitbots sync` union-merges the ledgers with the hosted repos,
//!   pushes the trusted and attempt branches (never forced), applies the
//!   dashboard's outbox as the human (the steward) and asks the Worker to
//!   re-index. Mutating commands run a quick activity-only sync afterwards
//!   (`gitbots.cloud.autoSync`, default on).
//! - **Forks.** `gitbots cloud fork <attempt>` creates `<prj>-<att>` and adds it
//!   as the plain remote `gitbots-fork-<attempt short id>`. Working in a fork is
//!   manual for now: `git -c http.extraHeader="Authorization: Bearer $TOKEN"
//!   push gitbots-fork-<id> <branch>`, with the token it printed (or a new one
//!   from `gitbots cloud token --repo <fork repo>`).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use gitbots_cloud::api::{
    ApiError, CreateProject, ForkCreated, ForkRequest, IngestReport, KnownRepo, OutboxAck,
    OutboxAction, OutboxItem, ProjectInfo, Remotes, RepoRef, TokenIssued, TokenRequest, TokenScope,
};
use gitbots_core::event::{ReviewDecision, short_sha};
use gitbots_core::{AttemptId, EventId, SessionId, Via};
use gitbots_git::{BranchPush, PushStatus, RemoteSpec, Repo, SyncReport};

use crate::project::{ActorCtx, CreateTask, PRODUCER, Project};

pub const MAIN_REMOTE: &str = "gitbots-main";
pub const LOGS_REMOTE: &str = "gitbots-logs";
/// The owner key, overriding `credentials.toml`.
pub const KEY_ENV: &str = "GITBOTS_CLOUD_KEY";
/// The deployment's admin key, needed once to provision a project.
pub const ADMIN_KEY_ENV: &str = "GITBOTS_ADMIN_KEY";
const URL_CONFIG: &str = "gitbots.cloud.url";
const PROJECT_CONFIG: &str = "gitbots.cloud.project";
const AUTOSYNC_CONFIG: &str = "gitbots.cloud.autoSync";
const CREDENTIALS_FILE: &str = "credentials.toml";
/// Reuse a cached token until it is this close to expiring.
const TOKEN_SLACK_SECS: i64 = 60;

// ---- config ---------------------------------------------------------------

/// Where this repo's project lives in the cloud (local git config).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CloudConfig {
    /// The Worker's base URL, without a trailing slash.
    pub url: String,
    pub project: String,
}

impl CloudConfig {
    /// `None` unless both `gitbots.cloud.url` and `gitbots.cloud.project` are set.
    pub fn load(repo: &Repo) -> Result<Option<CloudConfig>> {
        let url = repo.config_get(URL_CONFIG)?.filter(|u| !u.trim().is_empty());
        let project = repo.config_get(PROJECT_CONFIG)?.filter(|p| !p.trim().is_empty());
        Ok(match (url, project) {
            (Some(url), Some(project)) => {
                Some(CloudConfig { url: normalize_url(&url)?, project: project.trim().to_owned() })
            }
            _ => None,
        })
    }

    fn save(&self, repo: &Repo) -> Result<()> {
        repo.config_set_local(URL_CONFIG, &self.url)?;
        repo.config_set_local(PROJECT_CONFIG, &self.project)
    }

    /// The key of this project's entry in `credentials.toml`.
    pub fn credential_id(&self) -> String {
        format!("{}#{}", self.url, self.project)
    }

    /// The hosted dashboard, signed in with the owner key.
    pub fn dashboard_link(&self, owner_key: &str) -> String {
        format!("{}/#token={owner_key}", self.url)
    }
}

pub fn normalize_url(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    ensure!(
        url.starts_with("https://") || url.starts_with("http://"),
        "the Worker URL must start with https:// (or http:// for local development), got `{url}`"
    );
    Ok(url.to_owned())
}

/// `gitbots.cloud.autoSync` (default true).
pub fn auto_sync_enabled(repo: &Repo) -> Result<bool> {
    let value = repo.config_get(AUTOSYNC_CONFIG)?.map(|v| v.trim().to_ascii_lowercase());
    Ok(!matches!(value.as_deref(), Some("false" | "no" | "off" | "0")))
}

/// `GITBOTS_CONFIG_DIR`, else `$XDG_CONFIG_HOME/gitbots`, else `~/.config/gitbots`.
pub fn config_dir() -> Result<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = var("GITBOTS_CONFIG_DIR") {
        return Ok(dir);
    }
    if let Some(dir) = var("XDG_CONFIG_HOME") {
        return Ok(dir.join("gitbots"));
    }
    let home = std::env::home_dir().context("no home directory; set GITBOTS_CONFIG_DIR")?;
    Ok(home.join(".config").join("gitbots"))
}

pub fn credentials_path() -> Result<PathBuf> {
    Ok(config_dir()?.join(CREDENTIALS_FILE))
}

/// Where the owner key came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum KeySource {
    Env,
    File { path: PathBuf },
}

impl fmt::Display for KeySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeySource::Env => write!(f, "${KEY_ENV}"),
            KeySource::File { path } => write!(f, "{}", path.display()),
        }
    }
}

/// The project's owner key. Never printed except in the dashboard link.
#[derive(Clone)]
pub struct OwnerKey {
    pub key: String,
    pub source: KeySource,
}

impl fmt::Debug for OwnerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnerKey").field("source", &self.source).finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
struct Credential {
    owner_key: String,
}

type Credentials = BTreeMap<String, Credential>;

fn read_credentials(path: &Path) -> Result<Credentials> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Credentials::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// `GITBOTS_CLOUD_KEY`, else this project's entry in `credentials.toml`.
pub fn owner_key(config: &CloudConfig) -> Result<Option<OwnerKey>> {
    if let Some(key) = std::env::var(KEY_ENV).ok().map(|k| k.trim().to_owned())
        && !key.is_empty()
    {
        return Ok(Some(OwnerKey { key, source: KeySource::Env }));
    }
    let path = credentials_path()?;
    Ok(read_credentials(&path)?
        .remove(&config.credential_id())
        .map(|c| OwnerKey { key: c.owner_key, source: KeySource::File { path } }))
}

fn store_owner_key(config: &CloudConfig, key: &str) -> Result<PathBuf> {
    let path = credentials_path()?;
    let mut credentials = read_credentials(&path)?;
    credentials.insert(config.credential_id(), Credential { owner_key: key.to_owned() });
    let text = format!(
        "# gitbots cloud owner keys, by `<worker url>#<project id>`. Keep this file private.\n\n{}",
        toml::to_string(&credentials)?
    );
    write_private(&path, text.as_bytes())?;
    Ok(path)
}

/// Writes a file only its owner can read (0600 on unix), atomically.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("a private file needs a parent directory")?;
    if !dir.is_dir() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let nanos = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let name = path.file_name().map_or("file".into(), |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&tmp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.with_context(|| format!("writing {}", path.display()))
}

/// Adds or re-points a plain git remote (a URL, never a token).
fn set_remote(repo: &Repo, name: &str, url: &str) -> Result<()> {
    match repo.config_get(&format!("remote.{name}.url"))? {
        Some(current) if current == url => Ok(()),
        Some(_) => repo.git(&["remote", "set-url", name, url]).map(drop),
        None => repo.git(&["remote", "add", name, url]).map(drop),
    }
}

// ---- the /v1 client -------------------------------------------------------

/// A non-2xx answer from the control plane: its status and `{error}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudError {
    pub status: u16,
    pub message: String,
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (HTTP {})", self.message, self.status)
    }
}

impl std::error::Error for CloudError {}

/// The HTTP status of a [`CloudError`] anywhere in `err`'s chain.
pub fn error_status(err: &anyhow::Error) -> Option<u16> {
    err.downcast_ref::<CloudError>().map(|e| e.status)
}

/// An async client for the Worker's `/v1` API, authenticated with one key
/// (the owner key, or the admin key for [`Client::create_project`]).
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    key: String,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").field("base", &self.base).finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(base: &str, key: &str) -> Result<Client> {
        let http = reqwest::Client::builder()
            .user_agent(PRODUCER)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(60))
            .build()
            .context("building the HTTP client")?;
        Ok(Client { http, base: normalize_url(base)?, key: key.to_owned() })
    }

    async fn send<T: DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
        what: String,
    ) -> Result<T> {
        let resp =
            req.bearer_auth(&self.key).send().await.with_context(|| {
                format!("{what}: can't reach the gitbots Worker at {}", self.base)
            })?;
        let status = resp.status();
        let bytes = resp.bytes().await.with_context(|| format!("{what}: reading the response"))?;
        if !status.is_success() {
            let message = serde_json::from_slice::<ApiError>(&bytes).map_or_else(
                |_| {
                    let text = String::from_utf8_lossy(&bytes);
                    let text: String = text.trim().chars().take(200).collect();
                    if text.is_empty() { status.to_string() } else { text }
                },
                |e| e.error,
            );
            return Err(
                anyhow::Error::new(CloudError { status: status.as_u16(), message }).context(what)
            );
        }
        let body: &[u8] = if bytes.iter().all(u8::is_ascii_whitespace) { b"null" } else { &bytes };
        serde_json::from_slice(body).with_context(|| format!("{what}: unexpected response"))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(self.http.get(format!("{}{path}", self.base)), format!("GET {path}")).await
    }

    async fn post<T: DeserializeOwned>(&self, path: &str, body: &impl Serialize) -> Result<T> {
        let req = self.http.post(format!("{}{path}", self.base)).json(body);
        self.send(req, format!("POST {path}")).await
    }

    /// `POST /v1/projects`; the client must hold the admin key.
    pub async fn create_project(
        &self,
        req: &CreateProject,
    ) -> Result<gitbots_cloud::api::ProjectCreated> {
        self.post("/v1/projects", req).await
    }

    pub async fn project(&self) -> Result<ProjectInfo> {
        self.get("/v1/project").await
    }

    pub async fn token(&self, req: &TokenRequest) -> Result<TokenIssued> {
        self.post("/v1/tokens", req).await
    }

    pub async fn fork(&self, req: &ForkRequest) -> Result<ForkCreated> {
        self.post("/v1/forks", req).await
    }

    pub async fn ingest(&self) -> Result<IngestReport> {
        self.post("/v1/ingest", &serde_json::json!({})).await
    }

    /// Pending outbox items, unparsed: one item of a kind this build doesn't
    /// know must not hide the others.
    pub async fn outbox(&self) -> Result<Vec<Value>> {
        self.get("/v1/outbox").await
    }

    pub async fn ack(&self, id: &str, ack: &OutboxAck) -> Result<()> {
        ensure!(valid_id(id), "refusing to ack outbox item with odd id {id:?}");
        let _: Value = self.post(&format!("/v1/outbox/{id}/ack"), ack).await?;
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

// ---- tokens ---------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct CachedToken {
    token: String,
    expires_at: String,
    remote: String,
    scope: TokenScope,
}

/// Whether a token expiring at `expires_at` (RFC 3339) is still worth using.
fn fresh(expires_at: &str, now: OffsetDateTime) -> bool {
    OffsetDateTime::parse(expires_at, &Rfc3339)
        .is_ok_and(|t| (t - now).whole_seconds() > TOKEN_SLACK_SECS)
}

fn repo_label(repo: &RepoRef) -> String {
    match repo {
        RepoRef::Known(KnownRepo::Main) => "main".into(),
        RepoRef::Known(KnownRepo::Logs) => "logs".into(),
        RepoRef::Fork(name) => gitbots_core::ledger::sanitize_segment(name),
    }
}

/// `main`, `logs`, or a fork repo name.
pub fn parse_repo(s: &str) -> RepoRef {
    match s {
        "main" => RepoRef::Known(KnownRepo::Main),
        "logs" => RepoRef::Known(KnownRepo::Logs),
        fork => RepoRef::Fork(fork.to_owned()),
    }
}

// ---- the project in the cloud ----------------------------------------------

/// A configured project: its config, owner key and client.
pub struct Cloud<'p> {
    project: &'p Project,
    pub config: CloudConfig,
    pub key: OwnerKey,
    client: Client,
}

impl fmt::Debug for Cloud<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cloud").field("config", &self.config).finish_non_exhaustive()
    }
}

impl<'p> Cloud<'p> {
    /// `None` if the repo isn't set up for the cloud; an error if it is but
    /// there is no owner key here.
    pub fn open(project: &'p Project) -> Result<Option<Cloud<'p>>> {
        match CloudConfig::load(project.repo())? {
            Some(config) => Self::from_config(project, config).map(Some),
            None => Ok(None),
        }
    }

    fn from_config(project: &'p Project, config: CloudConfig) -> Result<Cloud<'p>> {
        let key = owner_key(&config)?.with_context(|| {
            format!(
                "no owner key for {} at {}: set {KEY_ENV} or add it to {}",
                config.project,
                config.url,
                credentials_path().map(|p| p.display().to_string()).unwrap_or_default()
            )
        })?;
        Self::with_key(project, config, key)
    }

    fn with_key(project: &'p Project, config: CloudConfig, key: OwnerKey) -> Result<Cloud<'p>> {
        let client = Client::new(&config.url, &key.key)?;
        Ok(Cloud { project, config, key, client })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn dashboard_link(&self) -> String {
        self.config.dashboard_link(&self.key.key)
    }

    /// `<git common dir>/gitbots/tokens/<repo>-<session|human>.json`.
    pub fn token_cache_path(&self, repo: &RepoRef, session: Option<&SessionId>) -> PathBuf {
        let who = session.map_or("human", SessionId::as_str);
        self.project
            .repo()
            .common_dir()
            .join("gitbots")
            .join("tokens")
            .join(format!("{}-{who}.json", repo_label(repo)))
    }

    fn cache_token(&self, path: &Path, issued: &TokenIssued, scope: TokenScope) -> Result<()> {
        let cached = CachedToken {
            token: issued.token.clone(),
            expires_at: issued.expires_at.clone(),
            remote: issued.remote.clone(),
            scope,
        };
        write_private(path, &serde_json::to_vec_pretty(&cached)?)
    }

    /// A write token for `repo`, minted for `session` (an agent) or the
    /// human (`None`), reused from the cache until 60 s before it expires.
    pub async fn token(&self, repo: &RepoRef, session: Option<&SessionId>) -> Result<TokenIssued> {
        let path = self.token_cache_path(repo, session);
        if let Some(cached) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CachedToken>(&bytes).ok())
            && cached.scope == TokenScope::Write
            && fresh(&cached.expires_at, OffsetDateTime::now_utc())
        {
            return Ok(TokenIssued {
                token: cached.token,
                expires_at: cached.expires_at,
                remote: cached.remote,
            });
        }
        let issued = self.mint(repo.clone(), TokenScope::Write, None, session).await?;
        self.cache_token(&path, &issued, TokenScope::Write)?;
        Ok(issued)
    }

    /// A new token, not cached (for handing to a hosted agent).
    pub async fn mint(
        &self,
        repo: RepoRef,
        scope: TokenScope,
        ttl_secs: Option<u64>,
        session: Option<&SessionId>,
    ) -> Result<TokenIssued> {
        let req = TokenRequest { repo, scope, session: session.map(|s| s.to_string()), ttl_secs };
        let issued = self.client.token(&req).await?;
        ensure!(!issued.token.is_empty(), "the Worker issued an empty token");
        Ok(issued)
    }

    /// Drops cached tokens of `session` (after git refused one, say).
    pub fn forget_tokens(&self, session: Option<&SessionId>) {
        for repo in [KnownRepo::Main, KnownRepo::Logs] {
            let _ = std::fs::remove_file(self.token_cache_path(&RepoRef::Known(repo), session));
        }
    }

    /// `gitbots-main` or `gitbots-logs`, authenticated as `session` (or the human).
    pub async fn remote(
        &self,
        which: KnownRepo,
        session: Option<&SessionId>,
    ) -> Result<RemoteSpec> {
        let token = self.token(&RepoRef::Known(which), session).await?;
        let name = match which {
            KnownRepo::Main => MAIN_REMOTE,
            KnownRepo::Logs => LOGS_REMOTE,
        };
        Ok(RemoteSpec::new(name)
            .with_bearer(&token.token)
            // Give up on a stalled transfer rather than hang a steward.
            .with_config("http.lowSpeedLimit", "1000")
            .with_config("http.lowSpeedTime", "60"))
    }

    /// The trusted branch (if it exists) and every attempt branch.
    fn code_branches(&self) -> Result<Vec<String>> {
        let repo = self.project.repo();
        let mut branches = Vec::new();
        let trusted = self.project.trusted_branch();
        if repo.branch_exists(trusted)? {
            branches.push(trusted.to_owned());
        }
        let prefix = &self.project.manifest().workrooms.branch_prefix;
        branches.extend(repo.branches_under(prefix)?.into_iter().filter(|b| b != trusted));
        Ok(branches)
    }

    fn push(&self, remote: &RemoteSpec, branches: &[String]) -> Result<Vec<BranchPush>> {
        let refs: Vec<&str> = branches.iter().map(String::as_str).collect();
        gitbots_git::push_branches(self.project.repo(), remote, &refs)
    }

    // ---- sync -------------------------------------------------------------

    /// `gitbots sync` against the hosted repos, as `ctx`.
    pub async fn sync(&self, ctx: &ActorCtx, opts: SyncOptions) -> Result<CloudSyncReport> {
        let report = self.sync_as(ctx, opts).await;
        self.after_failure(report, ctx.actor.session())
    }

    /// A failed git exchange may mean a revoked token: drop the cached ones
    /// so the next attempt mints fresh tokens.
    fn after_failure<T>(&self, result: Result<T>, session: Option<&SessionId>) -> Result<T> {
        let err = match result {
            Ok(value) => return Ok(value),
            Err(err) => err,
        };
        self.forget_tokens(session);
        let text = format!("{err:#}");
        let refused =
            ["could not read Username", "Authentication failed", "error: 401", "error: 403"]
                .iter()
                .any(|needle| text.contains(needle));
        Err(match refused {
            true => err.context("the hosted repo refused the token; it was dropped and the next run mints a new one"),
            false => err,
        })
    }

    async fn sync_as(&self, ctx: &ActorCtx, opts: SyncOptions) -> Result<CloudSyncReport> {
        let session = ctx.actor.session();
        let mut report = CloudSyncReport { actor: ctx.actor.label(), ..Default::default() };
        let main = self.remote(KnownRepo::Main, session).await?;
        let logs = match opts.logs {
            true => Some(self.remote(KnownRepo::Logs, session).await?),
            false => None,
        };
        let ledger = &self.project.manifest().ledger;
        let mut plan = vec![(ledger.activity_branch.as_str(), &main)];
        if let Some(logs) = &logs {
            plan.push((ledger.logs_branch.as_str(), logs));
        }
        report.ledgers = gitbots_git::sync_with(self.project.repo(), &plan, opts.push)?;
        if !opts.push {
            report.steward_skipped = Some("--no-push".into());
            return Ok(report);
        }
        report.branches = self.push(&main, &self.code_branches()?)?;
        if ctx.actor.is_human() {
            match self.steward(&main).await {
                Ok(steward) => report.steward = Some(steward),
                Err(e) => report.warnings.push(format!("outbox not applied: {e:#}")),
            }
        } else {
            report.steward_skipped =
                Some("the outbox is applied by the human's `gitbots sync`, not an agent's".into());
        }
        match self.client.ingest().await {
            Ok(ingest) => report.ingest = Some(ingest),
            Err(e) => report.warnings.push(format!("ingest: {e:#}")),
        }
        Ok(report)
    }

    /// After a mutating command: sync `gitbots/activity` and push `branches`
    /// as `session`.
    async fn quick_sync(
        &self,
        session: Option<&SessionId>,
        branches: &[String],
    ) -> Result<Vec<BranchPush>> {
        let main = self.remote(KnownRepo::Main, session).await?;
        let activity = &self.project.manifest().ledger.activity_branch;
        gitbots_git::sync_with(self.project.repo(), &[(activity.as_str(), &main)], true)?;
        self.push(&main, branches)
    }

    // ---- steward ----------------------------------------------------------

    /// Applies the dashboard's outbox as the human (`via: ui`), publishes
    /// the result and acks each item. Items are recorded under the
    /// idempotency key `outbox:<id>`, so an item applied but not acked (the
    /// push or the ack failed) is acked, not applied again, next time.
    async fn steward(&self, main: &RemoteSpec) -> Result<StewardReport> {
        let pending = self.client.outbox().await?;
        let mut report = StewardReport::default();
        if pending.is_empty() {
            return Ok(report);
        }
        let human = self.project.human_via(Via::Ui)?;
        let mut done: HashMap<String, EventId> = self
            .project
            .events()?
            .into_iter()
            .filter_map(|e| Some((e.idem.filter(|k| k.starts_with("outbox:"))?, e.id)))
            .collect();
        for value in pending {
            let outcome = self.apply_item(&human, value, main, &mut done);
            report.items.push(outcome);
        }

        // Publish before acking: an acked decision must already be on gitbots-main.
        let mut published = Ok(());
        if report.items.iter().any(|i| i.event.is_some()) {
            let activity = &self.project.manifest().ledger.activity_branch;
            published =
                gitbots_git::sync_with(self.project.repo(), &[(activity.as_str(), main)], true)
                    .map(drop);
        }
        if published.is_ok() && report.items.iter().any(|i| i.merged.is_some()) {
            let trusted = self.project.trusted_branch().to_owned();
            match self.push(main, &[trusted]) {
                Ok(pushes) => report.trusted = pushes.into_iter().next(),
                Err(e) => report.warnings.push(format!("pushing the merge: {e:#}")),
            }
        }
        if let Err(e) = &published {
            report
                .warnings
                .push(format!("applied items stay pending until they are on {MAIN_REMOTE}: {e:#}"));
        }
        for item in &mut report.items {
            if !valid_id(&item.id) || (item.error.is_none() && published.is_err()) {
                continue;
            }
            let ack = OutboxAck {
                event: item.event.as_ref().map(ToString::to_string),
                error: item.error.clone(),
            };
            match self.client.ack(&item.id, &ack).await {
                Ok(()) => item.acked = true,
                Err(e) => report.warnings.push(format!("ack {}: {e:#}", item.id)),
            }
        }
        Ok(report)
    }

    fn apply_item(
        &self,
        human: &ActorCtx,
        value: Value,
        main: &RemoteSpec,
        done: &mut HashMap<String, EventId>,
    ) -> OutboxOutcome {
        let text = |k: &str| value.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let mut outcome = OutboxOutcome {
            id: text("id"),
            kind: text("kind"),
            event: None,
            error: None,
            summary: String::new(),
            merged: None,
            acked: false,
        };
        if !valid_id(&outcome.id) {
            outcome.error = Some("outbox item without a usable id".into());
            return outcome;
        }
        let idem = format!("outbox:{}", outcome.id);
        if let Some(event) = done.get(&idem) {
            outcome.summary = "already applied".into();
            outcome.event = Some(event.clone());
            return outcome;
        }
        let applied = serde_json::from_value::<OutboxItem>(value)
            .map_err(|e| anyhow!("unsupported outbox item: {e}"))
            .and_then(|item| self.apply(human, &item, &idem, main));
        match applied {
            Ok(applied) => {
                outcome.event = Some(applied.event.clone());
                outcome.summary = applied.summary;
                outcome.merged = applied.merged;
                done.insert(idem, applied.event);
            }
            Err(e) => {
                outcome.error = Some(format!("{e:#}"));
                // A review whose merge failed is still decided: ack its event
                // together with the error.
                outcome.event = self.project.event_with_idem(&idem).ok().flatten();
            }
        }
        outcome
    }

    fn apply(
        &self,
        human: &ActorCtx,
        item: &OutboxItem,
        idem: &str,
        main: &RemoteSpec,
    ) -> Result<Applied> {
        ensure!(
            item.actor.is_human(),
            "outbox items must come from a human, not {}",
            item.actor.label()
        );
        match &item.action {
            OutboxAction::TaskCreate(task) => {
                let title = task.title.trim();
                ensure!(!title.is_empty(), "title is required");
                let req = CreateTask {
                    title: title.to_owned(),
                    body: task.body.clone().filter(|b| !b.trim().is_empty()),
                    labels: task.labels.clone(),
                    ..Default::default()
                };
                let (task, event) = self.project.create_task_once(human, req, idem)?;
                Ok(Applied { event, summary: format!("created task {task}"), merged: None })
            }
            OutboxAction::Review(review) => {
                let attempt = review.attempt.as_deref().context("review item names no attempt")?;
                if review.merge {
                    self.ensure_attempt_head(attempt, main)?;
                }
                let reason = review.reason.clone().filter(|r| !r.trim().is_empty());
                let outcome = self.project.review_once(
                    human,
                    attempt,
                    review.decision,
                    reason,
                    review.merge,
                    idem,
                )?;
                let mut summary =
                    format!("{} {}", decision_label(outcome.decision), outcome.attempt);
                if let Some(commit) = &outcome.merged {
                    summary.push_str(&format!(", merged as {}", short_sha(commit)));
                }
                Ok(Applied { event: outcome.event, summary, merged: outcome.merged })
            }
        }
    }

    /// Merging needs the attempt's head here; an agent on another machine
    /// only pushed it to `gitbots-main`.
    fn ensure_attempt_head(&self, attempt: &str, main: &RemoteSpec) -> Result<()> {
        let repo = self.project.repo();
        let board = self.project.board()?;
        let view = board.find_attempt(attempt)?;
        let Some(head) = &view.head else { return Ok(()) };
        let present = |head: &str| matches!(repo.resolve(head), Ok(Some(_)));
        if !present(head) {
            gitbots_git::fetch_branch(repo, main, &view.branch)?;
            ensure!(
                present(head),
                "attempt head {} is neither here nor on {}",
                short_sha(head),
                main.name()
            );
        }
        Ok(())
    }

    // ---- commands ---------------------------------------------------------

    /// `gitbots cloud fork`: a fork repo for `attempt`, with a write token for
    /// `session` (or the human), added as the plain remote `gitbots-fork-<id>`.
    pub async fn fork(&self, attempt: &str, session: Option<&SessionId>) -> Result<ForkReport> {
        let attempt = self.project.board()?.find_attempt(attempt)?.id.clone();
        let req =
            ForkRequest { attempt: attempt.to_string(), session: session.map(|s| s.to_string()) };
        let fork = self.client.fork(&req).await?;
        let git_remote = format!("gitbots-fork-{}", attempt.short());
        set_remote(self.project.repo(), &git_remote, &fork.remote)?;
        let issued = TokenIssued {
            token: fork.token.clone(),
            expires_at: fork.expires_at.clone(),
            remote: fork.remote.clone(),
        };
        let path = self.token_cache_path(&RepoRef::Fork(fork.repo.clone()), session);
        self.cache_token(&path, &issued, TokenScope::Write)?;
        Ok(ForkReport { attempt, git_remote, fork })
    }
}

fn decision_label(d: ReviewDecision) -> &'static str {
    match d {
        ReviewDecision::Accept => "accepted",
        ReviewDecision::Reject => "rejected",
        ReviewDecision::ChangesRequested => "requested changes on",
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SyncOptions {
    /// Also sync `gitbots/logs` with `gitbots-logs`.
    pub logs: bool,
    pub push: bool,
}

#[derive(Debug, Default)]
pub struct CloudSyncReport {
    /// Who the sync ran as (whose tokens it used).
    pub actor: String,
    pub ledgers: Vec<SyncReport>,
    /// The trusted branch and the attempt branches.
    pub branches: Vec<BranchPush>,
    pub steward: Option<StewardReport>,
    pub steward_skipped: Option<String>,
    pub ingest: Option<IngestReport>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Default)]
pub struct StewardReport {
    pub items: Vec<OutboxOutcome>,
    /// The trusted-branch push after a merge.
    pub trusted: Option<BranchPush>,
    pub warnings: Vec<String>,
}

/// What became of one outbox item.
#[derive(Clone, Debug, Serialize)]
pub struct OutboxOutcome {
    pub id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged: Option<String>,
    /// `false` leaves the item pending; the next sync acks it.
    pub acked: bool,
}

struct Applied {
    event: EventId,
    summary: String,
    merged: Option<String>,
}

#[derive(Debug)]
pub struct ForkReport {
    pub attempt: AttemptId,
    pub git_remote: String,
    pub fork: ForkCreated,
}

// ---- auto-sync ------------------------------------------------------------

/// What a mutating command publishes afterwards.
#[derive(Clone, Debug)]
pub struct AutoSync {
    /// Whose token the push uses.
    pub ctx: ActorCtx,
    /// Code branches to push besides `gitbots/activity` (the attempt branch).
    pub branches: Vec<String>,
}

/// Runs [`AutoSync`] if the repo is set up for the cloud and
/// `gitbots.cloud.autoSync` isn't false. Never fails: problems are warnings on
/// stderr, and the next `gitbots sync` catches up.
pub async fn auto_sync(project: &Project, job: &AutoSync) {
    if let Err(e) = try_auto_sync(project, job).await {
        eprintln!(
            "gitbots: warning: auto-sync with gitbots cloud failed (the command itself succeeded; \
             `gitbots sync` will catch up): {e:#}"
        );
    }
}

async fn try_auto_sync(project: &Project, job: &AutoSync) -> Result<()> {
    let Some(config) = CloudConfig::load(project.repo())? else { return Ok(()) };
    if !auto_sync_enabled(project.repo())? {
        return Ok(());
    }
    let cloud = Cloud::from_config(project, config)?;
    let session = job.ctx.actor.session();
    let pushed = cloud.quick_sync(session, &job.branches).await;
    for push in cloud.after_failure(pushed, session)? {
        if let PushStatus::Rejected { reason } = &push.status {
            eprintln!("gitbots: warning: auto-sync did not push {}: {reason}", push.branch);
        }
    }
    Ok(())
}

// ---- init and status --------------------------------------------------------

#[derive(Debug)]
pub struct InitReport {
    pub config: CloudConfig,
    /// `POST /v1/projects` ran; otherwise an existing project was joined.
    pub created: bool,
    /// Already set up: only verified with `GET /v1/project`.
    pub verified_only: bool,
    pub namespace: String,
    pub remotes: Remotes,
    pub key_source: KeySource,
    pub ledgers: Vec<SyncReport>,
    pub branches: Vec<BranchPush>,
    pub ingest: Option<IngestReport>,
    pub warnings: Vec<String>,
    pub dashboard: String,
}

/// `gitbots cloud init`: provision the project (or join or verify it), store
/// the owner key and config, add the remotes and publish everything as the
/// human.
pub async fn init(project: &Project, url: &str, admin_key: Option<String>) -> Result<InitReport> {
    let repo = project.repo();
    let manifest = project.manifest();
    let config = CloudConfig { url: normalize_url(url)?, project: manifest.project.id.to_string() };
    let existing = CloudConfig::load(repo)?;
    if let Some(existing) = &existing {
        ensure!(
            existing == &config,
            "this repo is already set up for {} at {}; `git config --unset {URL_CONFIG}` to move it",
            existing.project,
            existing.url
        );
    }

    let (key, namespace, remotes, created) = match owner_key(&config)? {
        Some(key) => {
            let info = Client::new(&config.url, &key.key)?
                .project()
                .await
                .context("checking the project with the gitbots Worker")?;
            ensure!(
                info.project_id == config.project,
                "the owner key belongs to {}, not {}",
                info.project_id,
                config.project
            );
            (key, info.namespace, info.remotes, false)
        }
        None => {
            let admin = admin_key.with_context(|| {
                format!(
                    "provisioning a project needs the deployment's admin key: set {ADMIN_KEY_ENV} \
                     or pass --admin-key-file (or set {KEY_ENV} to join an existing project)"
                )
            })?;
            let req = CreateProject {
                project_id: config.project.clone(),
                name: manifest.project.name.clone(),
            };
            let created =
                Client::new(&config.url, &admin)?.create_project(&req).await.map_err(|e| {
                    if error_status(&e) == Some(409) {
                        e.context(format!(
                            "{} is already provisioned: set {KEY_ENV} to its owner key to join it",
                            config.project
                        ))
                    } else {
                        e
                    }
                })?;
            // The owner key is shown once: keep it before anything else can fail.
            let path = store_owner_key(&config, &created.owner_key)?;
            let key = OwnerKey { key: created.owner_key, source: KeySource::File { path } };
            (key, created.namespace, created.remotes, true)
        }
    };

    config.save(repo)?;
    set_remote(repo, MAIN_REMOTE, &remotes.main)?;
    set_remote(repo, LOGS_REMOTE, &remotes.logs)?;
    let verified_only = existing.is_some() && !created;
    let cloud = Cloud::with_key(project, config.clone(), key)?;
    let mut report = InitReport {
        dashboard: cloud.dashboard_link(),
        key_source: cloud.key.source.clone(),
        config,
        created,
        verified_only,
        namespace,
        remotes,
        ledgers: vec![],
        branches: vec![],
        ingest: None,
        warnings: vec![],
    };
    if verified_only {
        return Ok(report);
    }

    let main = cloud.remote(KnownRepo::Main, None).await?;
    let logs = cloud.remote(KnownRepo::Logs, None).await?;
    let ledger = &manifest.ledger;
    report.ledgers = gitbots_git::sync_with(
        repo,
        &[(ledger.activity_branch.as_str(), &main), (ledger.logs_branch.as_str(), &logs)],
        true,
    )?;
    report.branches = cloud.push(&main, &cloud.code_branches()?)?;
    match cloud.client.ingest().await {
        Ok(ingest) => report.ingest = Some(ingest),
        Err(e) => report.warnings.push(format!("ingest: {e:#}")),
    }
    Ok(report)
}

#[derive(Debug, Default, Serialize)]
pub struct Status {
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Git remote name -> URL.
    pub remotes: BTreeMap<String, Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<KeySource>,
    pub auto_sync: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<ProjectInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_outbox: Option<usize>,
    pub errors: Vec<String>,
}

/// `gitbots cloud status`. Problems talking to the Worker are reported, not
/// returned as errors.
pub async fn status(project: &Project) -> Result<Status> {
    let repo = project.repo();
    let mut status = Status { auto_sync: auto_sync_enabled(repo)?, ..Default::default() };
    for name in [MAIN_REMOTE, LOGS_REMOTE] {
        status.remotes.insert(name.into(), repo.config_get(&format!("remote.{name}.url"))?);
    }
    let Some(config) = CloudConfig::load(repo)? else { return Ok(status) };
    status.configured = true;
    status.url = Some(config.url.clone());
    status.project = Some(config.project.clone());
    let cloud = match Cloud::from_config(project, config) {
        Ok(cloud) => cloud,
        Err(e) => {
            status.errors.push(format!("{e:#}"));
            return Ok(status);
        }
    };
    status.key = Some(cloud.key.source.clone());
    match cloud.client.project().await {
        Ok(info) => status.info = Some(info),
        Err(e) => status.errors.push(format!("{e:#}")),
    }
    match cloud.client.outbox().await {
        Ok(items) => status.pending_outbox = Some(items.len()),
        Err(e) => status.errors.push(format!("{e:#}")),
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_reused_until_a_minute_before_expiry() {
        let now = OffsetDateTime::parse("2026-10-03T10:00:00Z", &Rfc3339).unwrap();
        assert!(fresh("2026-10-03T10:01:01Z", now));
        assert!(!fresh("2026-10-03T10:01:00Z", now));
        assert!(!fresh("2026-10-03T09:00:00Z", now));
        assert!(!fresh("not a date", now));
    }

    #[test]
    fn urls_repos_and_ids() {
        assert_eq!(normalize_url(" https://w.example/ ").unwrap(), "https://w.example");
        assert!(normalize_url("w.example").is_err());
        assert_eq!(parse_repo("main"), RepoRef::Known(KnownRepo::Main));
        assert_eq!(parse_repo("prj_x-att-1"), RepoRef::Fork("prj_x-att-1".into()));
        assert_eq!(repo_label(&RepoRef::Fork("a/../b".into())), "a_.._b");
        assert!(valid_id("obx_01K") && !valid_id("../x") && !valid_id(""));
        let config = CloudConfig { url: "https://w.example".into(), project: "prj_1".into() };
        assert_eq!(config.credential_id(), "https://w.example#prj_1");
        assert_eq!(config.dashboard_link("k"), "https://w.example/#token=k");
    }

    #[test]
    fn credentials_round_trip_privately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gitbots").join(CREDENTIALS_FILE);
        let mut creds = Credentials::new();
        creds.insert("https://w.example#prj_1".into(), Credential { owner_key: "k1".into() });
        write_private(&path, toml::to_string(&creds).unwrap().as_bytes()).unwrap();
        let back = read_credentials(&path).unwrap();
        assert_eq!(back["https://w.example#prj_1"].owner_key, "k1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}

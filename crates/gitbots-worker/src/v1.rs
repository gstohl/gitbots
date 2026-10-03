//! The control-plane API (`/v1`, `docs/CLOUD.md`).

use gitbots_cloud::api::{
    CreateProject, ForkCreated, ForkRequest, IngestReport, KnownRepo, OutboxAck, OutboxAction,
    OutboxItem, ProjectCreated, ProjectInfo, Remotes, RepoRef, TokenIssued, TokenRequest,
    TokenScope,
};
use gitbots_cloud::index::LOGS_BRANCH;
use gitbots_cloud::naming;
use gitbots_core::{Actor, AttemptId, SessionId};
use serde_json::json;
use worker::{Request, Response};

use crate::Ctx;
use crate::artifacts::{CreatedRepo, Repo};
use crate::db::{OutboxRow, ProjectRow, RepoRow};
use crate::http::{ApiError, ApiResult, bearer, body, json_response, ok};
use crate::indexer::{index_repo, indexable_repos};
use crate::util::{now_rfc3339, random_bytes};

/// Artifacts token TTL bounds and default, in seconds.
const TTL_MIN: u64 = 60;
const TTL_MAX: u64 = 31_536_000;
pub const TTL_DEFAULT: u64 = 86_400;

/// Checks the `GITBOTS_ADMIN_KEY` secret, comparing hashes in constant time.
pub fn require_admin(ctx: &Ctx, req: &Request) -> ApiResult<()> {
    let secret = ctx
        .env
        .secret("GITBOTS_ADMIN_KEY")
        .map_err(|_| ApiError::internal("GITBOTS_ADMIN_KEY is not set on this deployment"))?
        .to_string();
    let given = bearer(req).ok_or_else(|| ApiError::unauthorized("missing admin key"))?;
    let ok = naming::constant_time_eq(
        naming::key_hash(&given).as_bytes(),
        naming::key_hash(&secret).as_bytes(),
    );
    if ok { Ok(()) } else { Err(ApiError::new(403, "wrong admin key")) }
}

/// The project whose owner key the request carries.
pub async fn require_owner(ctx: &Ctx, req: &Request) -> ApiResult<ProjectRow> {
    let given = bearer(req).ok_or_else(|| ApiError::unauthorized("missing owner key"))?;
    let hash = naming::key_hash(&given);
    match ctx.db.project_by_key_hash(&hash).await? {
        Some(p) if naming::constant_time_eq(p.owner_key_hash.as_bytes(), hash.as_bytes()) => Ok(p),
        _ => Err(ApiError::unauthorized("unknown owner key")),
    }
}

fn remotes(ctx: &Ctx, p: &ProjectRow) -> Remotes {
    Remotes { main: ctx.remote(&p.repo), logs: ctx.remote(&p.logs_repo) }
}

/// Creates a repo, or adopts it if it already exists (re-provisioning).
async fn create_or_adopt(
    ctx: &Ctx,
    name: &str,
    description: &str,
    branch: &str,
) -> ApiResult<String> {
    match ctx.artifacts.create(name, description, branch).await {
        Ok(CreatedRepo { remote, token, .. }) => {
            revoke_creation_token(ctx, name, &token).await;
            Ok(remote)
        }
        Err(e) if e.is("ALREADY_EXISTS") => {
            ctx.artifacts.get(name).await?;
            Ok(ctx.remote(name))
        }
        Err(e) => Err(e.into()),
    }
}

/// `POST /v1/projects` (admin key).
pub async fn create_project(ctx: &Ctx, mut req: Request) -> ApiResult<Response> {
    require_admin(ctx, &req)?;
    let CreateProject { project_id, name } = body(&mut req).await?;
    let repo =
        naming::project_repo(&project_id).map_err(|e| ApiError::unprocessable(e.to_string()))?;
    if name.trim().is_empty() {
        return Err(ApiError::unprocessable("name is required"));
    }
    if ctx.db.project(&project_id).await?.is_some() {
        return Err(ApiError::conflict(format!("project {project_id} already exists")));
    }
    let logs = naming::logs_repo(&repo);
    let main_remote = create_or_adopt(ctx, &repo, &format!("gitbots: {name}"), "main").await?;
    let logs_remote =
        create_or_adopt(ctx, &logs, &format!("gitbots logs: {name}"), LOGS_BRANCH).await?;

    let owner_key = naming::owner_key(&random_bytes::<32>());
    let row = ProjectRow {
        id: project_id.clone(),
        name,
        repo,
        logs_repo: logs,
        owner_key_hash: naming::key_hash(&owner_key),
        trusted_branch: "main".into(),
        created_at: now_rfc3339(),
    };
    ctx.db.insert_project(&row, &main_remote, &logs_remote).await?;
    // `-logs` repos are never indexed, so only `<prj>` is subscribed.
    crate::subscribe::subscribe_pushes(ctx, &row.repo).await;
    let created = ProjectCreated {
        project_id,
        owner_key,
        namespace: ctx.namespace.clone(),
        remotes: Remotes { main: main_remote, logs: logs_remote },
    };
    Ok(json_response(201, &created)?)
}

/// `GET /v1/project`.
pub async fn project(ctx: &Ctx, p: &ProjectRow) -> ApiResult<Response> {
    ok(&ProjectInfo {
        project_id: p.id.clone(),
        name: p.name.clone(),
        namespace: ctx.namespace.clone(),
        remotes: remotes(ctx, p),
        created_at: p.created_at.clone(),
    })
}

fn parse_session(session: Option<&str>) -> ApiResult<Option<String>> {
    session
        .map(|s| {
            s.parse::<SessionId>()
                .map(|id| id.to_string())
                .map_err(|e| ApiError::unprocessable(e.to_string()))
        })
        .transpose()
}

fn ttl(ttl: Option<u64>) -> ApiResult<u64> {
    let ttl = ttl.unwrap_or(TTL_DEFAULT);
    if (TTL_MIN..=TTL_MAX).contains(&ttl) {
        Ok(ttl)
    } else {
        Err(ApiError::unprocessable(format!("ttl_secs must be within {TTL_MIN}..={TTL_MAX}")))
    }
}

/// Mints a token on `repo` and records it.
async fn mint(
    ctx: &Ctx,
    p: &ProjectRow,
    handle: &Repo,
    write: bool,
    session: Option<&str>,
    ttl_secs: u64,
) -> ApiResult<(String, String)> {
    let issued = handle.create_token(write, ttl_secs).await?;
    let scope = if write { "write" } else { "read" };
    ctx.db
        .insert_token(&issued.id, &p.id, &handle.name, scope, session, &issued.expires_at)
        .await?;
    Ok((issued.plaintext, issued.expires_at))
}

/// `POST /v1/tokens`.
pub async fn tokens(ctx: &Ctx, p: &ProjectRow, mut req: Request) -> ApiResult<Response> {
    let TokenRequest { repo, scope, session, ttl_secs } = body(&mut req).await?;
    let session = parse_session(session.as_deref())?;
    let ttl_secs = ttl(ttl_secs)?;
    let name = match repo {
        RepoRef::Known(KnownRepo::Main) => p.repo.clone(),
        RepoRef::Known(KnownRepo::Logs) => p.logs_repo.clone(),
        RepoRef::Fork(name) => match ctx.db.repo(&name).await? {
            Some(r) if r.project_id == p.id && r.role == "fork" => r.name,
            _ => return Err(ApiError::not_found(format!("no fork `{name}` in this project"))),
        },
    };
    let handle = ctx.artifacts.get(&name).await?;
    let write = scope == TokenScope::Write;
    let (token, expires_at) = mint(ctx, p, &handle, write, session.as_deref(), ttl_secs).await?;
    ok(&TokenIssued { token, expires_at, remote: ctx.remote(&name) })
}

/// `create()` and `fork()` return a write token that no session holds.
/// Nobody needs it (tokens are minted per session), so revoke it.
async fn revoke_creation_token(ctx: &Ctx, repo: &str, token: &str) {
    let revoked = match ctx.artifacts.get(repo).await {
        Ok(handle) => handle.revoke_token(token).await,
        Err(e) => Err(e),
    };
    if let Err(e) = revoked {
        worker::console_warn!("{repo}: revoking the creation token: {e}");
    }
}

/// How long `POST /v1/forks` waits for a fresh fork to become usable.
const FORK_WAIT_MS: u64 = 10_000;
const FORK_POLL_MS: u64 = 500;

/// Mints a session write token on a fork, waiting while Artifacts is still
/// copying it (`FORK_IN_PROGRESS`).
async fn mint_on_fork(
    ctx: &Ctx,
    p: &ProjectRow,
    name: &str,
    session: Option<&str>,
) -> ApiResult<(String, String)> {
    let mut waited = 0;
    loop {
        let minted = match ctx.artifacts.get(name).await {
            Ok(handle) => mint(ctx, p, &handle, true, session, TTL_DEFAULT).await,
            Err(e) => Err(ApiError::from(e)),
        };
        match minted {
            Err(e) if e.status == 409 && waited < FORK_WAIT_MS => {
                worker::Delay::from(std::time::Duration::from_millis(FORK_POLL_MS)).await;
                waited += FORK_POLL_MS;
            }
            Err(e) if e.status == 409 => {
                return Err(ApiError::conflict(format!(
                    "fork {name} is still being created; retry POST /v1/forks ({})",
                    e.message
                )));
            }
            other => return other,
        }
    }
}

/// `POST /v1/forks`: fork `<prj>` into `<prj>-<attempt short>` (all
/// branches) and return a write token for it.
pub async fn forks(ctx: &Ctx, p: &ProjectRow, mut req: Request) -> ApiResult<Response> {
    let ForkRequest { attempt, session } = body(&mut req).await?;
    let attempt: AttemptId = attempt
        .parse()
        .map_err(|e: gitbots_core::IdError| ApiError::unprocessable(e.to_string()))?;
    let session = parse_session(session.as_deref())?;
    let existing = ctx.db.fork_for_attempt(&p.id, attempt.as_str()).await?;
    let name =
        existing.as_ref().map_or_else(|| naming::fork_repo(&p.repo, &attempt), |r| r.name.clone());
    if existing.is_none() {
        let main = ctx.artifacts.get(&p.repo).await?;
        let description = format!("gitbots: {} attempt {attempt}", p.name);
        let remote = match main.fork(&name, &description).await {
            Ok(created) => {
                revoke_creation_token(ctx, &name, &created.token).await;
                created.remote
            }
            Err(e) if e.is("ALREADY_EXISTS") => ctx.remote(&name),
            Err(e) => return Err(e.into()),
        };
        ctx.db
            .insert_fork(&RepoRow {
                name: name.clone(),
                project_id: p.id.clone(),
                role: "fork".into(),
                remote,
                attempt: Some(attempt.to_string()),
                session: session.clone(),
                created_at: now_rfc3339(),
                indexed_commit: None,
                indexed_tree: None,
            })
            .await?;
        crate::subscribe::subscribe_pushes(ctx, &name).await;
    }
    // Always a fresh token recorded against the session: pushes to the
    // fork are attested by it.
    let (token, expires_at) = mint_on_fork(ctx, p, &name, session.as_deref()).await?;
    ok(&ForkCreated { repo: name.clone(), remote: ctx.remote(&name), token, expires_at })
}

/// `POST /v1/ingest`: index `gitbots/activity` of `<prj>` and all its forks.
pub async fn ingest(ctx: &Ctx, p: &ProjectRow) -> ApiResult<Response> {
    let mut report = IngestReport { repos: vec![] };
    for repo in indexable_repos(&ctx.db, p).await? {
        match index_repo(&ctx.artifacts, &ctx.db, p, &repo, ctx.budget).await {
            Ok(done) => {
                if !done.complete {
                    worker::console_warn!("{}: index budget ran out; ingest again", repo.name);
                }
                report.repos.push(done.report);
            }
            // A broken fork must not block indexing the project.
            Err(e) if repo.role == "fork" => {
                worker::console_warn!("skipping fork {}: {}", repo.name, e.message);
            }
            Err(e) => return Err(e),
        }
    }
    ok(&report)
}

fn outbox_item(row: OutboxRow) -> ApiResult<OutboxItem> {
    let body: serde_json::Value = serde_json::from_str(&row.body)?;
    let action: OutboxAction = serde_json::from_value(json!({"kind": row.kind, "body": body}))?;
    let actor: Actor = serde_json::from_str(&row.actor)?;
    Ok(OutboxItem { id: row.id, created_at: row.created_at, action, actor })
}

/// `GET /v1/outbox`: pending items, oldest first.
pub async fn outbox(ctx: &Ctx, p: &ProjectRow) -> ApiResult<Response> {
    let items = ctx
        .db
        .pending_outbox(&p.id)
        .await?
        .into_iter()
        .map(outbox_item)
        .collect::<ApiResult<Vec<_>>>()?;
    ok(&items)
}

/// `POST /v1/outbox/{id}/ack`. Acking an item twice is a no-op.
pub async fn ack(ctx: &Ctx, p: &ProjectRow, id: &str, mut req: Request) -> ApiResult<Response> {
    let OutboxAck { event, error } = body(&mut req).await?;
    if ctx.db.outbox_item(&p.id, id).await?.is_none() {
        return Err(ApiError::not_found(format!("no outbox item {id}")));
    }
    ctx.db.ack_outbox(&p.id, id, event.as_deref(), error.as_deref()).await?;
    ok(&json!({}))
}

/// Shared by `/api` POSTs: queue a human decision.
pub async fn enqueue(
    ctx: &Ctx,
    p: &ProjectRow,
    action: &OutboxAction,
    actor: &Actor,
) -> ApiResult<String> {
    let id = format!("obx_{}", crate::util::new_ulid());
    let value = serde_json::to_value(action)?;
    let kind = value["kind"].as_str().unwrap_or_default().to_owned();
    let body = serde_json::to_string(&value["body"])?;
    ctx.db.insert_outbox(&id, &p.id, &kind, &body, &serde_json::to_string(actor)?).await?;
    Ok(id)
}

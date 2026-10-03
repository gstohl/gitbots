//! The hosted dashboard API (`/api`, `docs/API.md` with the hosted
//! differences from `docs/CLOUD.md`). Reads fold indexed events with
//! `gitbots-core`; writes go to the outbox.

use gitbots_cloud::api::{NewTask, OutboxAction, ReviewRequest};
use gitbots_cloud::commits::{commits_between, merge_base};
use gitbots_cloud::dashboard::{self, EventFilter, ManifestAt};
use gitbots_cloud::diff::{MAX_DIFF_BYTES, tree_changes, unified_diff};
use gitbots_cloud::index::LOGS_BRANCH;
use gitbots_cloud::source::{TreeSource, list_blobs, lookup};
use gitbots_core::manifest::MANIFEST_PATH;
use gitbots_core::{Actor, AttemptState, AttemptView, Board, Event, Manifest, Recipe};
use serde_json::{Value, json};
use worker::{Method, Request, Response, Url};

use crate::Ctx;
use crate::artifacts::Repo;
use crate::db::ProjectRow;
use crate::http::{ApiError, ApiResult, body, json_response, ok, text_response};
use crate::v1::enqueue;

const PRODUCER: &str = concat!("gitbots-worker/", env!("CARGO_PKG_VERSION"));
const RECIPES_DIR: &str = ".gitbots/recipes";
const ACTIONS_DIR: &str = ".gitbots/actions";

pub async fn route(
    ctx: &Ctx,
    p: &ProjectRow,
    method: Method,
    segs: &[&str],
    req: Request,
) -> ApiResult<Response> {
    let url = req.url()?;
    match (method, segs) {
        (Method::Get, ["project"]) => project(ctx, p).await,
        (Method::Get, ["tip"]) => ok(&json!({ "activity": ctx.db.latest_event(&p.id).await? })),
        (Method::Get, ["board"]) => ok(&dashboard::board_json(&load(ctx, p).await?.1)?),
        (Method::Get, ["inbox"]) => ok(&dashboard::inbox_json(&load(ctx, p).await?.1)?),
        (Method::Get, ["stats"]) => ok(&dashboard::stats_json(&load(ctx, p).await?.0)?),
        (Method::Get, ["events"]) => events(ctx, p, &url).await,
        (Method::Get, ["attempts", id]) => attempt(ctx, p, id).await,
        (Method::Get, ["attempts", id, "diff"]) => diff(ctx, p, id).await,
        (Method::Get, ["logs"]) => logs(ctx, p, &url).await,
        (Method::Get, ["workflows"]) => workflows(ctx, p).await,
        (Method::Get, ["recipes"]) => recipes(ctx, p).await,
        (Method::Post, ["tasks"]) => create_task(ctx, p, req).await,
        (Method::Post, ["attempts", id, "review"]) => review(ctx, p, id, req).await,
        _ => Err(ApiError::not_found("no such endpoint")),
    }
}

/// The project's indexed events (sorted by id) and their board.
async fn load(ctx: &Ctx, p: &ProjectRow) -> ApiResult<(Vec<Event>, Board)> {
    let mut events = Vec::new();
    for json in ctx.db.events_json(&p.id).await? {
        match serde_json::from_str::<Event>(&json) {
            Ok(e) => events.push(e),
            Err(e) => worker::console_warn!("stored event does not parse: {e}"),
        }
    }
    let board = Board::from_events(&events);
    Ok((events, board))
}

fn query(url: &Url, key: &str) -> Option<String> {
    url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

/// `.gitbots/manifest.json` on the trusted branch of `<prj>`.
async fn manifest(main: &Repo, branch: &str) -> ApiResult<Option<ManifestAt>> {
    let Some(tip) = main.tip(branch).await? else { return Ok(None) };
    let commit = main.read_commit(&tip).await?;
    let Some(entry) = lookup(main, &commit.tree, MANIFEST_PATH).await? else { return Ok(None) };
    let bytes = main.read_blob(&entry.sha).await?;
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::unprocessable(format!("{MANIFEST_PATH} on {branch}: {e}")))?;
    Ok(Some(ManifestAt { manifest, branch: branch.to_owned(), oid: entry.sha }))
}

async fn project(ctx: &Ctx, p: &ProjectRow) -> ApiResult<Response> {
    let main = ctx.artifacts.get(&p.repo).await?;
    let at = manifest(&main, &p.trusted_branch).await?.ok_or_else(|| {
        ApiError::not_found(format!(
            "no {MANIFEST_PATH} on `{}` of {} yet: push it first",
            p.trusted_branch, p.repo
        ))
    })?;
    let pending = ctx.db.count_pending(&p.id).await?;
    let mut body = dashboard::project_json(&at, PRODUCER, pending)?;
    // Hosted only: the steward's recent ack errors (newest first, max 10).
    body["outbox_errors"] = serde_json::to_value(ctx.db.outbox_errors(&p.id, 10).await?)?;
    ok(&body)
}

async fn events(ctx: &Ctx, p: &ProjectRow, url: &Url) -> ApiResult<Response> {
    let limit = match query(url, "limit") {
        Some(l) => l.parse().map_err(|_| ApiError::bad_request("limit must be a number"))?,
        None => 200,
    };
    let filter = EventFilter {
        kind: query(url, "kind"),
        session: query(url, "session"),
        task: query(url, "task"),
        attempt: query(url, "attempt"),
        limit: Some(limit),
    };
    let (events, board) = load(ctx, p).await?;
    ok(&dashboard::query_events(&events, &board, &filter)?)
}

/// The repo that holds an attempt's commits: its fork, if it has one.
async fn attempt_repo(ctx: &Ctx, p: &ProjectRow, a: &AttemptView) -> ApiResult<Repo> {
    let name = match ctx.db.fork_for_attempt(&p.id, a.id.as_str()).await? {
        Some(fork) => fork.name,
        None => p.repo.clone(),
    };
    Ok(ctx.artifacts.get(&name).await?)
}

/// `base...head` like `gitbots ui`: against the current base branch before a
/// merge, against the start commit after.
async fn diff_base(repo: &Repo, a: &AttemptView, head: &str) -> ApiResult<String> {
    if a.state == AttemptState::Merged {
        return Ok(a.base_commit.clone());
    }
    let Some(base_tip) = repo.tip(&a.base).await? else { return Ok(a.base_commit.clone()) };
    Ok(merge_base(repo, &base_tip, head).await?.unwrap_or_else(|| a.base_commit.clone()))
}

async fn head_of(repo: &Repo, a: &AttemptView) -> ApiResult<Option<String>> {
    match &a.head {
        Some(head) => Ok(Some(head.clone())),
        None => Ok(repo.tip(&a.branch).await?),
    }
}

async fn changed_paths(repo: &Repo, base: &str, head: &str) -> ApiResult<Vec<String>> {
    let old = repo.read_commit(base).await?.tree;
    let new = repo.read_commit(head).await?.tree;
    Ok(tree_changes(repo, Some(&old), Some(&new)).await?.into_iter().map(|c| c.path).collect())
}

async fn attempt(ctx: &Ctx, p: &ProjectRow, id: &str) -> ApiResult<Response> {
    let (events, board) = load(ctx, p).await?;
    let a = board.find_attempt(id)?;
    let repo = attempt_repo(ctx, p, a).await?;
    let mut commits = vec![];
    let mut violations = vec![];
    if let Some(head) = &a.head {
        // Best effort, as in `gitbots ui`: the branch may not be pushed yet.
        commits = commits_between(&repo, &a.base_commit, head).await.unwrap_or_default();
        let main = ctx.artifacts.get(&p.repo).await?;
        if let (Ok(base), Ok(Some(at))) =
            (diff_base(&repo, a, head).await, manifest(&main, &p.trusted_branch).await)
            && let Ok(paths) = changed_paths(&repo, &base, head).await
        {
            violations = at
                .manifest
                .mandate
                .agents
                .violations(paths.iter().map(String::as_str))
                .unwrap_or_default();
        }
    }
    ok(&dashboard::attempt_detail_json(&board, a, &events, &commits, &violations)?)
}

async fn diff(ctx: &Ctx, p: &ProjectRow, id: &str) -> ApiResult<Response> {
    let (_, board) = load(ctx, p).await?;
    let a = board.find_attempt(id)?;
    let repo = attempt_repo(ctx, p, a).await?;
    let Some(head) = head_of(&repo, a).await? else { return text_response(String::new()) };
    let base = diff_base(&repo, a, &head).await?;
    let old = repo.read_commit(&base).await?.tree;
    let new = repo.read_commit(&head).await?.tree;
    let changes = tree_changes(&repo, Some(&old), Some(&new)).await?;
    text_response(unified_diff(&repo, &changes, MAX_DIFF_BYTES).await?)
}

async fn logs(ctx: &Ctx, p: &ProjectRow, url: &Url) -> ApiResult<Response> {
    let path = query(url, "path").unwrap_or_default();
    let safe = !path.is_empty()
        && !path.starts_with('/')
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..");
    if !safe {
        return Err(ApiError::bad_request("path must be a relative path on the logs branch"));
    }
    let repo = ctx.artifacts.get(&p.logs_repo).await?;
    let bytes = repo
        .file(LOGS_BRANCH, &path)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no log at {path}")))?;
    text_response(String::from_utf8_lossy(&bytes).into_owned())
}

/// Files below `dir` on the trusted branch, as `(path, bytes)`.
async fn trusted_files(ctx: &Ctx, p: &ProjectRow, dir: &str) -> ApiResult<Vec<(String, Vec<u8>)>> {
    let main = ctx.artifacts.get(&p.repo).await?;
    let Some(tip) = main.tip(&p.trusted_branch).await? else { return Ok(vec![]) };
    let root = main.read_commit(&tip).await?.tree;
    let mut out = Vec::new();
    for (path, sha) in list_blobs(&main, &root, dir).await? {
        out.push((path, main.read_blob(&sha).await?));
    }
    Ok(out)
}

/// Hosted, workflows are listed as their TOML converted to JSON with the
/// spec's defaults filled in, but without the `gitbots-actions` validation
/// (that crate does not build for wasm32).
async fn workflows(ctx: &Ctx, p: &ProjectRow) -> ApiResult<Response> {
    let mut ok_list = Vec::new();
    let mut invalid = Vec::new();
    for (path, bytes) in trusted_files(ctx, p, ACTIONS_DIR).await? {
        if !path.ends_with(".toml") {
            continue;
        }
        let parsed = std::str::from_utf8(&bytes)
            .map_err(|e| e.to_string())
            .and_then(|s| toml::from_str::<toml::Table>(s).map_err(|e| e.to_string()))
            .and_then(|t| serde_json::to_value(t).map_err(|e| e.to_string()));
        match parsed {
            Ok(mut workflow) => {
                dashboard::fill_workflow_defaults(&mut workflow);
                ok_list.push(json!({"path": path, "workflow": workflow}));
            }
            Err(error) => invalid.push(json!({"path": path, "error": error})),
        }
    }
    ok(&json!({ "workflows": ok_list, "invalid": invalid }))
}

async fn recipes(ctx: &Ctx, p: &ProjectRow) -> ApiResult<Response> {
    let list: Vec<Value> = trusted_files(ctx, p, RECIPES_DIR)
        .await?
        .into_iter()
        .filter(|(path, _)| path.ends_with("/recipe.toml"))
        .map(|(path, bytes)| {
            let parsed = std::str::from_utf8(&bytes)
                .map_err(|e| e.to_string())
                .and_then(|s| toml::from_str::<Recipe>(s).map_err(|e| e.to_string()))
                .and_then(|r| r.validate().map(|()| r).map_err(|e| e.to_string()));
            match parsed {
                Ok(recipe) => json!({"path": path, "recipe": recipe}),
                Err(error) => json!({"path": path, "error": error}),
            }
        })
        .collect();
    ok(&list)
}

/// The human an outbox item is recorded for (see `dashboard::owner_actor`).
async fn decider(ctx: &Ctx, p: &ProjectRow) -> ApiResult<Actor> {
    let main = ctx.artifacts.get(&p.repo).await?;
    Ok(match manifest(&main, &p.trusted_branch).await? {
        Some(at) => dashboard::owner_actor(&at.manifest),
        None => Actor::human("owner", None),
    })
}

fn queued(id: &str) -> ApiResult<Response> {
    Ok(json_response(202, &json!({ "queued": true, "outbox": id }))?)
}

async fn create_task(ctx: &Ctx, p: &ProjectRow, mut req: Request) -> ApiResult<Response> {
    let task: NewTask = body(&mut req).await?;
    let title = task.title.trim().to_owned();
    if title.is_empty() {
        return Err(ApiError::unprocessable("title is required"));
    }
    let task =
        NewTask { title, body: task.body.filter(|b| !b.trim().is_empty()), labels: task.labels };
    let actor = decider(ctx, p).await?;
    queued(&enqueue(ctx, p, &OutboxAction::TaskCreate(task), &actor).await?)
}

async fn review(ctx: &Ctx, p: &ProjectRow, id: &str, mut req: Request) -> ApiResult<Response> {
    let mut review: ReviewRequest = body(&mut req).await?;
    let (_, board) = load(ctx, p).await?;
    let a = board.find_attempt(id)?;
    if a.state != AttemptState::Submitted {
        return Err(ApiError::unprocessable(format!(
            "attempt is {}; only submitted attempts can be reviewed",
            a.state.as_str()
        )));
    }
    let attempt = a.id.to_string();
    for item in ctx.db.pending_outbox(&p.id).await? {
        if item.kind == "review" && item.body.contains(&format!("\"{attempt}\"")) {
            return Err(ApiError::conflict(format!(
                "a review of {attempt} is already queued ({})",
                item.id
            )));
        }
    }
    review.attempt = Some(attempt);
    review.reason = review.reason.filter(|r| !r.trim().is_empty());
    let actor = decider(ctx, p).await?;
    queued(&enqueue(ctx, p, &OutboxAction::Review(review), &actor).await?)
}

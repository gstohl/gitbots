//! `gitbots ui`: the local JSON API from `docs/API.md`, plus the SolidJS
//! frontend's static files.
//!
//! Security model: bound to 127.0.0.1 only; every `/api` call needs the
//! random bearer token printed at launch (only the human at that terminal has
//! it); `Host` must be a loopback name (DNS-rebinding guard). Writes act as
//! the git-config human with `via: "ui"`. If an agent harness launched the
//! server, it is read-only.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use gitbots_core::event::{EventBody, ReviewDecision};
use gitbots_core::{Actor, AttemptView, Board, LookupError, Session, TaskView, Ulid, Via};
use gitbots_git::LedgerError;

use crate::project::{CreateTask, EventFilter, ManifestSource, PRODUCER, Project, agent_marker};

/// Diffs larger than this are truncated.
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;

pub struct UiOptions {
    pub port: u16,
    /// Built frontend (`web/dist`). Defaults to the source tree's `web/dist`
    /// when gitbots was built from a checkout and it exists.
    pub assets: Option<PathBuf>,
}

#[derive(Clone)]
struct AppState {
    root: Arc<PathBuf>,
    token: Arc<str>,
    can_decide: bool,
    port: u16,
    assets: Option<Arc<PathBuf>>,
}

pub async fn serve(root: PathBuf, opts: UiOptions) -> Result<()> {
    let token = format!("{:020x}{:020x}", Ulid::generate().random(), Ulid::generate().random());
    let assets = opts.assets.or_else(default_assets).filter(|p| p.join("index.html").is_file());
    let state = AppState {
        root: Arc::new(root),
        token: token.clone().into(),
        can_decide: agent_marker().is_none(),
        port: opts.port,
        assets: assets.clone().map(Arc::new),
    };

    let api = Router::new()
        .route("/project", get(project))
        .route("/tip", get(tip))
        .route("/board", get(board))
        .route("/inbox", get(inbox))
        .route("/events", get(events))
        .route("/stats", get(stats))
        .route("/attempts/{id}", get(attempt))
        .route("/attempts/{id}/diff", get(diff))
        .route("/attempts/{id}/review", post(review))
        .route("/logs", get(logs))
        .route("/workflows", get(workflows))
        .route("/recipes", get(recipes))
        .route("/tasks", post(create_task))
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "no such endpoint") })
        .layer(middleware::from_fn(json_errors))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));
    let app = Router::new()
        .nest("/api", api)
        .fallback(static_files)
        .layer(middleware::from_fn_with_state(state.clone(), require_loopback_host))
        .with_state(state.clone());

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, opts.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr} (try --port)"))?;
    eprintln!("gitbots ui: http://127.0.0.1:{}/#token={token}", opts.port);
    match &assets {
        Some(dir) => eprintln!("serving frontend from {}", dir.display()),
        None => eprintln!("no frontend build found: API only (build web/ or pass --assets)"),
    }
    if !state.can_decide {
        eprintln!("started by an agent ({}): read-only", agent_marker().unwrap_or_default());
    }
    axum::serve(listener, app).await?;
    Ok(())
}

fn default_assets() -> Option<PathBuf> {
    std::env::var_os("GITBOTS_UI_ASSETS").map(PathBuf::from).or_else(|| {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
        dir.is_dir().then_some(dir)
    })
}

// ---- errors -------------------------------------------------------------

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let message = format!("{e:#}");
        let status = if let Some(l) = e.downcast_ref::<LookupError>() {
            match l {
                LookupError::NotFound { .. } => StatusCode::NOT_FOUND,
                LookupError::Ambiguous { .. } => StatusCode::CONFLICT,
            }
        } else if let Some(LedgerError::MergeConflict { .. }) = e.downcast_ref::<LedgerError>() {
            StatusCode::CONFLICT
        } else if message.contains("only submitted attempts") || message.contains("not open") {
            StatusCode::CONFLICT
        } else if message.starts_with("denied")
            || message.contains("needs a human")
            || message.contains("own session tree")
        {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::UNPROCESSABLE_ENTITY
        };
        Self { status, message }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// Run `f` against a freshly opened project on the blocking pool. Opening per
/// request keeps the mandate current when the trusted branch moves.
async fn with_project<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(&Project) -> Result<T> + Send + 'static,
) -> ApiResult<T> {
    let root = state.root.clone();
    tokio::task::spawn_blocking(move || f(&Project::open(&root)?))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(ApiError::from)
}

// ---- middleware ---------------------------------------------------------

async fn require_loopback_host(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let port = state.port;
    let allowed =
        [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")];
    if allowed.iter().any(|a| a == host) {
        next.run(req).await
    } else {
        ApiError::new(StatusCode::FORBIDDEN, format!("host `{host}` not allowed")).into_response()
    }
}

async fn require_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    if constant_time_eq(given.as_bytes(), state.token.as_bytes()) {
        next.run(req).await
    } else {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "missing or wrong token: open the link `gitbots ui` printed",
        )
        .into_response()
    }
}

/// Framework rejections (bad JSON, missing query params, wrong content
/// type) are plain text; the contract says every error is `{"error": ...}`.
async fn json_errors(req: Request, next: Next) -> Response {
    let res = next.run(req).await;
    let is_json = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !(res.status().is_client_error() || res.status().is_server_error()) || is_json {
        return res;
    }
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap_or_default();
    let message = String::from_utf8_lossy(&body).trim().to_owned();
    ApiError::new(status, if message.is_empty() { status.to_string() } else { message })
        .into_response()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---- read endpoints -----------------------------------------------------

fn task_json(board: &Board, t: &TaskView) -> Result<Value> {
    let mut v = serde_json::to_value(t)?;
    v["status"] = serde_json::to_value(board.task_status(t))?;
    Ok(v)
}

fn attempt_json(a: &AttemptView) -> Result<Value> {
    let mut v = serde_json::to_value(a)?;
    v["checks_passed"] = json!(a.checks_passed());
    Ok(v)
}

async fn project(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let can_decide = state.can_decide;
    with_project(&state, move |p| {
        let source = match p.manifest_source() {
            ManifestSource::Trusted { branch, oid } => {
                json!({"type": "trusted", "branch": branch, "oid": oid})
            }
            ManifestSource::WorkingTree => json!({"type": "working_tree"}),
        };
        let viewer = p.human_via(Via::Ui).ok().map(|c| c.actor);
        Ok(Json(json!({
            "manifest": p.manifest(),
            "manifest_source": source,
            "trusted_branch": p.trusted_branch(),
            "viewer": viewer,
            "can_decide": can_decide,
            "producer": PRODUCER,
        })))
    })
    .await
}

async fn tip(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    with_project(&state, |p| Ok(Json(json!({ "activity": p.activity().tip()? })))).await
}

async fn board(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    with_project(&state, |p| {
        let board = p.board()?;
        let tasks: Vec<Value> =
            board.tasks.values().map(|t| task_json(&board, t)).collect::<Result<_>>()?;
        let attempts: Vec<Value> =
            board.attempts.values().map(attempt_json).collect::<Result<_>>()?;
        let awaiting: Vec<_> = board.awaiting_review().map(|a| &a.id).collect();
        Ok(Json(json!({
            "tasks": tasks,
            "attempts": attempts,
            "sessions": board.sessions.values().collect::<Vec<_>>(),
            "awaiting_review": awaiting,
            "orphans": board.orphans,
        })))
    })
    .await
}

async fn inbox(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    with_project(&state, |p| Ok(Json(serde_json::to_value(p.inbox()?)?))).await
}

#[derive(Deserialize)]
struct EventsQuery {
    kind: Option<String>,
    session: Option<String>,
    task: Option<String>,
    attempt: Option<String>,
    limit: Option<usize>,
}

async fn events(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Json<Value>> {
    with_project(&state, move |p| {
        let events = p.query_events(&EventFilter {
            kind: q.kind.filter(|s| !s.is_empty()),
            session: q.session.filter(|s| !s.is_empty()),
            task: q.task.filter(|s| !s.is_empty()),
            attempt: q.attempt.filter(|s| !s.is_empty()),
            limit: Some(q.limit.unwrap_or(200)),
        })?;
        Ok(Json(serde_json::to_value(events)?))
    })
    .await
}

async fn stats(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    with_project(&state, |p| Ok(Json(serde_json::to_value(p.stats()?)?))).await
}

async fn attempt(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    with_project(&state, move |p| {
        let board = p.board()?;
        let a = board.find_attempt(&id)?;
        let task = board.tasks.get(&a.task).context("attempt's task is missing")?;
        let violations: Vec<Value> = p
            .violations(a)
            .unwrap_or_default()
            .into_iter()
            .map(|v| json!({"path": v.path, "reason": v.reason}))
            .collect();
        let commits: Vec<Value> = match &a.head {
            Some(head) => p
                .repo()
                .commits_between(&a.base_commit, head)?
                .into_iter()
                .map(|sha| {
                    let subject = p.repo().commit_subject(&sha)?;
                    let trailers = p.repo().commit_trailers(&sha)?;
                    Ok(json!({"sha": sha, "subject": subject, "trailers": trailers}))
                })
                .collect::<Result<_>>()?,
            None => vec![],
        };
        let events =
            p.query_events(&EventFilter { attempt: Some(a.id.to_string()), ..Default::default() })?;
        let runs: Vec<&gitbots_core::ActionRun> = events
            .iter()
            .filter_map(|e| match &e.body {
                EventBody::ActionCompleted(run) => Some(run),
                _ => None,
            })
            .collect();
        Ok(Json(json!({
            "attempt": attempt_json(a)?,
            "task": task_json(&board, task)?,
            "workroom": p.workroom_of(a)?,
            "violations": violations,
            "commits": commits,
            "runs": runs,
            "events": events,
            "session_chain": session_chain(&board, a.session.as_ref()),
        })))
    })
    .await
}

fn session_chain(board: &Board, start: Option<&gitbots_core::SessionId>) -> Vec<Session> {
    let mut chain: Vec<Session> = Vec::new();
    let mut cursor = start.cloned();
    while let Some(id) = cursor {
        let Some(view) = board.sessions.get(&id) else { break };
        if chain.iter().any(|s| s.id == id) {
            break;
        }
        cursor = view.session.parent.clone();
        chain.push(view.session.clone());
    }
    chain
}

async fn diff(State(state): State<AppState>, UrlPath(id): UrlPath<String>) -> ApiResult<Response> {
    let text = with_project(&state, move |p| {
        let board = p.board()?;
        let a = board.find_attempt(&id)?;
        let Some(head) = &a.head else { return Ok(String::new()) };
        // Before merge, review against the current base; after, against the start.
        let base = if a.state == gitbots_core::AttemptState::Merged {
            a.base_commit.clone()
        } else {
            format!("refs/heads/{}", a.base)
        };
        let range = format!("{base}...{head}");
        let mut out = p.repo().git(&["diff", "--no-color", "--no-ext-diff", "-M", &range])?;
        if !out.is_empty() {
            out.push('\n');
        }
        if out.len() > MAX_DIFF_BYTES {
            let mut cut = MAX_DIFF_BYTES;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
            out.push_str("\n[gitbots: diff truncated at 2 MB]\n");
        }
        Ok(out)
    })
    .await?;
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response())
}

#[derive(Deserialize)]
struct LogQuery {
    path: String,
}

async fn logs(State(state): State<AppState>, Query(q): Query<LogQuery>) -> ApiResult<Response> {
    let bytes = with_project(&state, move |p| {
        p.logs().read(&q.path)?.with_context(|| format!("no log at {}", q.path))
    })
    .await
    .map_err(|e| {
        if e.status == StatusCode::UNPROCESSABLE_ENTITY {
            ApiError::new(StatusCode::NOT_FOUND, e.message)
        } else {
            e
        }
    })?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response())
}

async fn workflows(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    with_project(&state, |p| {
        let (ok, bad) = p.workflows()?;
        Ok(Json(json!({
            "workflows": ok.iter().map(|(path, wf)| json!({"path": path, "workflow": wf})).collect::<Vec<_>>(),
            "invalid": bad.iter().map(|(path, e)| json!({"path": path, "error": e})).collect::<Vec<_>>(),
        })))
    })
    .await
}

async fn recipes(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    with_project(&state, |p| {
        let list: Vec<Value> = p
            .recipes()?
            .into_iter()
            .map(|(path, r)| match r {
                Ok(recipe) => json!({"path": path, "recipe": recipe}),
                Err(e) => json!({"path": path, "error": format!("{e:#}")}),
            })
            .collect();
        Ok(Json(Value::Array(list)))
    })
    .await
}

// ---- write endpoints ----------------------------------------------------

fn ensure_can_decide(state: &AppState) -> ApiResult<()> {
    if state.can_decide {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "this gitbots ui was started by an agent and is read-only",
        ))
    }
}

#[derive(Deserialize)]
struct NewTask {
    title: String,
    body: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
}

async fn create_task(
    State(state): State<AppState>,
    Json(req): Json<NewTask>,
) -> ApiResult<Json<Value>> {
    ensure_can_decide(&state)?;
    if req.title.trim().is_empty() {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "title is required"));
    }
    with_project(&state, move |p| {
        let ctx = p.human_via(Via::Ui)?;
        let task = p.create_task(
            &ctx,
            CreateTask {
                title: req.title.trim().to_owned(),
                body: req.body.filter(|b| !b.trim().is_empty()),
                labels: req.labels,
                recipe: None,
                inputs: BTreeMap::new(),
            },
        )?;
        Ok(Json(json!({ "task": task })))
    })
    .await
}

#[derive(Deserialize)]
struct ReviewRequest {
    decision: ReviewDecision,
    reason: Option<String>,
    #[serde(default)]
    merge: bool,
}

async fn review(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Json(req): Json<ReviewRequest>,
) -> ApiResult<Json<Value>> {
    ensure_can_decide(&state)?;
    with_project(&state, move |p| {
        let ctx = p.human_via(Via::Ui)?;
        debug_assert!(matches!(ctx.actor, Actor::Human { .. }));
        let outcome = p.review(
            &ctx,
            &id,
            req.decision,
            req.reason.filter(|r| !r.trim().is_empty()),
            req.merge,
        )?;
        Ok(Json(serde_json::to_value(outcome)?))
    })
    .await
}

// ---- static files -------------------------------------------------------

async fn static_files(State(state): State<AppState>, req: Request) -> Response {
    let Some(dir) = &state.assets else {
        return (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<!doctype html><title>gitbots ui</title><p>The API is running. Build the frontend \
             (<code>cd web &amp;&amp; npm ci &amp;&amp; npm run build</code>) and restart with \
             <code>--assets web/dist</code>, or run <code>npm run dev</code> in <code>web/</code>.</p>",
        )
            .into_response();
    };
    let rel = req.uri().path().trim_start_matches('/');
    let safe = Path::new(rel).components().all(|c| matches!(c, Component::Normal(_)));
    let file = if safe && !rel.is_empty() { dir.join(rel) } else { dir.join("index.html") };
    // SPA fallback: unknown paths get index.html so client routes work on
    // reload, but a missing hashed asset is a real 404 (a stale tab would
    // otherwise get HTML where it expects JS).
    if !file.is_file() && rel.starts_with("assets/") {
        return ApiError::new(StatusCode::NOT_FOUND, "no such asset").into_response();
    }
    let file = if file.is_file() { file } else { dir.join("index.html") };
    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&file).first_or_octet_stream();
            let cache = if rel.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            (
                [
                    (header::CONTENT_TYPE, mime.essence_str().to_owned()),
                    (header::CACHE_CONTROL, cache.to_owned()),
                ],
                bytes,
            )
                .into_response()
        }
        Err(e) => ApiError::new(StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

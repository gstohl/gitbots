//! `gitbots-worker`: gitbots's Cloudflare control plane (`docs/CLOUD.md`).
//!
//! - `/v1/*`: provisioning, tokens, forks, ingest and the outbox;
//! - `/api/*`: the dashboard API of `docs/API.md`, folded from D1;
//! - a Queue consumer for Artifacts push events (re-indexes the repo);
//! - everything else: the SolidJS build (static assets, SPA fallback).
//!
//! The Worker never writes git: Artifacts has no write API outside `git
//! push`, so human decisions go to the D1 outbox for `gitbots sync`.

mod artifacts;
mod dash;
mod db;
mod http;
mod indexer;
mod subscribe;
mod util;
mod v1;

use gitbots_cloud::index::ACTIVITY_BRANCH;
use gitbots_cloud::naming;
use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::JsValue;
use worker::{Context, Env, MessageBatch, MessageExt, Method, Request, Response, event};

use crate::artifacts::Artifacts;
use crate::db::Db;
use crate::http::{ApiError, ApiResult, ok};

/// Per-request handles and settings.
pub struct Ctx {
    pub env: Env,
    pub db: Db,
    pub artifacts: Artifacts,
    pub namespace: String,
    pub account_id: String,
    /// Blobs one indexing run may read (`INDEX_BUDGET`).
    pub budget: usize,
}

impl Ctx {
    fn new(env: Env) -> worker::Result<Self> {
        let var = |name: &str| env.var(name).map(|v| v.to_string());
        Ok(Self {
            db: Db::from_env(&env)?,
            artifacts: Artifacts::from_env(&env)?,
            namespace: var("ARTIFACTS_NAMESPACE")?,
            account_id: var("ARTIFACTS_ACCOUNT_ID")?,
            budget: var("INDEX_BUDGET").ok().and_then(|b| b.parse().ok()).unwrap_or(2000),
            env,
        })
    }

    /// The git remote of a repo in this deployment's namespace.
    pub fn remote(&self, repo: &str) -> String {
        naming::remote_url(&self.account_id, &self.namespace, repo)
    }
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> worker::Result<Response> {
    let path = req.path();
    let is_api = path == "/api" || path.starts_with("/api/") || path.starts_with("/v1/");
    if !is_api {
        // Assets normally never reach the Worker (`run_worker_first` lists
        // only the API paths); this covers `wrangler dev` quirks.
        return env.assets("ASSETS")?.fetch_request(req).await;
    }
    match handle(req, env).await {
        Ok(resp) => Ok(resp),
        Err(e) => e.into_response(),
    }
}

async fn handle(req: Request, env: Env) -> ApiResult<Response> {
    let ctx = Ctx::new(env)?;
    let path = req.path();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let method = req.method();
    match (method, segs.as_slice()) {
        (Method::Post, ["v1", "projects"]) => v1::create_project(&ctx, req).await,
        (Method::Get, ["v1", "admin", "inspect"]) => inspect(&ctx, &req).await,
        (method, ["v1", rest @ ..]) => {
            let p = v1::require_owner(&ctx, &req).await?;
            match (method, rest) {
                (Method::Get, ["project"]) => v1::project(&ctx, &p).await,
                (Method::Post, ["tokens"]) => v1::tokens(&ctx, &p, req).await,
                (Method::Post, ["forks"]) => v1::forks(&ctx, &p, req).await,
                (Method::Post, ["ingest"]) => v1::ingest(&ctx, &p).await,
                (Method::Get, ["outbox"]) => v1::outbox(&ctx, &p).await,
                (Method::Post, ["outbox", id, "ack"]) => v1::ack(&ctx, &p, id, req).await,
                _ => Err(ApiError::not_found("no such endpoint")),
            }
        }
        (method, ["api", rest @ ..]) => {
            let p = v1::require_owner(&ctx, &req).await?;
            dash::route(&ctx, &p, method, rest, req).await
        }
        _ => Err(ApiError::not_found("no such endpoint")),
    }
}

fn raw(v: Result<JsValue, artifacts::ArtifactsError>) -> Value {
    match v {
        Ok(v) => js_sys::JSON::stringify(&v)
            .ok()
            .and_then(|s| s.as_string())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null),
        Err(e) => json!({ "error": e.to_string(), "code": e.code }),
    }
}

/// `GET /v1/admin/inspect?repo=&ref=` (admin key): raw binding results, to
/// check field names and ref spellings against a live repo.
async fn inspect(ctx: &Ctx, req: &Request) -> ApiResult<Response> {
    v1::require_admin(ctx, req)?;
    let url = req.url()?;
    let get = |k: &str| url.query_pairs().find(|(q, _)| q == k).map(|(_, v)| v.into_owned());
    let name = get("repo").ok_or_else(|| ApiError::bad_request("repo is required"))?;
    let reference = get("ref").unwrap_or_else(|| ACTIVITY_BRANCH.to_owned());
    let repo = ctx.artifacts.get(&name).await?;
    let mut logs = serde_json::Map::new();
    for spelling in [reference.clone(), format!("refs/heads/{reference}"), "HEAD".into()] {
        logs.insert(spelling.clone(), raw(repo.log_raw(&spelling, 1).await));
    }
    let tip = repo.tip(&reference).await?;
    let mut out = json!({ "info": raw(repo.info_raw().await), "log": logs, "tip": tip });
    if let Some(tip) = tip {
        let by_sha = raw(repo.log_raw(&tip, 1).await);
        out["log_by_sha"] = by_sha;
        if let Some(commit) = repo.commit(&tip).await? {
            out["tree"] = raw(repo.tree_raw(&commit.tree_hash).await);
        }
    }
    ok(&out)
}

#[derive(Deserialize)]
struct ArtifactsEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    source: EventSource,
    #[serde(default)]
    payload: PushPayload,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventSource {
    namespace: Option<String>,
    repo_name: Option<String>,
}

#[derive(Default, Deserialize)]
struct PushPayload {
    #[serde(rename = "ref")]
    reference: Option<String>,
}

const PUSHED: &str = "cf.artifacts.repo.pushed";

/// Handles one subscription message; `Ok(false)` asks for a retry (the
/// index budget ran out and the next delivery continues).
async fn on_artifacts_event(ctx: &Ctx, body: Value) -> ApiResult<bool> {
    let Ok(event) = serde_json::from_value::<ArtifactsEvent>(body) else {
        worker::console_warn!("ignoring a queue message that is not an Artifacts event");
        return Ok(true);
    };
    let activity_ref = format!("refs/heads/{ACTIVITY_BRANCH}");
    let ours = event.source.namespace.as_deref().is_none_or(|ns| ns == ctx.namespace);
    let activity = event.payload.reference.as_deref().is_none_or(|r| r == activity_ref);
    let Some(name) = event.source.repo_name.filter(|_| event.kind == PUSHED && ours && activity)
    else {
        return Ok(true);
    };
    if name.ends_with(naming::LOGS_SUFFIX) {
        return Ok(true);
    }
    let Some(repo) = ctx.db.repo(&name).await?.filter(|r| r.role != "logs") else {
        return Ok(true);
    };
    let Some(project) = ctx.db.project(&repo.project_id).await? else { return Ok(true) };
    let done = indexer::index_repo(&ctx.artifacts, &ctx.db, &project, &repo, ctx.budget).await?;
    worker::console_log!(
        "indexed {name} at {:?}: {} new events",
        done.report.tip,
        done.report.new_events
    );
    Ok(done.complete)
}

#[event(queue)]
async fn queue(batch: MessageBatch<Value>, env: Env, _ctx: Context) -> worker::Result<()> {
    let ctx = Ctx::new(env)?;
    for msg in batch.messages()? {
        match on_artifacts_event(&ctx, msg.body().clone()).await {
            Ok(true) => msg.ack(),
            Ok(false) => msg.retry(),
            Err(e) => {
                worker::console_error!("indexing failed: {} {}", e.status, e.message);
                msg.retry();
            }
        }
    }
    Ok(())
}

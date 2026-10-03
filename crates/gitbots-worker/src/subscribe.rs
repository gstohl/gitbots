//! Artifacts event subscriptions are per repo: the API requires
//! `source.namespace` and `source.repo_name` for `artifacts.repo`, and
//! wrangler 4.147 has no flags for them. So each new `<prj>` and fork gets
//! its own subscription of `pushed` events to the events Queue, through the
//! Cloudflare REST API.
//!
//! Needs the `CF_API_TOKEN` secret (an API token allowed to manage event
//! subscriptions, e.g. "Queues Write") and the `EVENTS_QUEUE_ID` var.
//! Without them this is a no-op and new repos are indexed by `/v1/ingest`
//! (or a manual subscription, see README.md).

use serde_json::json;
use wasm_bindgen::JsValue;
use worker::{Fetch, Headers, Method, Request, RequestInit};

use crate::Ctx;

/// Subscribes the events Queue to pushes of `repo`. Failures are logged,
/// never fatal: indexing still works on demand.
pub async fn subscribe_pushes(ctx: &Ctx, repo: &str) {
    let (Ok(token), Ok(queue)) = (ctx.env.secret("CF_API_TOKEN"), ctx.env.var("EVENTS_QUEUE_ID"))
    else {
        return;
    };
    match send(ctx, &token.to_string(), &queue.to_string(), repo).await {
        Ok(200..=299) => worker::console_log!("subscribed the events queue to pushes of {repo}"),
        Ok(status) => worker::console_warn!("subscribing {repo} to push events: HTTP {status}"),
        Err(e) => worker::console_warn!("subscribing {repo} to push events: {e}"),
    }
}

async fn send(ctx: &Ctx, token: &str, queue: &str, repo: &str) -> worker::Result<u16> {
    let url = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/event_subscriptions/subscriptions",
        ctx.account_id
    );
    let body = json!({
        "name": format!("{}-{repo}-pushed", ctx.namespace),
        "enabled": true,
        "source": {"type": "artifacts.repo", "namespace": ctx.namespace, "repo_name": repo},
        "destination": {"type": "queues.queue", "queue_id": queue},
        "events": ["pushed"],
    });
    let headers = Headers::new();
    headers.set("authorization", &format!("Bearer {token}"))?;
    headers.set("content-type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(JsValue::from_str(&body.to_string())));
    let resp = Fetch::Request(Request::new_with_init(&url, &init)?).send().await?;
    Ok(resp.status_code())
}

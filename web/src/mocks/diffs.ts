// Realistic unified diffs for the mock attempts. Built from file contents so
// the hunk headers' line counts are always right.

import { sha } from "./ids";

const idx = (seed: string) => sha(seed).slice(0, 7);

function lines(s: string): string[] {
  const out = s.replace(/^\n/, "").split("\n");
  if (out[out.length - 1] === "") out.pop();
  return out;
}

export function addedFile(path: string, content: string): string {
  const body = lines(content);
  return [
    `diff --git a/${path} b/${path}`,
    "new file mode 100644",
    `index 0000000..${idx(path)}`,
    "--- /dev/null",
    `+++ b/${path}`,
    `@@ -0,0 +1,${body.length} @@`,
    ...body.map((l) => `+${l}`),
  ].join("\n");
}

export function deletedFile(path: string, content: string): string {
  const body = lines(content);
  return [
    `diff --git a/${path} b/${path}`,
    "deleted file mode 100644",
    `index ${idx(path)}..0000000`,
    `--- a/${path}`,
    "+++ /dev/null",
    `@@ -1,${body.length} +0,0 @@`,
    ...body.map((l) => `-${l}`),
  ].join("\n");
}

export type Hunk = { oldStart: number; newStart: number; section?: string; body: string };

/** `body` lines start with " ", "+" or "-" (a bare empty line is context). */
export function hunk(h: Hunk): string {
  const ls = lines(h.body).map((l) => (l === "" ? " " : l));
  const oldN = ls.filter((l) => l[0] === " " || l[0] === "-").length;
  const newN = ls.filter((l) => l[0] === " " || l[0] === "+").length;
  return [`@@ -${h.oldStart},${oldN} +${h.newStart},${newN} @@${h.section ? ` ${h.section}` : ""}`, ...ls].join("\n");
}

export function modifiedFile(path: string, hunks: Hunk[], opts: { from?: string; similarity?: number } = {}): string {
  const from = opts.from ?? path;
  const head = [`diff --git a/${from} b/${path}`];
  if (opts.from) head.push(`similarity index ${opts.similarity ?? 90}%`, `rename from ${from}`, `rename to ${path}`);
  head.push(`index ${idx(`${from}-old`)}..${idx(`${path}-new`)} 100644`, `--- a/${from}`, `+++ b/${path}`);
  return [...head, ...hunks.map(hunk)].join("\n");
}

export function binaryFile(path: string): string {
  return [
    `diff --git a/${path} b/${path}`,
    "new file mode 100644",
    `index 0000000..${idx(path)}`,
    `Binary files /dev/null and b/${path} differ`,
  ].join("\n");
}

export function joinDiff(files: string[]): string {
  return `${files.join("\n")}\n`;
}

// ---- A1: token-bucket rate limiting (claude-opus) ----------------------------

export const DIFF_RATE_LIMIT = joinDiff([
  modifiedFile("Cargo.toml", [
    {
      oldStart: 18,
      newStart: 18,
      section: "[dependencies]",
      body: `
 axum = { version = "0.9", features = ["macros"] }
 bytes = "1.8"
+dashmap = "6.1"
 serde = { version = "1", features = ["derive"] }
 serde_json = "1"
-tokio = { version = "1.47", features = ["rt-multi-thread", "macros"] }
+tokio = { version = "1.47", features = ["rt-multi-thread", "macros", "time"] }
 tower = "0.5"
 tracing = "0.1"
`,
    },
  ]),
  modifiedFile("src/config.rs", [
    {
      oldStart: 9,
      newStart: 9,
      section: "pub struct Config {",
      body: `
     pub database_url: String,
     pub listen: SocketAddr,
     pub log_format: LogFormat,
-    /// Requests per minute per API key. 0 disables the limit.
-    pub rate_limit: u32,
+    pub rate_limit: RateLimitConfig,
+}
+
+/// Token bucket per API key: \`burst\` tokens, refilled at \`per_second\`.
+#[derive(Clone, Debug, Deserialize)]
+pub struct RateLimitConfig {
+    #[serde(default = "default_burst")]
+    pub burst: u32,
+    #[serde(default = "default_per_second")]
+    pub per_second: f64,
+    /// Keys exempt from limiting (internal jobs).
+    #[serde(default)]
+    pub exempt_keys: Vec<String>,
 }
`,
    },
    {
      oldStart: 41,
      newStart: 54,
      section: "impl Config {",
      body: `
             listen: env_or("LISTEN", "127.0.0.1:8080")?.parse()?,
             log_format: env_or("LOG_FORMAT", "pretty")?.parse()?,
-            rate_limit: env_or("RATE_LIMIT", "600")?.parse()?,
+            rate_limit: RateLimitConfig {
+                burst: env_or("RATE_LIMIT_BURST", "60")?.parse()?,
+                per_second: env_or("RATE_LIMIT_PER_SECOND", "10")?.parse()?,
+                exempt_keys: env_list("RATE_LIMIT_EXEMPT_KEYS"),
+            },
         })
     }
 }
+
+fn default_burst() -> u32 {
+    60
+}
+
+fn default_per_second() -> f64 {
+    10.0
+}
`,
    },
  ]),
  modifiedFile("src/main.rs", [
    {
      oldStart: 1,
      newStart: 1,
      body: `
 use std::sync::Arc;

 use billing::{config::Config, db, routes};
+use billing::middleware::rate_limit::{RateLimitLayer, RateLimiter};
 use tracing::info;
`,
    },
    {
      oldStart: 22,
      newStart: 23,
      section: "async fn main() -> anyhow::Result<()> {",
      body: `
     let pool = db::connect(&config.database_url).await?;
     let state = Arc::new(routes::AppState::new(pool));
+    let limiter = RateLimiter::new(config.rate_limit.clone());
+    limiter.spawn_janitor(std::time::Duration::from_secs(60));

-    let app = routes::router(state);
+    let app = routes::router(state).layer(RateLimitLayer::new(limiter));
     let listener = tokio::net::TcpListener::bind(config.listen).await?;
     info!(addr = %config.listen, "listening");
     axum::serve(listener, app).await?;
`,
    },
  ]),
  modifiedFile("src/middleware/mod.rs", [
    {
      oldStart: 1,
      newStart: 1,
      body: `
 pub mod auth;
 pub mod request_id;
+pub mod rate_limit;
 pub mod tracing;
`,
    },
  ]),
  addedFile(
    "src/middleware/rate_limit.rs",
    `
//! Per-API-key token buckets.
//!
//! Each key gets \`burst\` tokens that refill continuously at \`per_second\`.
//! A request takes one token; with none left we answer 429 with a
//! \`Retry-After\` header instead of queueing.

use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::http::{HeaderValue, Request, Response, StatusCode};
use dashmap::DashMap;
use tokio::time::Instant;
use tower::{Layer, Service};

use crate::config::RateLimitConfig;
use crate::middleware::auth::ApiKey;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

impl Bucket {
    fn full(burst: u32, now: Instant) -> Self {
        Self { tokens: f64::from(burst), updated: now }
    }

    /// Refills, then takes a token. Returns how long to wait when empty.
    fn take(&mut self, cfg: &RateLimitConfig, now: Instant) -> Result<(), Duration> {
        let elapsed = now.duration_since(self.updated).as_secs_f64();
        self.tokens = (self.tokens + elapsed * cfg.per_second).min(f64::from(cfg.burst));
        self.updated = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            let missing = 1.0 - self.tokens;
            Err(Duration::from_secs_f64(missing / cfg.per_second))
        }
    }
}

#[derive(Clone)]
pub struct RateLimiter {
    cfg: Arc<RateLimitConfig>,
    buckets: Arc<DashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn new(cfg: RateLimitConfig) -> Self {
        Self { cfg: Arc::new(cfg), buckets: Arc::new(DashMap::new()) }
    }

    pub fn check(&self, key: &str) -> Result<(), Duration> {
        if self.cfg.exempt_keys.iter().any(|k| k == key) {
            return Ok(());
        }
        let now = Instant::now();
        let mut bucket = self
            .buckets
            .entry(key.to_owned())
            .or_insert_with(|| Bucket::full(self.cfg.burst, now));
        bucket.take(&self.cfg, now)
    }

    /// Drops buckets that have been full for a while, so memory stays bounded.
    pub fn spawn_janitor(&self, every: Duration) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
                let now = Instant::now();
                let full = f64::from(this.cfg.burst);
                this.buckets.retain(|_, b| {
                    let refilled = b.tokens + now.duration_since(b.updated).as_secs_f64() * this.cfg.per_second;
                    refilled < full
                });
            }
        });
    }
}

#[derive(Clone)]
pub struct RateLimitLayer {
    limiter: RateLimiter,
}

impl RateLimitLayer {
    pub fn new(limiter: RateLimiter) -> Self {
        Self { limiter }
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimit { inner, limiter: self.limiter.clone() }
    }
}

#[derive(Clone)]
pub struct RateLimit<S> {
    inner: S,
    limiter: RateLimiter,
}

impl<S, B> Service<Request<B>> for RateLimit<S>
where
    S: Service<Request<B>, Response = Response<axum::body::Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = futures::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let key = req.extensions().get::<ApiKey>().map(|k| k.id().to_owned());
        if let Some(key) = key {
            if let Err(wait) = self.limiter.check(&key) {
                return Box::pin(async move { Ok(too_many_requests(wait)) });
            }
        }
        let fut = self.inner.call(req);
        Box::pin(fut)
    }
}

fn too_many_requests(wait: Duration) -> Response<axum::body::Body> {
    let secs = wait.as_secs_f64().ceil().max(1.0) as u64;
    let mut res = Response::new(axum::body::Body::from(
        r#"{"error":{"type":"rate_limited","message":"Too many requests"}}"#,
    ));
    *res.status_mut() = StatusCode::TOO_MANY_REQUESTS;
    let headers = res.headers_mut();
    headers.insert("retry-after", HeaderValue::from(secs));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    res
}
`,
  ),
  modifiedFile(
    "src/clock.rs",
    [
      {
        oldStart: 1,
        newStart: 1,
        body: `
-//! Time helpers for the invoice scheduler.
+//! Time helpers shared by the scheduler and the rate limiter.

 use time::OffsetDateTime;
`,
      },
    ],
    { from: "src/util/time.rs", similarity: 94 },
  ),
  addedFile(
    "tests/rate_limit.rs",
    `
use std::time::Duration;

use billing::config::RateLimitConfig;
use billing::middleware::rate_limit::RateLimiter;

fn limiter(burst: u32, per_second: f64) -> RateLimiter {
    RateLimiter::new(RateLimitConfig { burst, per_second, exempt_keys: vec!["internal".into()] })
}

#[tokio::test(start_paused = true)]
async fn allows_a_full_burst_then_limits() {
    let l = limiter(3, 1.0);
    for _ in 0..3 {
        assert!(l.check("key_a").is_ok());
    }
    let wait = l.check("key_a").unwrap_err();
    assert!(wait <= Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn refills_over_time() {
    let l = limiter(1, 2.0);
    assert!(l.check("key_a").is_ok());
    assert!(l.check("key_a").is_err());
    tokio::time::advance(Duration::from_millis(500)).await;
    assert!(l.check("key_a").is_ok());
}

#[tokio::test(start_paused = true)]
async fn keys_are_independent() {
    let l = limiter(1, 0.1);
    assert!(l.check("key_a").is_ok());
    assert!(l.check("key_b").is_ok());
    assert!(l.check("key_a").is_err());
}

#[tokio::test(start_paused = true)]
async fn exempt_keys_are_never_limited() {
    let l = limiter(1, 0.1);
    for _ in 0..100 {
        assert!(l.check("internal").is_ok());
    }
}
`,
  ),
  addedFile(
    "docs/api/rate-limits.md",
    `
# Rate limits

Every API key gets a token bucket: **60 requests of burst**, refilled at
**10 requests per second**. When the bucket is empty the API answers
\`429 Too Many Requests\` with a \`Retry-After\` header (seconds).

| variable                  | default | meaning                         |
|---------------------------|---------|---------------------------------|
| \`RATE_LIMIT_BURST\`        | 60      | bucket size                     |
| \`RATE_LIMIT_PER_SECOND\`   | 10      | refill rate                     |
| \`RATE_LIMIT_EXEMPT_KEYS\`  | (none)  | comma-separated exempt key ids  |
`,
  ),
  binaryFile("docs/img/rate-limit-burst.png"),
]);

// ---- A3: webhook retry flake (codex) --------------------------------------------

export const DIFF_WEBHOOK = joinDiff([
  modifiedFile("src/webhooks/retry.rs", [
    {
      oldStart: 58,
      newStart: 58,
      section: "impl RetryScheduler {",
      body: `
     pub async fn schedule(&self, delivery: DeliveryId, attempt: u32) {
-        let delay = self.backoff(attempt);
-        tokio::time::sleep(delay).await;
-        self.queue.push(delivery).await;
+        let delay = self.backoff(attempt);
+        if !self.pending.insert(delivery) {
+            // Already scheduled: a duplicate 5xx must not double the retries.
+            return;
+        }
+        let queue = self.queue.clone();
+        let pending = self.pending.clone();
+        tokio::spawn(async move {
+            tokio::time::sleep(delay).await;
+            pending.remove(&delivery);
+            queue.push(delivery).await;
+        });
     }
`,
    },
    {
      oldStart: 196,
      newStart: 205,
      section: "mod tests {",
      body: `
-    #[tokio::test]
+    #[tokio::test(start_paused = true)]
     async fn retries_on_503() {
         let (sched, sink) = scheduler_with_sink();
         sink.respond_with(503).times(2);
         sched.deliver(delivery()).await;
-        tokio::time::sleep(Duration::from_millis(1_500)).await;
+        tokio::time::advance(Duration::from_secs(8)).await;
         assert_eq!(sink.calls(), 3);
     }
`,
    },
  ]),
]);

// ---- small diffs for the other attempts ------------------------------------------

export function smallDiff(seed: string): string {
  return joinDiff([
    modifiedFile(`src/${seed}.rs`, [
      {
        oldStart: 10,
        newStart: 10,
        section: "fn handle()",
        body: `
     let ctx = Context::current();
-    let result = legacy::run(&ctx);
+    let result = run(&ctx).await?;
+    tracing::debug!(?result, "handled");
     Ok(result)
`,
      },
    ]),
  ]);
}

export const DIFF_CI_CACHE = joinDiff([
  modifiedFile(".github/workflows/ci.yml", [
    {
      oldStart: 14,
      newStart: 14,
      section: "jobs:",
      body: `
       - uses: actions/checkout@v5
       - uses: dtolnay/rust-toolchain@stable
+      - uses: actions/cache@v4
+        with:
+          path: |
+            ~/.cargo/registry
+            ~/.cargo/git
+          key: cargo-\${{ hashFiles('Cargo.lock') }}
       - run: cargo test --workspace
`,
    },
  ]),
  modifiedFile(".gitbots/actions/ci.toml", [
    {
      oldStart: 3,
      newStart: 3,
      body: `
 [jobs.test]
 runs-on = "local"
-timeout-secs = 900
+timeout-secs = 1800
`,
    },
  ]),
]);

/**
 * A ~5,000-line diff (dependency upgrade touching many handlers) to keep the
 * diff view honest about performance.
 */
export function bigUpgradeDiff(): string {
  const files: string[] = [];
  // Cargo.lock churn.
  const lock: string[] = [];
  for (let i = 0; i < 160; i++) {
    lock.push(
      ` [[package]]`,
      ` name = "crate-${i.toString(36)}"`,
      `-version = "0.${8 + (i % 3)}.${i % 7}"`,
      `+version = "0.${9 + (i % 3)}.${(i + 1) % 7}"`,
      ` source = "registry+https://github.com/rust-lang/crates.io-index"`,
      `-checksum = "${sha(`old${i}`)}${sha(`old${i}x`).slice(0, 24)}"`,
      `+checksum = "${sha(`new${i}`)}${sha(`new${i}x`).slice(0, 24)}"`,
      ``,
    );
  }
  files.push(modifiedFile("Cargo.lock", [{ oldStart: 1, newStart: 1, body: lock.join("\n") }]));
  for (let f = 0; f < 60; f++) {
    const body: string[] = [];
    for (let h = 0; h < 6; h++) {
      body.push(
        ` pub async fn handler_${f}_${h}(`,
        `-    State(state): State<Arc<AppState>>,`,
        `-    Path(id): Path<String>,`,
        `+    State(state): State<AppState>,`,
        `+    Path(id): Path<InvoiceId>,`,
        ` ) -> Result<Json<Invoice>, ApiError> {`,
        `     let invoice = state.invoices.get(&id).await?;`,
        `-    Ok(Json(invoice))`,
        `+    Ok(Json(invoice.into()))`,
        ` }`,
        ``,
      );
    }
    files.push(
      modifiedFile(`src/routes/v1/resource_${String(f).padStart(2, "0")}.rs`, [
        { oldStart: 20, newStart: 20, section: "use axum::extract::{Path, State};", body: body.join("\n") },
      ]),
    );
  }
  return joinDiff(files);
}

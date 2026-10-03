//! `gitbots cloud` and cloud `gitbots sync` against a mock Worker.
//!
//! The mock serves the `/v1` API and the "Artifacts" repos as git smart HTTP
//! (`git http-backend` over local bare repos). Every git request must carry a
//! bearer token the mock issued for that repo, so the tests see which session
//! each push was made as.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Barrier, Mutex, MutexGuard};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path as UrlPath, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;
use tokio::io::AsyncWriteExt;

use gitbots_cloud::api::{
    CreateProject, ForkCreated, ForkRequest, IngestReport, KnownRepo, OutboxAck, ProjectCreated,
    ProjectInfo, Remotes, RepoIngest, RepoRef, TokenIssued, TokenRequest, TokenScope,
};

const ADMIN: &str = "adm_test_admin_key";

// ---- the mock Worker --------------------------------------------------------

#[derive(Clone, Debug)]
struct Grant {
    repo: String,
    scope: TokenScope,
    session: Option<String>,
}

/// One authenticated git request.
#[derive(Clone, Debug)]
struct GitHit {
    repo: String,
    /// `POST .../git-receive-pack`: a push that sent data.
    push: bool,
    session: Option<String>,
}

#[derive(Default)]
struct MockState {
    owner_key: Option<String>,
    project: Option<ProjectInfo>,
    created: Vec<CreateProject>,
    /// Valid tokens.
    tokens: HashMap<String, Grant>,
    /// Every token ever issued, revoked or not.
    issued: Vec<String>,
    token_requests: Vec<TokenRequest>,
    forks: Vec<ForkRequest>,
    outbox: Vec<Value>,
    acks: Vec<(String, OutboxAck)>,
    ingests: usize,
    git: Vec<GitHit>,
    denied: usize,
    fail_tokens: usize,
    /// Holds `GET /v1/project` until this many callers wait on it.
    project_gate: Option<Arc<tokio::sync::Barrier>>,
    counter: u64,
}

struct Shared {
    state: Mutex<MockState>,
    root: PathBuf,
    gitconfig: PathBuf,
    url: String,
}

type Api<T> = Result<Json<T>, (StatusCode, Json<Value>)>;

fn fail(code: StatusCode, msg: &str) -> (StatusCode, Json<Value>) {
    (code, Json(json!({ "error": msg })))
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("authorization")?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(str::to_owned)
}

fn owner<'s>(
    s: &'s Shared,
    headers: &HeaderMap,
) -> Result<MutexGuard<'s, MockState>, (StatusCode, Json<Value>)> {
    let st = s.state.lock().unwrap();
    match (&st.owner_key, bearer(headers)) {
        (Some(key), Some(given)) if *key == given => Ok(st),
        _ => Err(fail(StatusCode::UNAUTHORIZED, "bad owner key")),
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").current_dir(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn expires(ttl: u64) -> String {
    let at = time::OffsetDateTime::now_utc() + time::Duration::seconds(ttl as i64);
    at.format(&Rfc3339).unwrap()
}

impl Shared {
    fn repo_dir(&self, repo: &str) -> PathBuf {
        self.root.join("gitbots").join(format!("{repo}.git"))
    }

    fn remote(&self, repo: &str) -> String {
        format!("{}/git/gitbots/{repo}.git", self.url)
    }

    fn init_repo(&self, repo: &str) {
        let dir = self.repo_dir(repo);
        std::fs::create_dir_all(&dir).unwrap();
        run_git(&dir, &["init", "-q", "--bare", "-b", "main"]);
    }

    fn issue(
        &self,
        st: &mut MockState,
        repo: &str,
        scope: TokenScope,
        session: Option<String>,
    ) -> String {
        st.counter += 1;
        let nanos = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
        let token = format!("art_v1_{}_{nanos:x}", st.counter);
        st.tokens.insert(token.clone(), Grant { repo: repo.to_owned(), scope, session });
        st.issued.push(token.clone());
        token
    }
}

async fn create_project(
    State(s): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(req): Json<CreateProject>,
) -> Api<ProjectCreated> {
    if bearer(&headers).as_deref() != Some(ADMIN) {
        return Err(fail(StatusCode::UNAUTHORIZED, "bad admin key"));
    }
    let mut st = s.state.lock().unwrap();
    if st.project.is_some() {
        return Err(fail(StatusCode::CONFLICT, "project exists"));
    }
    let repo = req.project_id.to_lowercase();
    s.init_repo(&repo);
    s.init_repo(&format!("{repo}-logs"));
    let remotes = Remotes { main: s.remote(&repo), logs: s.remote(&format!("{repo}-logs")) };
    let owner_key = format!("own_{}", time::OffsetDateTime::now_utc().unix_timestamp_nanos());
    st.owner_key = Some(owner_key.clone());
    st.project = Some(ProjectInfo {
        project_id: req.project_id.clone(),
        name: req.name.clone(),
        namespace: "gitbots".into(),
        remotes: remotes.clone(),
        created_at: "2026-10-03T10:00:00Z".into(),
    });
    st.created.push(req.clone());
    Ok(Json(ProjectCreated {
        project_id: req.project_id,
        owner_key,
        namespace: "gitbots".into(),
        remotes,
    }))
}

async fn project(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Api<ProjectInfo> {
    let gate = owner(&s, &headers)?.project_gate.clone();
    if let Some(gate) = gate {
        gate.wait().await;
        s.state.lock().unwrap().project_gate = None;
    }
    let st = owner(&s, &headers)?;
    Ok(Json(st.project.clone().unwrap()))
}

async fn tokens(
    State(s): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(req): Json<TokenRequest>,
) -> Api<TokenIssued> {
    let mut st = owner(&s, &headers)?;
    st.token_requests.push(req.clone());
    if st.fail_tokens > 0 {
        st.fail_tokens -= 1;
        return Err(fail(StatusCode::SERVICE_UNAVAILABLE, "try again"));
    }
    if req.session.as_deref().is_some_and(|s| !s.starts_with("ses_")) {
        return Err(fail(StatusCode::UNPROCESSABLE_ENTITY, "bad session"));
    }
    let base = st.project.as_ref().unwrap().project_id.to_lowercase();
    let repo = match &req.repo {
        RepoRef::Known(KnownRepo::Main) => base,
        RepoRef::Known(KnownRepo::Logs) => format!("{base}-logs"),
        RepoRef::Fork(fork) => fork.clone(),
    };
    let token = s.issue(&mut st, &repo, req.scope, req.session.clone());
    Ok(Json(TokenIssued {
        token,
        expires_at: expires(req.ttl_secs.unwrap_or(3600)),
        remote: s.remote(&repo),
    }))
}

async fn forks(
    State(s): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(req): Json<ForkRequest>,
) -> Api<ForkCreated> {
    let mut st = owner(&s, &headers)?;
    st.forks.push(req.clone());
    let base = st.project.as_ref().unwrap().project_id.to_lowercase();
    let repo = format!("{base}-{}", req.attempt.to_lowercase());
    s.init_repo(&repo);
    let token = s.issue(&mut st, &repo, TokenScope::Write, req.session.clone());
    Ok(Json(ForkCreated { remote: s.remote(&repo), repo, token, expires_at: expires(3600) }))
}

async fn ingest(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Api<IngestReport> {
    let mut st = owner(&s, &headers)?;
    st.ingests += 1;
    let repo = st.project.as_ref().unwrap().project_id.to_lowercase();
    Ok(Json(IngestReport { repos: vec![RepoIngest { repo, tip: None, new_events: 0 }] }))
}

async fn outbox(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Api<Vec<Value>> {
    let st = owner(&s, &headers)?;
    Ok(Json(st.outbox.clone()))
}

async fn ack(
    State(s): State<Arc<Shared>>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    Json(ack): Json<OutboxAck>,
) -> Api<Value> {
    let mut st = owner(&s, &headers)?;
    st.outbox.retain(|item| item["id"] != id.as_str());
    st.acks.push((id, ack));
    Ok(Json(json!({})))
}

/// Git smart HTTP: check the token, then hand the request to `git http-backend`.
async fn git_http(
    State(s): State<Arc<Shared>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().strip_prefix("/git").unwrap_or_default().to_owned();
    let repo = path.split('/').nth(2).and_then(|r| r.strip_suffix(".git")).unwrap_or_default();
    let query = uri.query().unwrap_or_default().to_owned();
    let push = path.ends_with("/git-receive-pack");
    let write = push || query.contains("service=git-receive-pack");
    {
        let mut st = s.state.lock().unwrap();
        let grant = bearer(&headers).and_then(|t| st.tokens.get(&t).cloned());
        match grant {
            Some(g) if g.repo == repo && (!write || g.scope == TokenScope::Write) => {
                st.git.push(GitHit { repo: repo.to_owned(), push, session: g.session });
            }
            _ => {
                st.denied += 1;
                return (StatusCode::UNAUTHORIZED, "token required").into_response();
            }
        }
    }

    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("");
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("http-backend")
        .env("GIT_PROJECT_ROOT", &s.root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", &path)
        .env("REQUEST_METHOD", method.as_str())
        .env("QUERY_STRING", &query)
        .env("CONTENT_TYPE", header("content-type"))
        .env("CONTENT_LENGTH", body.len().to_string())
        .env("REMOTE_USER", "gitbots")
        .env("REMOTE_ADDR", "127.0.0.1")
        .env("GIT_CONFIG_GLOBAL", &s.gitconfig)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (var, name) in
        [("HTTP_CONTENT_ENCODING", "content-encoding"), ("GIT_PROTOCOL", "git-protocol")]
    {
        if !header(name).is_empty() {
            cmd.env(var, header(name));
        }
    }
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(&body).await;
    });
    let out = child.wait_with_output().await.unwrap();
    let _ = writer.await;

    let raw = out.stdout;
    let (head_len, sep) = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => (i, 4),
        None => (raw.windows(2).position(|w| w == b"\n\n").unwrap_or(raw.len()), 2),
    };
    let head = String::from_utf8_lossy(&raw[..head_len]).into_owned();
    let body = raw.get(head_len + sep..).unwrap_or_default().to_vec();
    let mut resp = Response::builder();
    let mut status = 200;
    for line in head.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("status") {
                status = v.trim()[..3].parse().unwrap();
            } else {
                resp = resp.header(k.trim(), v.trim());
            }
        }
    }
    resp.status(status).body(Body::from(body)).unwrap()
}

struct Mock {
    url: String,
    shared: Arc<Shared>,
}

impl Mock {
    fn start(root: &Path, gitconfig: &Path) -> Mock {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            root: root.to_owned(),
            gitconfig: gitconfig.to_owned(),
            url: url.clone(),
        });
        let app = Router::new()
            .route("/v1/projects", post(create_project))
            .route("/v1/project", get(project))
            .route("/v1/tokens", post(tokens))
            .route("/v1/forks", post(forks))
            .route("/v1/ingest", post(ingest))
            .route("/v1/outbox", get(outbox))
            .route("/v1/outbox/{id}/ack", post(ack))
            .route("/git/{*rest}", any(git_http))
            .layer(DefaultBodyLimit::disable())
            .with_state(Arc::clone(&shared));
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        Mock { url, shared }
    }

    fn state(&self) -> MutexGuard<'_, MockState> {
        self.shared.state.lock().unwrap()
    }

    fn base(&self) -> String {
        self.state().project.as_ref().unwrap().project_id.to_lowercase()
    }

    /// The bare repo behind `<prj>` (`main`) or `<prj>-logs` (`logs`).
    fn bare(&self, which: &str) -> PathBuf {
        let base = self.base();
        match which {
            "main" => self.shared.repo_dir(&base),
            "logs" => self.shared.repo_dir(&format!("{base}-logs")),
            other => self.shared.repo_dir(other),
        }
    }

    fn queue(&self, items: Value) {
        self.state().outbox.extend(items.as_array().unwrap().iter().cloned());
    }

    fn revoke_all(&self) {
        self.state().tokens.clear();
    }

    fn token_requests_for(&self, session: Option<&str>) -> usize {
        self.state().token_requests.iter().filter(|r| r.session.as_deref() == session).count()
    }
}

// ---- the world ----------------------------------------------------------------

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    config_dir: PathBuf,
    gitconfig: PathBuf,
    /// Output of every gitbots run that must not show a token.
    outputs: Mutex<Vec<String>>,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        let gitconfig = root.join("gitconfig");
        std::fs::create_dir_all(&repo).unwrap();
        // No credential helper: a refused token must fail, not prompt.
        std::fs::write(&gitconfig, "[credential]\n\thelper =\n").unwrap();
        let w = World {
            home: root.join("gitbots-home"),
            config_dir: root.join("config"),
            repo,
            gitconfig,
            root,
            _tmp: tmp,
            outputs: Mutex::default(),
        };
        w.git(&w.repo, &["init", "-q", "-b", "main"]);
        w.git(&w.repo, &["config", "user.name", "gstohl"]);
        w.git(&w.repo, &["config", "user.email", "dominik@example.com"]);
        std::fs::write(w.repo.join("README.md"), "# demo\n").unwrap();
        w.git(&w.repo, &["add", "."]);
        w.git(&w.repo, &["commit", "-q", "-m", "initial"]);
        w
    }

    fn env(&self, cmd: &mut Command) {
        for (k, _) in std::env::vars() {
            let lower = k.to_ascii_lowercase();
            if k == "CLAUDECODE"
                || k.starts_with("CLAUDE_CODE")
                || k.starts_with("CODEX_")
                || k.starts_with("GITBOTS_")
                || k.starts_with("GIT_")
                || lower.ends_with("_proxy")
                || k == "XDG_CONFIG_HOME"
            {
                cmd.env_remove(k);
            }
        }
        cmd.env("GITBOTS_HOME", &self.home)
            .env("GITBOTS_CONFIG_DIR", &self.config_dir)
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1");
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        cmd.current_dir(dir).args(args);
        self.env(&mut cmd);
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn git_env(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) {
        let mut cmd = Command::new("git");
        cmd.current_dir(dir).args(args);
        self.env(&mut cmd);
        cmd.envs(env.iter().copied());
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn command(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_gitbots"));
        cmd.current_dir(dir).args(args);
        self.env(&mut cmd);
        cmd.envs(env.iter().copied());
        cmd
    }

    fn record(&self, out: &Output) {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        self.outputs.lock().unwrap().push(text);
    }

    fn gitbots_raw(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let out = self.command(dir, args, env).output().unwrap();
        self.record(&out);
        out
    }

    fn parse(args: &[&str], out: &Output) -> Value {
        assert!(
            out.status.success(),
            "gitbots {args:?} failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!("gitbots {args:?}: bad json ({e}): {}", String::from_utf8_lossy(&out.stdout))
        })
    }

    /// `gitbots --json <args>`; asserts success and no warnings on stderr.
    fn gitbots(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = self.gitbots_raw(dir, &all, env);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("warning"), "gitbots {args:?} warned: {stderr}");
        Self::parse(args, &out)
    }

    /// Like [`World::gitbots`] for commands that print a token on purpose.
    fn gitbots_with_token(&self, args: &[&str], env: &[(&str, &str)]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = self.command(&self.repo, &all, env).output().unwrap();
        Self::parse(args, &out)
    }

    fn session(&self, provider: &str) -> String {
        let started = self.gitbots(
            &self.repo,
            &["session", "start", "--provider", provider, "--model", "m", "--client", "test"],
            &[],
        );
        started["id"].as_str().unwrap().to_owned()
    }

    /// An gitbots project, set up for gitbots cloud on a fresh mock Worker.
    fn cloud() -> (World, Mock) {
        let w = World::new();
        let mock = Mock::start(&w.root.join("artifacts"), &w.gitconfig);
        w.gitbots(&w.repo, &["init", "--commit"], &[]);
        w.gitbots(&w.repo, &["cloud", "init", "--url", &mock.url], &[("GITBOTS_ADMIN_KEY", ADMIN)]);
        (w, mock)
    }

    /// No token the mock ever issued is in `.git/config`, the credentials
    /// file or any recorded output; cached tokens are private.
    fn assert_no_token_leaks(&self, mock: &Mock) {
        let issued = mock.state().issued.clone();
        assert!(!issued.is_empty());
        let config = std::fs::read_to_string(self.repo.join(".git/config")).unwrap();
        assert!(!config.to_ascii_lowercase().contains("extraheader"), "{config}");
        let creds = std::fs::read_to_string(self.config_dir.join("credentials.toml")).unwrap();
        let outputs = self.outputs.lock().unwrap();
        for token in &issued {
            assert!(!config.contains(token.as_str()), "token in .git/config");
            assert!(!creds.contains(token.as_str()), "token in credentials.toml");
            for out in outputs.iter() {
                assert!(!out.contains(token.as_str()), "token in output: {out}");
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&self.config_dir.join("credentials.toml")), 0o600);
            let tokens = self.repo.join(".git/gitbots/tokens");
            assert_eq!(mode(&tokens), 0o700);
            for entry in std::fs::read_dir(&tokens).unwrap() {
                assert_eq!(mode(&entry.unwrap().path()), 0o600);
            }
        }
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_else(|| panic!("not a string: {v}")).to_owned()
}

/// Event files on `gitbots/activity` of a fresh clone of `bare`, by path.
fn remote_events(w: &World, bare: &Path) -> Vec<String> {
    let clone = w.root.join(format!("clone-{}", w.outputs.lock().unwrap().len()));
    w.git(&w.root, &["clone", "-q", "--mirror", bare.to_str().unwrap(), clone.to_str().unwrap()]);
    w.git(&clone, &["ls-tree", "-r", "--name-only", "gitbots/activity", "--", "events"])
        .lines()
        .filter(|p| p.ends_with(".json"))
        .map(str::to_owned)
        .collect()
}

/// Starts a task and an attempt as `session`, commits `hello.txt` in the
/// workroom and submits. Returns `(attempt id, attempt branch)`.
fn submitted_attempt(w: &World, session: &str) -> (String, String) {
    let task = s(&w.gitbots(
        &w.repo,
        &["task", "create", "Add hello"],
        &[("GITBOTS_SESSION", session)],
    )["task"]);
    let info = w.gitbots(&w.repo, &["attempt", "start", &task], &[("GITBOTS_SESSION", session)]);
    let room = PathBuf::from(s(&info["workroom"]));
    std::fs::write(room.join("hello.txt"), "hello\n").unwrap();
    w.git(&room, &["add", "hello.txt"]);
    w.git(&room, &["commit", "-q", "-m", "Add hello"]);
    w.gitbots(&room, &["attempt", "submit", "--summary", "adds hello"], &[]);
    (s(&info["attempt"]), s(&info["branch"]))
}

// ---- tests ----------------------------------------------------------------------

#[test]
fn cloud_init_provisions_pushes_everything_and_is_idempotent() {
    let w = World::new();
    let mock = Mock::start(&w.root.join("artifacts"), &w.gitconfig);
    w.gitbots(&w.repo, &["init", "--commit"], &[]);
    let agent = w.session("anthropic");
    let (_, branch) = submitted_attempt(&w, &agent);

    // Provisioning needs the admin key.
    let err = w.gitbots_raw(&w.repo, &["cloud", "init", "--url", &mock.url], &[]);
    assert!(String::from_utf8_lossy(&err.stderr).contains("GITBOTS_ADMIN_KEY"));
    let key_file = w.root.join("admin.key");
    std::fs::write(&key_file, format!("{ADMIN}\n")).unwrap();
    let init = w.gitbots(
        &w.repo,
        &[
            "cloud",
            "init",
            "--url",
            &format!("{}/", mock.url),
            "--admin-key-file",
            key_file.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(init["created"], true, "{init}");

    let manifest: Value =
        serde_json::from_str(&w.git(&w.repo, &["show", "main:.gitbots/manifest.json"])).unwrap();
    let created = mock.state().created.clone();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].project_id, s(&manifest["project"]["id"]));
    assert_eq!(created[0].name, s(&manifest["project"]["name"]));

    // Everything is on the hosted repos: code + activity on main, logs on logs.
    let (main, logs) = (mock.bare("main"), mock.bare("logs"));
    for b in ["main", branch.as_str(), "gitbots/activity"] {
        assert_eq!(w.git(&main, &["rev-parse", b]), w.git(&w.repo, &["rev-parse", b]), "{b}");
    }
    assert_eq!(
        w.git(&logs, &["rev-parse", "gitbots/logs"]),
        w.git(&w.repo, &["rev-parse", "gitbots/logs"])
    );
    assert!(w.git(&main, &["branch", "--list", "gitbots/logs"]).is_empty());
    assert!(w.git(&logs, &["branch", "--list", "main"]).is_empty());
    {
        let st = mock.state();
        // Pushed with human tokens: no session.
        assert!(
            st.token_requests.iter().all(|r| r.session.is_none() && r.scope == TokenScope::Write)
        );
        assert!(st.git.iter().all(|h| h.session.is_none()));
        let pushed: Vec<&str> = st.git.iter().filter(|h| h.push).map(|h| h.repo.as_str()).collect();
        assert!(
            pushed.iter().any(|r| r.ends_with("-logs"))
                && pushed.iter().any(|r| !r.ends_with("-logs"))
        );
        assert_eq!(st.ingests, 1);
    }

    // Config: plain remotes and cloud keys in git config, the owner key in credentials.toml.
    assert_eq!(w.git(&w.repo, &["config", "gitbots.cloud.url"]), mock.url);
    assert_eq!(
        w.git(&w.repo, &["remote", "get-url", "gitbots-main"]),
        format!("{}/git/gitbots/{}.git", mock.url, mock.base())
    );
    assert_eq!(
        w.git(&w.repo, &["remote", "get-url", "gitbots-logs"]),
        format!("{}/git/gitbots/{}-logs.git", mock.url, mock.base())
    );
    let owner_key = mock.state().owner_key.clone().unwrap();
    let creds = std::fs::read_to_string(w.config_dir.join("credentials.toml")).unwrap();
    assert!(
        creds.contains(&format!("{}#{}", mock.url, s(&manifest["project"]["id"])))
            && creds.contains(&owner_key)
    );
    let link = format!("{}/#token={owner_key}", mock.url);
    assert_eq!(init["dashboard"], link.as_str());
    assert_eq!(w.gitbots(&w.repo, &["cloud", "dashboard"], &[])["dashboard"], link.as_str());

    // Again, without the admin key: only verified.
    let again = w.gitbots(&w.repo, &["cloud", "init", "--url", &mock.url], &[]);
    assert_eq!(again["created"], false);
    assert_eq!(again["verified_only"], true);
    assert_eq!(mock.state().created.len(), 1);

    // Logs sync with gitbots-logs on request, as the acting session.
    let log = w.root.join("tool.log");
    std::fs::write(&log, "ran the tests\n").unwrap();
    let env = [("GITBOTS_SESSION", agent.as_str())];
    w.gitbots(&w.repo, &["trace", "cargo test", "--log-file", log.to_str().unwrap()], &env);
    let sync = w.gitbots(&w.repo, &["sync", "--logs"], &env);
    assert_eq!(sync["ledgers"][1]["branch"], "gitbots/logs", "{sync}");
    assert_eq!(sync["ledgers"][1]["pushed"], true, "{sync}");
    assert_eq!(
        w.git(&logs, &["rev-parse", "gitbots/logs"]),
        w.git(&w.repo, &["rev-parse", "gitbots/logs"])
    );
    let logs_push =
        mock.state().git.iter().rev().find(|h| h.push && h.repo.ends_with("-logs")).cloned();
    assert_eq!(logs_push.unwrap().session.as_deref(), Some(agent.as_str()));
    let err = w.gitbots_raw(&w.repo, &["cloud", "init", "--url", &mock.url], &env);
    assert!(String::from_utf8_lossy(&err.stderr).contains("acts as the human"));

    let status = w.gitbots(&w.repo, &["cloud", "status"], &[]);
    assert_eq!(status["configured"], true);
    assert_eq!(status["pending_outbox"], 0);
    assert_eq!(status["info"]["project_id"], manifest["project"]["id"]);
    assert_eq!(status["key"]["from"], "file");
    w.assert_no_token_leaks(&mock);
}

#[test]
fn sync_applies_the_outbox_as_the_human_and_merges() {
    let (w, mock) = World::cloud();
    let agent = w.session("anthropic");
    let (attempt, branch) = submitted_attempt(&w, &agent);
    // Auto-sync published the attempt branch, attested as the agent's session.
    let main = mock.bare("main");
    assert_eq!(w.git(&main, &["rev-parse", &branch]), w.git(&w.repo, &["rev-parse", &branch]));
    assert!(
        mock.state().git.iter().any(|h| h.push && h.session.as_deref() == Some(agent.as_str()))
    );

    let actor = json!({"type": "human", "handle": "gstohl"});
    mock.queue(json!([
        {"id": "obx_bad", "created_at": "2026-10-03T10:00:00Z", "kind": "review",
         "body": {"attempt": "att_nope", "decision": "accept"}, "actor": actor},
        {"id": "obx_task", "created_at": "2026-10-03T10:00:01Z", "kind": "task.create",
         "body": {"title": "From the dashboard", "labels": ["ui"]}, "actor": actor},
        {"id": "obx_review", "created_at": "2026-10-03T10:00:02Z", "kind": "review",
         "body": {"attempt": attempt, "decision": "accept", "reason": "ship it", "merge": true}, "actor": actor},
    ]));
    let ingests = mock.state().ingests;
    let sync = w.gitbots(&w.repo, &["sync"], &[]);
    assert_eq!(sync["outbox"]["items"].as_array().unwrap().len(), 3, "{sync}");
    assert_eq!(mock.state().ingests, ingests + 1);

    // Every item is acked; the failure did not stop the others.
    let acks: HashMap<String, OutboxAck> = mock.state().acks.iter().cloned().collect();
    assert_eq!(acks.len(), 3);
    assert!(acks["obx_bad"].error.is_some() && acks["obx_bad"].event.is_none());
    let task_event = acks["obx_task"].event.clone().expect("task event");
    let review_event = acks["obx_review"].event.clone().expect("review event");
    assert!(acks["obx_task"].error.is_none() && acks["obx_review"].error.is_none());

    // The ledger has them, as the human via the UI.
    let events = w.gitbots(&w.repo, &["log", "-n", "500"], &[]);
    let by_id =
        |id: &str| events.as_array().unwrap().iter().find(|e| e["id"] == id).cloned().unwrap();
    let task = by_id(&task_event);
    assert_eq!(task["kind"], "task.created");
    assert_eq!(task["data"]["title"], "From the dashboard");
    assert_eq!((&task["via"], &task["actor"]["type"]), (&json!("ui"), &json!("human")));
    let review = by_id(&review_event);
    assert_eq!(review["kind"], "review.decided");
    assert_eq!(review["data"]["decision"], "accept");
    assert_eq!(review["via"], "ui");
    let merged = events.as_array().unwrap().iter().find(|e| e["kind"] == "attempt.merged").unwrap();
    assert_eq!(merged["on"], review_event.as_str());

    // ...and so does the hosted repo, with the merge pushed.
    assert_eq!(w.git(&main, &["show", "main:hello.txt"]), "hello");
    let remote = remote_events(&w, &main);
    assert!(remote.iter().any(|p| p.ends_with(&format!("{task_event}.json"))));
    assert!(remote.iter().any(|p| p.ends_with(&format!("{review_event}.json"))));

    // Applied once: nothing pending, a second sync changes nothing.
    w.gitbots(&w.repo, &["sync"], &[]);
    assert_eq!(mock.state().acks.len(), 3);
    let tasks = w.gitbots(&w.repo, &["task", "list"], &[]);
    assert_eq!(
        tasks.as_array().unwrap().iter().filter(|t| t["title"] == "From the dashboard").count(),
        1
    );

    // An agent's sync never applies the outbox.
    mock.queue(
        json!([{"id": "obx_late", "created_at": "2026-10-03T10:00:03Z", "kind": "task.create",
        "body": {"title": "Later"}, "actor": actor}]),
    );
    let agent_sync = w.gitbots(&w.repo, &["sync"], &[("GITBOTS_SESSION", &agent)]);
    assert!(
        agent_sync["outbox"].is_null() && agent_sync["outbox_skipped"].is_string(),
        "{agent_sync}"
    );
    assert_eq!(mock.state().outbox.len(), 1);
    w.assert_no_token_leaks(&mock);
}

#[test]
fn concurrent_agents_auto_sync_into_one_remote_ledger() {
    let (w, mock) = World::cloud();
    let agents = [w.session("anthropic"), w.session("openai")];
    let pushes_before = mock.state().git.len();
    let main_repo = mock.base();
    let rounds = 3;

    let barrier = Barrier::new(agents.len());
    std::thread::scope(|scope| {
        for agent in &agents {
            let (w, barrier) = (&w, &barrier);
            scope.spawn(move || {
                barrier.wait();
                for i in 0..rounds {
                    let env = [("GITBOTS_SESSION", agent.as_str())];
                    w.gitbots(&w.repo, &["task", "create", &format!("{agent} task {i}")], &env);
                    w.gitbots(&w.repo, &["report", &format!("{agent} report {i}")], &env);
                }
            });
        }
    });

    // Without any manual sync, every event of both agents is in the hosted ledger.
    let events = remote_events(&w, &mock.bare("main"));
    for agent in &agents {
        let mine = events.iter().filter(|p| p.contains(&format!("/{agent}/"))).count();
        assert_eq!(mine, 1 + 2 * rounds, "{agent}: {events:#?}");
    }
    // Each push was made with a token of the pushing agent's session, and
    // each session's token was minted once and then reused from the cache.
    let st = mock.state();
    let pushes: Vec<_> = st.git[pushes_before..].iter().filter(|h| h.push).collect();
    for agent in &agents {
        assert!(pushes.iter().any(|h| h.session.as_deref() == Some(agent.as_str())));
        let mints =
            st.token_requests.iter().filter(|r| r.session.as_deref() == Some(agent.as_str()));
        assert_eq!(mints.count(), 1, "{agent}");
    }
    assert!(pushes.iter().all(|h| h.session.is_some() && h.repo == main_repo), "{pushes:?}");
    assert_eq!(st.denied, 0);
    drop(st);
    w.assert_no_token_leaks(&mock);
}

#[test]
fn refused_tokens_warn_without_leaking_and_are_replaced() {
    let (w, mock) = World::cloud();
    let agent = w.session("anthropic");
    mock.revoke_all();

    // The cached token is refused: the command still succeeds and warns.
    let out = w.gitbots_raw(
        &w.repo,
        &["task", "create", "while revoked"],
        &[("GITBOTS_SESSION", &agent)],
    );
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("auto-sync") && stderr.contains("refused the token"), "{stderr}");
    assert!(mock.state().denied > 0);

    // The refused token was dropped: the next command mints a new one and lands.
    let minted = mock.token_requests_for(Some(&agent));
    w.gitbots(&w.repo, &["report", "back again"], &[("GITBOTS_SESSION", &agent)]);
    assert_eq!(mock.token_requests_for(Some(&agent)), minted + 1);
    let events = remote_events(&w, &mock.bare("main"));
    assert_eq!(events.iter().filter(|p| p.contains(&format!("/{agent}/"))).count(), 3);

    // The same for a full sync, which fails loudly instead.
    mock.revoke_all();
    let out = w.gitbots_raw(&w.repo, &["sync"], &[]);
    assert!(!out.status.success());
    w.gitbots(&w.repo, &["sync"], &[]);
    w.assert_no_token_leaks(&mock);
}

#[test]
fn cloud_token_and_fork_are_minted_for_the_session() {
    let (w, mock) = World::cloud();
    let agent = w.session("anthropic");

    let token = w.gitbots_with_token(&["cloud", "token"], &[("GITBOTS_SESSION", &agent)]);
    assert!(s(&token["token"]).starts_with("art_v1_"), "{token}");
    assert_eq!(token["remote"], w.git(&w.repo, &["remote", "get-url", "gitbots-main"]).as_str());
    assert!(time::OffsetDateTime::parse(&s(&token["expires_at"]), &Rfc3339).is_ok());
    let last = mock.state().token_requests.last().cloned().unwrap();
    assert_eq!(last.session.as_deref(), Some(agent.as_str()));
    assert_eq!(
        (last.repo, last.scope, last.ttl_secs),
        (RepoRef::Known(KnownRepo::Main), TokenScope::Write, None)
    );

    w.gitbots_with_token(
        &[
            "--session",
            &agent,
            "cloud",
            "token",
            "--repo",
            "logs",
            "--scope",
            "read",
            "--ttl",
            "600",
        ],
        &[],
    );
    let last = mock.state().token_requests.last().cloned().unwrap();
    assert_eq!(last.session.as_deref(), Some(agent.as_str()));
    assert_eq!(
        (last.repo, last.scope, last.ttl_secs),
        (RepoRef::Known(KnownRepo::Logs), TokenScope::Read, Some(600))
    );

    // The human's token carries no session.
    w.gitbots_with_token(&["cloud", "token"], &[]);
    assert_eq!(mock.state().token_requests.last().unwrap().session, None);

    // A fork for an attempt: remote added as a plain git remote, token for the session.
    let task = s(&w.gitbots(&w.repo, &["task", "create", "Hosted"], &[])["task"]);
    let attempt = s(&w.gitbots(
        &w.repo,
        &["attempt", "start", &task],
        &[("GITBOTS_SESSION", &agent)],
    )["attempt"]);
    let fork = w.gitbots_with_token(&["cloud", "fork", &attempt], &[("GITBOTS_SESSION", &agent)]);
    let req = mock.state().forks.last().cloned().unwrap();
    assert_eq!(
        (req.attempt.as_str(), req.session.as_deref()),
        (attempt.as_str(), Some(agent.as_str()))
    );
    assert!(s(&fork["token"]).starts_with("art_v1_"));
    assert_eq!(w.git(&w.repo, &["remote", "get-url", &s(&fork["git_remote"])]), s(&fork["remote"]));
    w.assert_no_token_leaks(&mock);
}

#[test]
fn watch_survives_transient_errors() {
    let (w, mock) = World::cloud();
    // No cached token and a Worker hiccup: the first round fails.
    std::fs::remove_file(w.repo.join(".git/gitbots/tokens/main-human.json")).unwrap();
    mock.state().fail_tokens = 1;
    mock.queue(
        json!([{"id": "obx_watch", "created_at": "2026-10-03T10:00:00Z", "kind": "task.create",
        "body": {"title": "Watched"}, "actor": {"type": "human", "handle": "gstohl"}}]),
    );

    let mut child = w
        .command(&w.repo, &["--json", "sync", "--watch", "--interval", "1"], &[])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let first = rx.recv_timeout(Duration::from_secs(60)).expect("a round after the failure");
    let round2 = rx.recv_timeout(Duration::from_secs(60)).expect("and the next round");
    child.kill().unwrap();
    child.wait().unwrap();
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    w.outputs.lock().unwrap().push(format!("{first}\n{round2}\n{stderr}"));

    assert!(stderr.contains("retrying in 1s"), "{stderr}");
    let first: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first["outbox"]["items"][0]["id"], "obx_watch", "{first}");
    assert_eq!(first["outbox"]["items"][0]["acked"], true);
    let round2: Value = serde_json::from_str(&round2).unwrap();
    assert!(round2["outbox"]["items"].as_array().unwrap().is_empty(), "{round2}");
    assert_eq!(mock.state().acks.len(), 1);
    w.assert_no_token_leaks(&mock);
}

/// Moves the hosted trusted branch from another clone (`OTHER.md`).
fn push_from_another_clone(w: &World, bare: &Path) {
    let other = w.root.join("other");
    w.git(&w.root, &["clone", "-q", bare.to_str().unwrap(), other.to_str().unwrap()]);
    w.git(&other, &["config", "user.name", "someone"]);
    w.git(&other, &["config", "user.email", "someone@example.com"]);
    std::fs::write(other.join("OTHER.md"), "other\n").unwrap();
    w.git(&other, &["add", "OTHER.md"]);
    w.git(&other, &["commit", "-q", "-m", "other clone"]);
    w.git(&other, &["push", "-q", "origin", "main"]);
}

fn review_item(id: &str, attempt: &str) -> Value {
    json!({"id": id, "created_at": "2026-10-03T10:00:00Z", "kind": "review",
        "body": {"attempt": attempt, "decision": "accept", "merge": true},
        "actor": {"type": "human", "handle": "gstohl"}})
}

fn task_item(id: &str, title: &str) -> Value {
    json!({"id": id, "created_at": "2026-10-03T10:00:00Z", "kind": "task.create",
        "body": {"title": title}, "actor": {"type": "human", "handle": "gstohl"}})
}

#[test]
fn steward_merges_onto_the_hosted_trusted_branch() {
    let (w, mock) = World::cloud();
    let agent = w.session("anthropic");
    let (attempt, _) = submitted_attempt(&w, &agent);
    let main = mock.bare("main");
    push_from_another_clone(&w, &main);

    mock.queue(json!([review_item("obx_merge", &attempt)]));
    let sync = w.gitbots(&w.repo, &["sync"], &[]);
    assert_eq!(sync["outbox"]["items"][0]["acked"], true, "{sync}");
    // Local main followed the hosted one first, so the merge sits on top of it.
    assert_eq!(w.git(&main, &["show", "main:hello.txt"]), "hello");
    assert_eq!(w.git(&main, &["show", "main:OTHER.md"]), "other");
    assert_eq!(w.git(&main, &["rev-parse", "main"]), w.git(&w.repo, &["rev-parse", "main"]));
    let acks = mock.state().acks.clone();
    assert!(acks[0].1.event.is_some() && acks[0].1.error.is_none(), "{acks:?}");
    w.assert_no_token_leaks(&mock);
}

#[test]
fn steward_leaves_what_a_retry_can_fix_pending() {
    let (w, mock) = World::cloud();
    let agent = w.session("anthropic");
    let (attempt, _) = submitted_attempt(&w, &agent);
    let main = mock.bare("main");
    // Hosted and local main diverge.
    push_from_another_clone(&w, &main);
    std::fs::write(w.repo.join("LOCAL.md"), "local\n").unwrap();
    w.git(&w.repo, &["add", "LOCAL.md"]);
    w.git(&w.repo, &["commit", "-q", "-m", "local"]);

    mock.queue(json!([
        {"id": "obx_new", "created_at": "2026-10-03T10:00:00Z", "kind": "task.delete",
         "body": {"task": "tsk_x"}, "actor": {"type": "human", "handle": "gstohl"}},
        review_item("obx_gone", "att_nope"),
        review_item("obx_merge", &attempt),
        task_item("obx_task", "Still created"),
    ]));
    let out = w.gitbots_raw(&w.repo, &["--json", "sync"], &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let sync: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(sync["warnings"].to_string().contains("diverged"), "{sync}");
    let items: HashMap<String, Value> = sync["outbox"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (s(&i["id"]), i.clone()))
        .collect();
    // An unknown kind and a merge onto a diverged main wait; the rest is final.
    for pending in ["obx_new", "obx_merge"] {
        assert_eq!(
            (&items[pending]["retry"], &items[pending]["acked"]),
            (&json!(true), &json!(false))
        );
    }
    assert!(items["obx_gone"]["error"].is_string() && items["obx_gone"]["acked"] == true);
    assert!(items["obx_task"]["event"].is_string() && items["obx_task"]["acked"] == true);
    let acked: Vec<String> = mock.state().acks.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(acked, ["obx_gone", "obx_task"]);
    let state = w.gitbots(&w.repo, &["attempt", "show", &attempt], &[]);
    assert_eq!(state["attempt"]["state"], "submitted");

    // Once main is reconciled, the next sync merges; the unknown kind still waits.
    w.git(&w.repo, &["merge", "-q", "--no-edit", "refs/remotes/gitbots-main/main"]);
    let sync = w.gitbots(&w.repo, &["sync"], &[]);
    assert_eq!(sync["outbox"]["items"].as_array().unwrap().len(), 2, "{sync}");
    for file in ["hello.txt", "OTHER.md", "LOCAL.md"] {
        w.git(&main, &["show", &format!("main:{file}")]);
    }
    let pending: Vec<Value> = mock.state().outbox.clone();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["id"], "obx_new");
    w.assert_no_token_leaks(&mock);
}

#[test]
fn concurrent_stewards_apply_each_item_once() {
    let (w, mock) = World::cloud();
    let agent = w.session("anthropic");
    let (attempt, _) = submitted_attempt(&w, &agent);
    mock.queue(json!([
        task_item("obx_t1", "Raced one"),
        task_item("obx_t2", "Raced two"),
        review_item("obx_merge", &attempt),
        task_item("obx_t3", "Raced three"),
    ]));
    // Both stewards check the project (after their snapshot of what is
    // applied) before either applies anything: they race on every item.
    mock.state().project_gate = Some(Arc::new(tokio::sync::Barrier::new(2)));
    std::thread::scope(|scope| {
        let runs: Vec<_> = (0..2)
            .map(|_| scope.spawn(|| w.gitbots_raw(&w.repo, &["--json", "sync"], &[])))
            .collect();
        for run in runs {
            let out = run.join().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            // Each steward acks every item as a success, whoever applied it.
            let sync: Value = serde_json::from_slice(&out.stdout).unwrap();
            for item in sync["outbox"]["items"].as_array().unwrap() {
                assert!(item["error"].is_null() && item["acked"] == true, "{item}");
            }
        }
    });

    let events = w.gitbots(&w.repo, &["log", "-n", "500"], &[]);
    let count =
        |kind: &str| events.as_array().unwrap().iter().filter(|e| e["kind"] == kind).count();
    assert_eq!(count("review.decided"), 1);
    assert_eq!(count("attempt.merged"), 1);
    let tasks = w.gitbots(&w.repo, &["task", "list"], &[]);
    for title in ["Raced one", "Raced two", "Raced three"] {
        assert_eq!(tasks.as_array().unwrap().iter().filter(|t| t["title"] == title).count(), 1);
    }
    // Every ack is a success, and both stewards name the same event.
    let mut by_item: HashMap<String, Vec<String>> = HashMap::new();
    for (id, ack) in mock.state().acks.clone() {
        assert!(ack.error.is_none(), "{id}: {ack:?}");
        by_item.entry(id).or_default().push(ack.event.expect("event"));
    }
    assert_eq!(by_item.len(), 4);
    for (id, events) in &by_item {
        assert!(events.windows(2).all(|p| p[0] == p[1]), "{id}: {events:?}");
    }
    assert_eq!(w.git(&mock.bare("main"), &["show", "main:hello.txt"]), "hello");
}

#[test]
fn an_agent_harness_without_a_session_acts_as_one_session_of_its_own() {
    let (w, mock) = World::cloud();
    let human_mints = mock.token_requests_for(None);
    let seen = mock.state().git.len();
    let harness = [("CLAUDECODE", "1")];

    // Two commands in a row in the main worktree: one session, started once.
    let out = w.gitbots_raw(&w.repo, &["--json", "task", "create", "from a harness"], &harness);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success() && stderr.contains("started ses_"), "{stderr}");
    let task = s(&serde_json::from_slice::<Value>(&out.stdout).unwrap()["task"]);
    let out = w.gitbots_raw(&w.repo, &["--json", "report", "still me"], &harness);
    assert!(out.status.success() && !String::from_utf8_lossy(&out.stderr).contains("started"));
    let who = w.gitbots(&w.repo, &["whoami"], &harness);
    assert_eq!(who["via"], "env");
    let auto = s(&who["actor"]["session"]);
    let sessions = w.gitbots(&w.repo, &["session", "list"], &[]);
    let labels: Vec<&Value> =
        sessions.as_array().unwrap().iter().map(|v| &v["session"]["label"]).collect();
    assert_eq!(labels, [&json!("auto: CLAUDECODE")], "{sessions}");

    // A commit there carries the same session in its trailers.
    std::fs::write(w.repo.join("harness.txt"), "x\n").unwrap();
    w.git(&w.repo, &["add", "harness.txt"]);
    w.git_env(&w.repo, &["commit", "-q", "-m", "harness commit"], &harness);
    let message = w.git(&w.repo, &["log", "-1", "--format=%B"]);
    assert!(message.contains(&format!("Gitbots-Session: {auto}")), "{message}");

    // Events, minted tokens and pushes all name it; the human's token is never used.
    let events = w.gitbots(&w.repo, &["log", "-n", "50"], &[]);
    let events = events.as_array().unwrap();
    let created = events.iter().find(|e| e["data"]["task"] == task.as_str()).unwrap();
    let report = events.iter().find(|e| e["data"]["title"] == "still me").unwrap();
    let commit = events.iter().find(|e| e["kind"] == "commit.recorded").unwrap();
    for e in [created, report, commit] {
        assert_eq!(e["actor"]["session"], auto.as_str(), "{e}");
    }
    assert_eq!((&created["via"], &report["via"]), (&json!("env"), &json!("env")));
    let st = mock.state();
    let minted: Vec<_> = st.token_requests.iter().filter_map(|r| r.session.clone()).collect();
    assert_eq!(minted, std::slice::from_ref(&auto));
    let pushes: Vec<&GitHit> = st.git[seen..].iter().filter(|h| h.push).collect();
    assert!(
        !pushes.is_empty() && pushes.iter().all(|h| h.session.as_deref() == Some(auto.as_str()))
    );
    drop(st);
    assert_eq!(mock.token_requests_for(None), human_mints);
    let remote = remote_events(&w, &mock.bare("main"));
    assert_eq!(remote.iter().filter(|p| p.contains(&format!("/{auto}/"))).count(), 3);

    // The human in the same worktree is still the human.
    assert_eq!(w.gitbots(&w.repo, &["whoami"], &[])["actor"]["type"], "human");
    // A workroom's binding wins over the auto-started session.
    let bound = w.session("openai");
    let room = w.gitbots(&w.repo, &["attempt", "start", &task], &[("GITBOTS_SESSION", &bound)]);
    let room = PathBuf::from(s(&room["workroom"]));
    let who = w.gitbots(&room, &["whoami"], &harness);
    assert_eq!((&who["via"], &who["actor"]["session"]), (&json!("worktree"), &json!(bound)));

    // What acts as the human refuses: the outbox stays pending, init is refused.
    mock.queue(json!([task_item("obx_human", "Only for the human")]));
    let sync = w.gitbots(&w.repo, &["sync"], &harness);
    assert!(s(&sync["outbox_skipped"]).contains("CLAUDECODE"), "{sync}");
    assert_eq!(mock.state().outbox.len(), 1);
    let err = w.gitbots_raw(&w.repo, &["cloud", "init", "--url", &mock.url], &harness);
    let stderr = String::from_utf8_lossy(&err.stderr);
    assert!(!err.status.success() && stderr.contains("acts as the human"), "{stderr}");
    assert_eq!(mock.token_requests_for(None), human_mints);

    // Another agent, or the same one after its session ended, gets a new session.
    let other = w.gitbots(&w.repo, &["whoami"], &[("CLAUDECODE", "1"), ("GITBOTS_MODEL", "other")]);
    assert_ne!(other["actor"]["session"], auto.as_str());
    let again = s(&w.gitbots(&w.repo, &["whoami"], &harness)["actor"]["session"]);
    assert_ne!(again, auto);
    w.gitbots(&w.repo, &["session", "end"], &harness);
    assert_ne!(w.gitbots(&w.repo, &["whoami"], &harness)["actor"]["session"], again.as_str());
    w.assert_no_token_leaks(&mock);
}

#[test]
fn an_agent_sync_pushes_only_its_family_attempts() {
    let (w, mock) = World::cloud();
    w.git(&w.repo, &["config", "gitbots.cloud.autoSync", "false"]);
    let a = w.session("anthropic");
    let b = w.session("openai");
    let sub = s(&w.gitbots(
        &w.repo,
        &["session", "start", "--provider", "anthropic", "--model", "small", "--client", "test"],
        &[("GITBOTS_SESSION", &a)],
    )["id"]);
    let work = |session: &str, title: &str| {
        let task = s(&w.gitbots(&w.repo, &["task", "create", title], &[])["task"]);
        let info =
            w.gitbots(&w.repo, &["attempt", "start", &task], &[("GITBOTS_SESSION", session)]);
        let room = PathBuf::from(s(&info["workroom"]));
        std::fs::write(room.join(format!("{title}.txt")), "x\n").unwrap();
        w.git(&room, &["add", "."]);
        w.git(&room, &["commit", "-q", "-m", title]);
        (s(&info["attempt"]), s(&info["branch"]))
    };
    let (_, by_sub) = work(&sub, "sub");
    let (y, by_b) = work(&b, "bee");
    std::fs::write(w.repo.join("MAIN.md"), "human\n").unwrap();
    w.git(&w.repo, &["add", "MAIN.md"]);
    w.git(&w.repo, &["commit", "-q", "-m", "human work on main"]);
    let main = mock.bare("main");
    let hosted = |branch: &str| !w.git(&main, &["branch", "--list", branch]).is_empty();
    let hosted_main = || w.git(&main, &["rev-parse", "main"]);
    let before = hosted_main();

    // a's sync pushes its subagent's attempt, not b's, and not the human's main.
    w.gitbots(&w.repo, &["sync"], &[("GITBOTS_SESSION", &a)]);
    assert!(hosted(&by_sub) && !hosted(&by_b));
    assert_eq!(hosted_main(), before);
    let pushers: Vec<_> =
        mock.state().git.iter().filter(|h| h.push).map(|h| h.session.clone()).collect();
    assert_eq!(pushers.last().unwrap().as_deref(), Some(a.as_str()));

    // A handoff makes b's attempt a's to push.
    w.gitbots(&w.repo, &["handoff", &y, "--to-session", &a], &[("GITBOTS_SESSION", &b)]);
    w.gitbots(&w.repo, &["sync"], &[("GITBOTS_SESSION", &a)]);
    assert!(hosted(&by_b));

    // The human's sync pushes everything.
    let (_, by_b2) = work(&b, "bee two");
    w.gitbots(&w.repo, &["sync"], &[]);
    assert!(hosted(&by_b2));
    assert_eq!(hosted_main(), w.git(&w.repo, &["rev-parse", "main"]));
    w.assert_no_token_leaks(&mock);
}

#[test]
fn cloud_init_publishes_on_a_rerun_after_a_failed_first_publish() {
    let w = World::new();
    let mock = Mock::start(&w.root.join("artifacts"), &w.gitconfig);
    w.gitbots(&w.repo, &["init", "--commit"], &[]);
    // Provisioned, then the Worker hiccups before anything is pushed.
    mock.state().fail_tokens = 1;
    let out = w.gitbots_raw(
        &w.repo,
        &["cloud", "init", "--url", &mock.url],
        &[("GITBOTS_ADMIN_KEY", ADMIN)],
    );
    assert!(!out.status.success());
    let main = mock.bare("main");
    assert!(w.git(&main, &["branch", "--list", "gitbots/activity"]).is_empty());

    let again = w.gitbots(&w.repo, &["cloud", "init", "--url", &mock.url], &[]);
    assert_eq!(
        (&again["created"], &again["verified_only"]),
        (&json!(false), &json!(false)),
        "{again}"
    );
    assert_eq!(
        w.git(&main, &["rev-parse", "gitbots/activity"]),
        w.git(&w.repo, &["rev-parse", "gitbots/activity"])
    );
    // Once published, a rerun only verifies.
    let third = w.gitbots(&w.repo, &["cloud", "init", "--url", &mock.url], &[]);
    assert_eq!(third["verified_only"], true);
    w.assert_no_token_leaks(&mock);
}

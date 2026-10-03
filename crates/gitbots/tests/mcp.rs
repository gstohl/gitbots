//! End-to-end: drive `gitbots mcp` over stdio the way an MCP client does.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(30);

struct World {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    gitconfig: PathBuf,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("gitbots-home");
        let gitconfig = tmp.path().join("gitconfig");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(&gitconfig, "").unwrap();
        let w = World { _tmp: tmp, repo, home, gitconfig };
        w.git(&w.repo, &["init", "-q", "-b", "main"], &[]);
        w.git(&w.repo, &["config", "user.name", "gstohl"], &[]);
        w.git(&w.repo, &["config", "user.email", "dominik@example.com"], &[]);
        std::fs::write(w.repo.join("README.md"), "# demo\n").unwrap();
        w.git(&w.repo, &["add", "."], &[]);
        w.git(&w.repo, &["commit", "-q", "-m", "initial"], &[]);

        let out = w.cmd(env!("CARGO_BIN_EXE_gitbots"), &w.repo).args(["init", "--commit"]).output();
        let out = out.unwrap();
        assert!(out.status.success(), "gitbots init: {}", String::from_utf8_lossy(&out.stderr));
        w
    }

    /// A command isolated from the developer's agent harness and git config.
    fn cmd(&self, program: &str, dir: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(dir);
        for (k, _) in std::env::vars_os() {
            let k = k.to_string_lossy();
            if k == "CLAUDECODE"
                || k == "CURSOR_AGENT"
                || k.starts_with("CLAUDE_CODE")
                || k.starts_with("CODEX_")
                || k.starts_with("GITBOTS_")
            {
                cmd.env_remove(&*k);
            }
        }
        cmd.env("GITBOTS_HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        cmd
    }

    fn git(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let out = self.cmd("git", dir).args(args).envs(env.iter().copied()).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn mcp(&self, client: &str, version: &str, deadline: Instant) -> Client {
        let mut child = self
            .cmd(env!("CARGO_BIN_EXE_gitbots"), &self.repo)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let log = Arc::new(Mutex::new(String::new()));
        let sink = log.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let mut c = Client { child, stdin, lines, stderr: log, deadline, next_id: 1 };
        let init = c.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": client, "version": version },
            }),
        );
        assert_eq!(init["serverInfo"]["name"], "gitbots", "{init}");
        let instructions = init["instructions"].as_str().unwrap_or_default();
        assert!(instructions.contains("session_start"), "{init}");
        c.notify("notifications/initialized");
        c
    }
}

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    deadline: Instant,
    next_id: u64,
}

impl Client {
    fn fail(&self, msg: String) -> ! {
        panic!("{msg}\n--- gitbots mcp stderr ---\n{}", self.stderr.lock().unwrap());
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method }));
    }

    /// Send a request and wait for the response with its id.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(line) => line,
                Err(e) => self.fail(format!("no response to {method} (id {id}): {e}")),
            };
            let msg: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => self.fail(format!("stdout must carry only JSON-RPC ({e}): {line}")),
            };
            if msg["id"] != json!(id) {
                continue;
            }
            if msg.get("error").is_some() {
                self.fail(format!("{method} failed: {msg}"));
            }
            return msg["result"].clone();
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        self.request("tools/call", json!({ "name": tool, "arguments": args }))
    }

    /// A successful tool call's JSON payload.
    fn ok(&mut self, tool: &str, args: Value) -> (Value, String) {
        let result = self.call(tool, args);
        if result["isError"] == json!(true) {
            self.fail(format!("{tool} returned a tool error: {result}"));
        }
        let text = all_text(&result);
        let payload = match result.get("structuredContent") {
            Some(v) => v.clone(),
            None => {
                let first = result["content"][0]["text"].as_str().unwrap_or_default();
                serde_json::from_str(first)
                    .unwrap_or_else(|e| self.fail(format!("{tool}: not JSON ({e}): {result}")))
            }
        };
        (payload, text)
    }

    /// Close stdin; the server must exit on its own.
    fn close(mut self) {
        drop(self.stdin.take());
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "gitbots mcp exited with {status}");
                return;
            }
            if Instant::now() >= self.deadline {
                self.fail("gitbots mcp did not exit after stdin closed".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn all_text(result: &Value) -> String {
    let blocks = result["content"].as_array().cloned().unwrap_or_default();
    blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_else(|| panic!("not a string: {v}")).to_owned()
}

#[test]
fn mcp_session_lifecycle() {
    let deadline = Instant::now() + TIMEOUT;
    let w = World::new();
    let mut c = w.mcp("codex", "0.42.0", deadline);

    let tools = c.request("tools/list", json!({}));
    let names: Vec<String> =
        tools["tools"].as_array().unwrap().iter().map(|t| s(&t["name"])).collect();
    for expected in [
        "whoami",
        "session_start",
        "session_end",
        "status",
        "task_create",
        "task_show",
        "attempt_start",
        "attempt_submit",
        "attempt_show",
        "handoff",
        "review",
        "report",
        "trace",
        "activity",
        "inbox",
        "stats",
        "actions_list",
        "actions_run",
    ] {
        assert!(names.iter().any(|n| n == expected), "missing tool {expected}: {names:?}");
    }

    // Read-only tools work before any session exists.
    let (who, _) = c.ok("whoami", json!({}));
    assert!(who["session"].is_null(), "{who}");
    assert_eq!(who["trusted_branch"], "main");

    let (session, _) =
        c.ok("session_start", json!({ "provider": "openai", "model": "gpt-5-codex" }));
    let session_id = s(&session["id"]);
    assert_eq!(session["agent"]["client"], "codex", "{session}");
    assert_eq!(session["agent"]["client_version"], "0.42.0", "{session}");

    let (task, _) = c.ok("task_create", json!({ "title": "Add hello file" }));
    let task_id = s(&task["task"]);
    assert!(task.get("notice").is_none(), "no auto-start after session_start: {task}");

    let (info, guide) = c.ok("attempt_start", json!({ "task": task_id }));
    let attempt_id = s(&info["attempt"]);
    assert_eq!(info["session"], session_id.as_str());
    assert!(guide.contains("attempt_submit"), "{guide}");
    let workroom = PathBuf::from(s(&info["workroom"]));
    assert!(workroom.starts_with(&w.home), "workroom outside GITBOTS_HOME: {}", workroom.display());

    // The agent works in the workroom with plain git; hooks attribute it.
    std::fs::write(workroom.join("hello.txt"), "hello\n").unwrap();
    let env = [("GITBOTS_SESSION", session_id.as_str())];
    w.git(&workroom, &["add", "hello.txt"], &env);
    w.git(&workroom, &["commit", "-q", "-m", "add hello"], &env);
    let msg = w.git(&workroom, &["log", "-1", "--format=%B"], &[]);
    assert!(msg.contains(&format!("Gitbots-Session: {session_id}")), "{msg}");

    // Over MCP the attempt must be named.
    let missing = c.call("attempt_submit", json!({}));
    assert_eq!(missing["isError"], true, "{missing}");

    let (submitted, _) =
        c.ok("attempt_submit", json!({ "attempt": attempt_id, "summary": "adds hello.txt" }));
    assert_eq!(submitted["attempt"], attempt_id.as_str());

    // Same session family: a tool-level error the agent can read.
    let review = c.call("review", json!({ "attempt": attempt_id, "decision": "accept" }));
    assert_eq!(review["isError"], true, "{review}");
    assert!(all_text(&review).contains("own session"), "{review}");

    let (events, _) = c.ok("activity", json!({}));
    let kinds: Vec<String> = events.as_array().unwrap().iter().map(|e| s(&e["kind"])).collect();
    for kind in [
        "session.started",
        "task.created",
        "attempt.started",
        "commit.recorded",
        "attempt.submitted",
    ] {
        assert!(kinds.iter().any(|k| k == kind), "missing {kind}: {kinds:?}");
    }
    let agent_events = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["actor"]["session"] == session_id.as_str() && e["via"] == "mcp")
        .count();
    assert!(agent_events >= 3, "MCP writes are attributed to the session: {events}");

    c.close();

    // A second client that never calls session_start gets an auto-started
    // session seeded from its clientInfo, and is told to start one itself.
    let mut c = w.mcp("Claude Code", "2.1.0", deadline);
    let (reported, _) = c.ok("report", json!({ "title": "hello.txt is ready", "level": "info" }));
    let notice = s(&reported["notice"]);
    assert!(notice.contains("session_start"), "{notice}");
    let (who, _) = c.ok("whoami", json!({}));
    assert_eq!(who["session"]["agent"], "anthropic/unknown@claude-code", "{who}");
    let (again, _) = c.ok("trace", json!({ "tool": "bash", "ok": true }));
    assert!(again.get("notice").is_none(), "notice only once: {again}");
    c.close();
}

//! End-to-end: one human, two agents, one task, through the real binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

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
            if k == "CLAUDECODE"
                || k.starts_with("CLAUDE_CODE")
                || k.starts_with("CODEX_")
                || k.starts_with("GITBOTS_")
            {
                cmd.env_remove(k);
            }
        }
        cmd.env("GITBOTS_HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        self.git_env(dir, args, &[])
    }

    fn git_env(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let mut cmd = Command::new("git");
        cmd.current_dir(dir).args(args);
        self.env(&mut cmd);
        cmd.envs(env.iter().copied());
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn gitbots_raw(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_gitbots"));
        cmd.current_dir(dir).args(args);
        self.env(&mut cmd);
        cmd.envs(env.iter().copied());
        cmd.output().unwrap()
    }

    fn gitbots(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = self.gitbots_raw(dir, &all, env);
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

    fn gitbots_err(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let out = self.gitbots_raw(dir, args, env);
        assert!(!out.status.success(), "gitbots {args:?} unexpectedly succeeded");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_else(|| panic!("not a string: {v}")).to_owned()
}

#[test]
fn full_lifecycle() {
    let w = World::new();
    let repo = w.repo.clone();

    // The agentic `git init`.
    let init = w.gitbots(&repo, &["init", "--commit", "--goal", "ship the demo"], &[]);
    assert_eq!(init["created_manifest"], true);
    assert_eq!(init["trusted_branch"], "main");
    assert!(w.git(&repo, &["show", "gitbots/activity:LEDGER.json"]).contains("\"activity\""));
    assert!(w.git(&repo, &["show", "main:.gitbots/manifest.json"]).contains("ship the demo"));

    // A workflow on the trusted branch, plus a non-protected integration branch.
    std::fs::write(
        repo.join(".gitbots/actions/ci.toml"),
        "name = \"ci\"\non = [\"attempt.submitted\", \"manual\"]\n\n[jobs.check]\nsteps = [{ name = \"hello exists\", run = \"test -f hello.txt && echo ok\" }]\n",
    )
    .unwrap();
    w.git(&repo, &["add", "."]);
    w.git(&repo, &["commit", "-q", "-m", "add ci workflow"]);
    w.git(&repo, &["branch", "feature"]);
    let wf = w.gitbots(&repo, &["actions", "list"], &[]);
    assert_eq!(wf["workflows"][0][1]["name"], "ci", "{wf}");

    // Two agents.
    let worker = w.gitbots(
        &repo,
        &[
            "session",
            "start",
            "--provider",
            "anthropic",
            "--model",
            "claude-opus-5-5",
            "--client",
            "claude-code",
            "--role",
            "implementer",
        ],
        &[],
    );
    let worker_id = s(&worker["id"]);
    let reviewer = w.gitbots(
        &repo,
        &[
            "session",
            "start",
            "--provider",
            "openai",
            "--model",
            "gpt-5-codex",
            "--client",
            "codex",
            "--role",
            "reviewer",
        ],
        &[],
    );
    let reviewer_id = s(&reviewer["id"]);
    // A subagent of the worker inherits GITBOTS_SESSION as its parent.
    let sub = w.gitbots(
        &repo,
        &[
            "session",
            "start",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "--client",
            "claude-code",
        ],
        &[("GITBOTS_SESSION", &worker_id)],
    );
    assert_eq!(sub["parent"], worker_id.as_str());

    // The human files a task; the worker starts an attempt against `feature`.
    let task = w.gitbots(&repo, &["task", "create", "Add hello file"], &[]);
    let task_id = s(&task["task"]);
    let info = w.gitbots(
        &repo,
        &["attempt", "start", &task_id, "--base", "feature"],
        &[("GITBOTS_SESSION", &worker_id)],
    );
    let attempt_id = s(&info["attempt"]);
    let workroom = PathBuf::from(s(&info["workroom"]));
    assert!(
        workroom.starts_with(&w.home),
        "workrooms live outside the repo: {}",
        workroom.display()
    );

    // Inside the workroom the binding attributes plain `git commit`.
    let who = w.gitbots(&workroom, &["whoami"], &[]);
    assert_eq!(who["via"], "worktree");
    assert_eq!(who["actor"]["session"], worker_id.as_str());
    std::fs::write(workroom.join("hello.txt"), "hello\n").unwrap();
    w.git(&workroom, &["add", "hello.txt"]);
    w.git(
        &workroom,
        &["commit", "-q", "-m", "Add hello\n\nCo-Authored-By: Claude <noreply@anthropic.com>"],
    );
    let msg = w.git(&workroom, &["log", "-1", "--format=%B"]);
    assert!(msg.contains(&format!("Gitbots-Session: {worker_id}")), "{msg}");
    assert!(msg.contains("Gitbots-Model: claude-opus-5-5"), "{msg}");
    let trailers =
        w.git(&workroom, &["log", "-1", "--format=%(trailers:key=Co-Authored-By,valueonly)"]);
    assert!(trailers.contains("Claude"), "Co-Authored-By must stay in the trailer block: {msg}");

    // Submit runs the trusted workflow in the workroom.
    let submitted =
        w.gitbots(&workroom, &["attempt", "submit", "--summary", "adds hello.txt"], &[]);
    assert_eq!(submitted["runs"][0]["status"], "success", "{submitted}");
    assert_eq!(submitted["diff"]["insertions"], 1);
    let log_ref = &submitted["runs"][0]["jobs"][0]["log"];
    let log =
        w.git(&repo, &["show", &format!("{}:{}", s(&log_ref["branch"]), s(&log_ref["path"]))]);
    assert!(log.contains("ok"), "{log}");

    // No self-review, not even by a subagent.
    let err = w.gitbots_err(
        &repo,
        &["review", &attempt_id, "accept"],
        &[("GITBOTS_SESSION", &s(&sub["id"]))],
    );
    assert!(err.contains("own session tree"), "{err}");

    // An agent may not touch the mandate: a second attempt editing .gitbots/ is refused.
    let bad = w.gitbots(
        &repo,
        &["attempt", "start", &task_id, "--base", "feature"],
        &[("GITBOTS_SESSION", &worker_id)],
    );
    let bad_room = PathBuf::from(s(&bad["workroom"]));
    std::fs::write(bad_room.join(".gitbots/manifest.json"), "{}").unwrap();
    w.git(&bad_room, &["commit", "-q", "-am", "grant myself everything"]);
    let err = w.gitbots_err(&bad_room, &["attempt", "submit"], &[]);
    assert!(err.contains("mandate violation") && err.contains(".gitbots/manifest.json"), "{err}");

    // The independent reviewer agent accepts and merges into `feature` (not protected).
    let review = w.gitbots(
        &repo,
        &["review", &attempt_id, "accept", "--merge", "--reason", "looks right"],
        &[("GITBOTS_SESSION", &reviewer_id)],
    );
    assert!(review["merged"].is_string(), "{review}");
    assert_eq!(w.git(&repo, &["show", "feature:hello.txt"]), "hello");

    // Agents report; humans read the inbox.
    w.gitbots(
        &repo,
        &["report", "Done with hello", "--level", "info", "--attempt", &attempt_id],
        &[("GITBOTS_SESSION", &worker_id)],
    );
    let inbox = w.gitbots(&repo, &["inbox"], &[]);
    assert_eq!(inbox["reports"][0]["report"]["title"], "Done with hello");

    // Observability: the ledger tells the whole story, per agent.
    let kinds: Vec<String> = w
        .gitbots(&repo, &["log", "-n", "100"], &[])
        .as_array()
        .unwrap()
        .iter()
        .map(|e| s(&e["kind"]))
        .collect();
    for k in [
        "project.initialized",
        "session.started",
        "task.created",
        "attempt.started",
        "commit.recorded",
        "attempt.submitted",
        "action.completed",
        "review.decided",
        "attempt.merged",
        "report",
    ] {
        assert!(kinds.iter().any(|x| x == k), "missing {k} in {kinds:?}");
    }
    let stats = w.gitbots(&repo, &["stats"], &[]);
    let opus = &stats["by_actor"]["anthropic/claude-opus-5-5@claude-code"];
    assert_eq!(opus["accepted"], 1, "{stats}");
    assert_eq!(opus["merged"], 1);
    assert_eq!(opus["commits"], 2);
    assert_eq!(opus["runs_passed"], 1);
    assert_eq!(stats["by_actor"]["anthropic/claude-haiku-4-5@claude-code"]["subagent_sessions"], 1);

    let fsck =
        Command::new("git").current_dir(&repo).args(["fsck", "--no-dangling"]).output().unwrap();
    assert!(fsck.status.success(), "{}", String::from_utf8_lossy(&fsck.stderr));
}

#[test]
fn init_is_idempotent_and_hooks_fail_open() {
    let w = World::new();
    w.gitbots(&w.repo, &["init", "--commit"], &[]);
    let again = w.gitbots(&w.repo, &["init"], &[]);
    assert_eq!(again["created_manifest"], false);
    assert!(again["created_ledgers"].as_array().unwrap().is_empty());

    // A broken GITBOTS_BIN must never block a commit.
    std::fs::write(w.repo.join("x.txt"), "x").unwrap();
    w.git(&w.repo, &["add", "x.txt"]);
    w.git_env(&w.repo, &["commit", "-q", "-m", "x"], &[("GITBOTS_BIN", "/nonexistent/gitbots")]);
}

#[test]
fn protected_merge_needs_an_interactive_human() {
    let w = World::new();
    w.gitbots(&w.repo, &["init", "--commit"], &[]);
    let worker = w.gitbots(
        &w.repo,
        &["session", "start", "--provider", "anthropic", "--model", "m", "--client", "claude-code"],
        &[],
    );
    let worker_id = s(&worker["id"]);
    let task = s(&w.gitbots(&w.repo, &["task", "create", "Touch main"], &[])["task"]);
    let info = w.gitbots(&w.repo, &["attempt", "start", &task], &[("GITBOTS_SESSION", &worker_id)]);
    let attempt = s(&info["attempt"]);
    let room = PathBuf::from(s(&info["workroom"]));
    std::fs::write(room.join("a.txt"), "a").unwrap();
    w.git(&room, &["add", "a.txt"]);
    w.git(&room, &["commit", "-q", "-m", "a"]);
    w.gitbots(&room, &["attempt", "submit"], &[]);

    // Merging into `main` (protected) needs a maintainer: the git-config
    // fallback is refused without a terminal...
    let err = w.gitbots_err(&w.repo, &["review", &attempt, "accept", "--merge"], &[]);
    assert!(err.contains("interactive terminal"), "{err}");
    // ...and refused outright when an agent harness is detected.
    let err =
        w.gitbots_err(&w.repo, &["review", &attempt, "accept", "--merge"], &[("CLAUDECODE", "1")]);
    assert!(err.contains("CLAUDECODE"), "{err}");
    // An agent gets told a human must decide.
    let other = w.gitbots(
        &w.repo,
        &["session", "start", "--provider", "openai", "--model", "m", "--client", "codex"],
        &[],
    );
    let err = w.gitbots_err(
        &w.repo,
        &["review", &attempt, "accept", "--merge"],
        &[("GITBOTS_SESSION", &s(&other["id"]))],
    );
    assert!(err.contains("needs a human"), "{err}");
    assert!(w.git(&w.repo, &["ls-tree", "main", "--name-only"]).lines().all(|l| l != "a.txt"));
}

//! Tests against real temporary repositories.
//!
//! Every git process (ours and the fixtures') runs with an empty `HOME`,
//! `GIT_CONFIG_NOSYSTEM=1` and no identity variables, so the suite passes on
//! a machine with no git identity and is immune to the user's config.

mod ledger;
mod repo;
mod sync;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use gitbots_core::event::{Event, EventBody, Report};
use gitbots_core::id::{EventId, ProjectId, SessionId, Ulid};
use gitbots_core::identity::{Actor, AgentDescriptor};
use gitbots_core::ledger::{LedgerKind, LedgerMeta};
use time::OffsetDateTime;

use crate::Repo;

/// 2026-10-03T10:31:00.000Z
pub(crate) const TS: u64 = 1_791_023_460_000;

static EMPTY_HOME: LazyLock<tempfile::TempDir> =
    LazyLock::new(|| tempfile::tempdir().expect("tempdir"));

/// Called by `cli::command` in test builds.
pub(crate) fn isolate(cmd: &mut Command) {
    let home = EMPTY_HOME.path();
    cmd.env("HOME", home).env("XDG_CONFIG_HOME", home).env("GIT_CONFIG_NOSYSTEM", "1");
    for var in [
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
        "EMAIL",
        "GITBOTS_BIN",
    ] {
        cmd.env_remove(var);
    }
}

/// Runs a fixture git command (with a test identity) and returns trimmed stdout.
pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
    git_env(dir, args, &[])
}

pub(crate) fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    isolate(&mut cmd);
    cmd.env("GIT_AUTHOR_NAME", "Tester")
        .env("GIT_AUTHOR_EMAIL", "tester@example.com")
        .env("GIT_COMMITTER_NAME", "Tester")
        .env("GIT_COMMITTER_EMAIL", "tester@example.com");
    cmd.envs(env.iter().copied());
    let out = cmd.output().expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8").trim_end().to_owned()
}

/// `git fsck` finds nothing but the unborn-HEAD notice.
pub(crate) fn fsck_clean(dir: &Path) {
    let mut cmd = Command::new("git");
    cmd.args(["fsck", "--full", "--strict", "--no-dangling"]).current_dir(dir);
    isolate(&mut cmd);
    let out = cmd.output().expect("run git fsck");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let noise: Vec<_> = stderr.lines().filter(|l| !l.starts_with("notice:")).collect();
    assert!(out.status.success() && out.stdout.is_empty() && noise.is_empty(), "fsck: {out:?}");
}

/// A repository on `main` with one commit (`README.md`).
pub(crate) struct Fixture {
    pub tmp: tempfile::TempDir,
    pub dir: PathBuf,
    pub repo: Repo,
}

pub(crate) fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().canonicalize().expect("canonical").join("repo");
    std::fs::create_dir(&dir).expect("mkdir");
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "hello\n").expect("write");
    git(&dir, &["add", "README.md"]);
    git(&dir, &["commit", "-q", "-m", "initial"]);
    let repo = Repo::discover(&dir).expect("discover");
    Fixture { tmp, dir, repo }
}

pub(crate) fn project(n: u128) -> ProjectId {
    ProjectId::from_ulid(Ulid::from_parts(TS, n))
}

pub(crate) fn meta(kind: LedgerKind) -> LedgerMeta {
    LedgerMeta::new(kind, project(1))
}

pub(crate) fn session_id(n: u128) -> SessionId {
    SessionId::from_ulid(Ulid::from_parts(TS, n))
}

pub(crate) fn agent_actor(session: &SessionId) -> Actor {
    Actor::Agent {
        session: session.clone(),
        agent: AgentDescriptor::new("anthropic", "claude", "test"),
        parent: None,
    }
}

/// A `report` event with id `(ts_ms, n)`, by an agent session or a human.
pub(crate) fn report(ts_ms: u64, n: u128, session: Option<&SessionId>) -> Event {
    let actor = session.map_or_else(|| Actor::human("tester", None), agent_actor);
    let body = EventBody::Report(Report {
        title: format!("report {n}"),
        body: None,
        level: Default::default(),
        task: None,
        attempt: None,
    });
    Event::new(
        EventId::from_ulid(Ulid::from_parts(ts_ms, n)),
        OffsetDateTime::UNIX_EPOCH,
        actor,
        body,
    )
}

#[test]
fn repo_is_send_and_sync() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Repo>();
    send_sync::<crate::Ledger<'static>>();
}

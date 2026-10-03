//! Exchanging branches with a remote: fetch, union merge, push.
//!
//! A [`RemoteSpec`] names the remote and carries per-command git config for
//! every command that talks to it (a hosted remote's `Authorization` header).
//! That config only ever goes on the command line (`git -c key=value`):
//! never into `.git/config`, and redacted from every error.

use std::fmt;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gitbots_core::ledger::LedgerKind;

use crate::cli;
use crate::{Ledger, MergeOutcome, Repo};

/// Re-fetch, re-merge and push again this many times after a rejection.
const PUSH_RETRIES: usize = 5;
/// Retries of a fetch that lost a ref-lock race with a concurrent gitbots.
const LOCK_RETRIES: usize = 8;
const REDACTED: &str = "<redacted>";

/// A remote (a configured remote name) plus the `-c key=value` config its
/// commands need. Values passed to [`RemoteSpec::with_bearer`] are secrets:
/// they are redacted from errors and from the `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub struct RemoteSpec {
    name: String,
    config: Vec<(String, String)>,
    secrets: Vec<String>,
}

impl RemoteSpec {
    pub fn new(name: impl Into<String>) -> Self {
        RemoteSpec { name: name.into(), config: Vec::new(), secrets: Vec::new() }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Adds `git -c key=value` to every command that talks to this remote.
    pub fn with_config(mut self, key: &str, value: &str) -> Self {
        self.config.push((key.to_owned(), value.to_owned()));
        self
    }

    /// Authenticates with `http.extraHeader="Authorization: Bearer <token>"`.
    pub fn with_bearer(mut self, token: &str) -> Self {
        if !token.is_empty() {
            self.secrets.push(token.to_owned());
        }
        self.with_config("http.extraHeader", &format!("Authorization: Bearer {token}"))
    }

    /// Whether any per-command config is attached.
    pub fn has_config(&self) -> bool {
        !self.config.is_empty()
    }

    /// `text` with every secret replaced by `<redacted>`.
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for secret in &self.secrets {
            out = out.replace(secret.as_str(), REDACTED);
        }
        out
    }

    /// `git -c ... <args>` where [`Repo::git`] runs, never prompting.
    pub(crate) fn command(&self, repo: &Repo, args: &[&str]) -> Command {
        let mut all: Vec<String> = Vec::with_capacity(self.config.len() * 2 + args.len());
        for (key, value) in &self.config {
            all.push("-c".into());
            all.push(format!("{key}={value}"));
        }
        all.extend(args.iter().map(|a| (*a).to_owned()));
        let mut cmd = repo.command(all);
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd
    }
}

impl From<&str> for RemoteSpec {
    fn from(name: &str) -> Self {
        RemoteSpec::new(name)
    }
}

impl fmt::Debug for RemoteSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys: Vec<&str> = self.config.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("RemoteSpec").field("name", &self.name).field("config", &keys).finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncReport {
    pub branch: String,
    /// Whether the remote has the branch.
    pub fetched: bool,
    /// The (first significant) merge of the remote tip; `None` if not fetched.
    pub outcome: Option<MergeOutcome>,
    /// Whether the remote branch moved.
    pub pushed: bool,
}

/// Syncs each ledger branch with `remote`: fetch it into
/// `refs/remotes/<remote>/<b>`, union-merge, and (if `push`) push without
/// force, retrying after a non-fast-forward rejection. A branch missing on
/// the remote is simply pushed. Ledger pushes skip the user's `pre-push`
/// hook (`--no-verify`): they carry no code and run unattended.
pub fn sync(repo: &Repo, remote: &str, branches: &[&str], push: bool) -> Result<Vec<SyncReport>> {
    let remote = RemoteSpec::new(remote);
    let plan: Vec<_> = branches.iter().map(|b| (*b, &remote)).collect();
    sync_with(repo, &plan, push)
}

/// [`sync`] with a remote (and its per-command config) per ledger branch,
/// e.g. `gitbots/activity` with one hosted repo and `gitbots/logs` with another.
pub fn sync_with(repo: &Repo, plan: &[(&str, &RemoteSpec)], push: bool) -> Result<Vec<SyncReport>> {
    plan.iter().map(|(branch, remote)| sync_branch(repo, remote, branch, push)).collect()
}

fn sync_branch(repo: &Repo, remote: &RemoteSpec, branch: &str, push: bool) -> Result<SyncReport> {
    // The kind only matters for `ensure`; a union merge checks LEDGER.json.
    let ledger = Ledger::new(repo, branch, LedgerKind::Activity);
    let branch = ledger.branch().to_owned();
    let mut report =
        SyncReport { branch: branch.clone(), fetched: false, outcome: None, pushed: false };
    for round in 0..=PUSH_RETRIES {
        let remote_tip = fetch_branch(repo, remote, &branch)?;
        report.fetched = remote_tip.is_some();
        if let Some(remote_tip) = &remote_tip {
            let outcome = ledger.union_merge(remote_tip)?;
            if report.outcome.is_none() || outcome != MergeOutcome::UpToDate {
                report.outcome = Some(outcome);
            }
        }
        let Some(local) = ledger.tip()? else { break };
        if !push || remote_tip.as_ref() == Some(&local) {
            break;
        }
        match push_branch(repo, remote, &branch)? {
            None => {
                report.pushed = true;
                break;
            }
            Some(_) if round < PUSH_RETRIES => backoff(round),
            Some(reason) => bail!(
                "pushing {branch} to {} was rejected {} times: {reason}",
                remote.name(),
                PUSH_RETRIES + 1
            ),
        }
    }
    Ok(report)
}

/// Fetches `branch` from `remote` into `refs/remotes/<remote>/<branch>` and
/// returns its commit; `None` if the remote does not have the branch.
pub fn fetch_branch(repo: &Repo, remote: &RemoteSpec, branch: &str) -> Result<Option<String>> {
    let tracking = format!("refs/remotes/{}/{branch}", remote.name());
    let refspec = format!("+refs/heads/{branch}:{tracking}");
    let args = ["fetch", "-q", "--no-tags", "--no-write-fetch-head", remote.name(), &refspec];
    for round in 0..=LOCK_RETRIES {
        let out = cli::output(remote.command(repo, &args))?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            let tip = repo
                .resolve(&tracking)?
                .with_context(|| format!("fetched {tracking} does not resolve"))?;
            return Ok(Some(tip));
        }
        if stderr.contains("couldn't find remote ref") {
            return Ok(None);
        }
        if round < LOCK_RETRIES && lock_contention(&stderr) {
            backoff(round);
            continue;
        }
        bail!("`git fetch {} {refspec}` failed: {}", remote.name(), remote.redact(stderr.trim()));
    }
    unreachable!("the last round returns or bails")
}

/// `None` if pushed; `Some(reason)` if rejected in a way a re-fetch fixes.
pub(crate) fn push_branch(
    repo: &Repo,
    remote: &RemoteSpec,
    branch: &str,
) -> Result<Option<String>> {
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    let cmd =
        remote.command(repo, &["push", "--porcelain", "--no-verify", remote.name(), &refspec]);
    let out = cli::output(cmd)?;
    if out.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rejected = stdout.lines().find(|line| line.starts_with('!') && line.contains(&refspec));
    const RETRYABLE: &[&str] = &[
        "non-fast-forward",
        "fetch first",
        "lock",
        "failed to update ref",
        "incorrect old value",
        "stale info",
    ];
    match rejected {
        Some(line) if RETRYABLE.iter().any(|r| line.contains(r)) => {
            Ok(Some(remote.redact(line.trim())))
        }
        _ => bail!(
            "`git push {} {refspec}` failed: {}",
            remote.name(),
            remote.redact(&format!(
                "{} {}",
                String::from_utf8_lossy(&out.stderr).trim(),
                stdout.trim()
            ))
        ),
    }
}

/// What happened to one branch in [`push_branches`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushStatus {
    /// The remote did not have the branch.
    Created,
    /// The remote branch fast-forwarded.
    Updated,
    UpToDate,
    /// Refused, e.g. `non-fast-forward`; nothing was forced.
    Rejected {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchPush {
    pub branch: String,
    pub status: PushStatus,
}

/// Pushes local branches (short names) to the same names on `remote` in one
/// `git push`, never forcing: a branch the remote can't fast-forward is
/// reported as [`PushStatus::Rejected`] and the others still go through.
/// Skips the user's `pre-push` hook: gitbots pushes run unattended.
pub fn push_branches(
    repo: &Repo,
    remote: &RemoteSpec,
    branches: &[&str],
) -> Result<Vec<BranchPush>> {
    if branches.is_empty() {
        return Ok(vec![]);
    }
    let refspecs: Vec<String> =
        branches.iter().map(|b| format!("refs/heads/{b}:refs/heads/{b}")).collect();
    let mut args = vec!["push", "--porcelain", "--no-verify", remote.name()];
    args.extend(refspecs.iter().map(String::as_str));
    let out = cli::output(remote.command(repo, &args))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut results = Vec::with_capacity(branches.len());
    for (branch, refspec) in branches.iter().zip(&refspecs) {
        let line = stdout.lines().find_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let (flag, refs, summary) = (fields.next()?, fields.next()?, fields.next());
            (refs == refspec).then(|| (flag.trim(), summary.unwrap_or("").trim()))
        });
        let status = match line {
            Some(("*", _)) => PushStatus::Created,
            Some(("=", _)) => PushStatus::UpToDate,
            Some(("", _)) => PushStatus::Updated,
            Some(("!", summary)) => PushStatus::Rejected { reason: remote.redact(summary) },
            Some((flag, summary)) => {
                bail!(
                    "`git push {}`: unexpected status `{flag}` for {branch}: {summary}",
                    remote.name()
                )
            }
            None => bail!(
                "`git push {} {}` failed: {}",
                remote.name(),
                refspecs.join(" "),
                remote.redact(&format!(
                    "{} {}",
                    String::from_utf8_lossy(&out.stderr).trim(),
                    stdout.trim()
                ))
            ),
        };
        results.push(BranchPush { branch: (*branch).to_owned(), status });
    }
    Ok(results)
}

/// Another git process holds a ref lock this command needed.
fn lock_contention(stderr: &str) -> bool {
    stderr.contains("cannot lock ref")
        || stderr.contains(".lock': File exists")
        || stderr.contains("Unable to create")
}

/// A short, jittered pause before retrying a race with another writer.
fn backoff(round: usize) {
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos() % 40));
    let ms = 20 * (round as u64 + 1) + jitter + u64::from(std::process::id() % 17);
    std::thread::sleep(Duration::from_millis(ms));
}

//! A git repository: gix for objects and refs, the `git` CLI for porcelain.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use gitbots_core::event::DiffStat;
use gix::ObjectId;

use crate::LedgerError;
use crate::cli;
use crate::objects;

/// A repository opened at one worktree (or a bare repository).
///
/// `Send + Sync`: it holds a [`gix::ThreadSafeRepository`] and opens a
/// thread-local handle per operation, so one `Repo` can be shared by threads.
pub struct Repo {
    inner: gix::ThreadSafeRepository,
    git_dir: PathBuf,
    common_dir: PathBuf,
    workdir: Option<PathBuf>,
}

const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    send_sync::<Repo>();
};

impl std::fmt::Debug for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repo")
            .field("git_dir", &self.git_dir)
            .field("workdir", &self.workdir)
            .finish()
    }
}

impl Repo {
    /// Opens the repository containing `path`, including from inside a
    /// linked worktree.
    pub fn discover(path: &Path) -> Result<Repo> {
        let inner = gix::ThreadSafeRepository::discover(path)
            .with_context(|| format!("no git repository at or above {}", path.display()))?;
        let local = inner.to_thread_local();
        let git_dir = canonical(local.git_dir());
        let common_dir = canonical(local.common_dir());
        let workdir = local.workdir().map(canonical);
        Ok(Repo { inner, git_dir, common_dir, workdir })
    }

    /// Opens the repository containing `path`, or runs `git init -b main`
    /// there if there is none.
    pub fn init(path: &Path) -> Result<Repo> {
        if let Ok(repo) = Self::discover(path) {
            return Ok(repo);
        }
        std::fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))?;
        cli::run_in(path, ["init", "-q", "-b", "main"])?;
        Self::discover(path)
    }

    /// Root of the worktree this repo was opened in; `None` if bare.
    pub fn workdir(&self) -> Option<PathBuf> {
        self.workdir.clone()
    }

    /// Root of the main worktree, even when opened from a linked worktree.
    pub fn main_workdir(&self) -> Result<PathBuf> {
        let repo = self.local();
        let main = match repo.kind() {
            gix::repository::Kind::LinkedWorkTree => repo.main_repo()?,
            _ => repo,
        };
        main.workdir().map(canonical).context("the main worktree is bare")
    }

    /// Per-worktree git dir (`.git/worktrees/<name>` in a linked worktree).
    pub fn git_dir(&self) -> PathBuf {
        self.git_dir.clone()
    }

    /// Git dir shared by all worktrees: objects, refs, config, hooks.
    pub fn common_dir(&self) -> PathBuf {
        self.common_dir.clone()
    }

    pub(crate) fn local(&self) -> gix::Repository {
        self.inner.to_thread_local()
    }

    /// Where git commands run: the worktree, or the git dir if bare.
    fn cwd(&self) -> &Path {
        self.workdir.as_deref().unwrap_or(&self.git_dir)
    }

    /// Runs git in the worktree (or git dir if bare). Returns stdout with
    /// trailing whitespace trimmed; a failure carries stderr.
    pub fn git(&self, args: &[&str]) -> Result<String> {
        self.git_in(self.cwd(), args)
    }

    pub fn git_in(&self, dir: &Path, args: &[&str]) -> Result<String> {
        Ok(cli::text(cli::run_in(dir, args)?))
    }

    /// A git command running where [`Repo::git`] runs.
    pub(crate) fn command<I, S>(&self, args: I) -> std::process::Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        cli::command(self.cwd(), args)
    }

    fn git_raw(&self, args: &[&str]) -> Result<Vec<u8>> {
        cli::run_in(self.cwd(), args)
    }

    /// Resolves `rev` to a commit id; `None` if it does not resolve.
    pub fn resolve(&self, rev: &str) -> Result<Option<String>> {
        Ok(resolve_commit(&self.local(), rev)?.map(|id| id.to_string()))
    }

    /// Whether commit `a` is an ancestor of (or equal to) commit `b`.
    pub fn is_ancestor(&self, a: &str, b: &str) -> Result<bool> {
        let repo = self.local();
        let a = resolve_commit(&repo, a)?.with_context(|| format!("unknown revision {a}"))?;
        let b = resolve_commit(&repo, b)?.with_context(|| format!("unknown revision {b}"))?;
        objects::is_ancestor(&repo, a, b)
    }

    /// Short name of the checked-out branch; `None` if detached or unborn.
    pub fn current_branch(&self) -> Result<Option<String>> {
        let repo = self.local();
        Ok(repo.head_ref()?.map(|r| {
            let name = r.name().as_bstr().to_string();
            name.strip_prefix("refs/heads/").map(str::to_owned).unwrap_or(name)
        }))
    }

    pub fn branch_exists(&self, short: &str) -> Result<bool> {
        Ok(self.local().try_find_reference(format!("refs/heads/{short}").as_str())?.is_some())
    }

    /// Short names of the local branches below `prefix` (e.g. `gitbots/attempt`), sorted.
    pub fn branches_under(&self, prefix: &str) -> Result<Vec<String>> {
        let pattern = format!("refs/heads/{}/", prefix.trim_matches('/'));
        let out = self.git(&["for-each-ref", "--format=%(refname)", &pattern])?;
        let mut names: Vec<String> =
            out.lines().filter_map(|l| l.strip_prefix("refs/heads/")).map(str::to_owned).collect();
        names.sort();
        Ok(names)
    }

    /// `(blob id, contents)` of `path` at `rev`; `None` if either is missing.
    pub fn read_blob_at(&self, rev: &str, path: &str) -> Result<Option<(String, Vec<u8>)>> {
        let repo = self.local();
        let Some(tree) = resolve_tree(&repo, rev)? else { return Ok(None) };
        Ok(objects::read_blob(&repo, tree, path)?.map(|(id, data)| (id.to_string(), data)))
    }

    /// Every blob below directory `prefix` at `rev`, recursively and sorted;
    /// empty if `rev` or `prefix` is missing.
    pub fn list_blobs_at(&self, rev: &str, prefix: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let repo = self.local();
        let Some(tree) = resolve_tree(&repo, rev)? else { return Ok(vec![]) };
        objects::list_blobs(&repo, tree, prefix)?
            .into_iter()
            .map(|(path, id)| Ok((path, repo.find_blob(id)?.take_data())))
            .collect()
    }

    /// `git config --get` (all scopes); `None` if unset.
    pub fn config_get(&self, key: &str) -> Result<Option<String>> {
        let out = cli::output(cli::command(self.cwd(), ["config", "--get", key]))?;
        match out.status.code() {
            Some(0) => Ok(Some(cli::text(out.stdout))),
            Some(1) => Ok(None),
            _ => bail!(
                "`git config --get {key}` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        }
    }

    pub fn config_set_local(&self, key: &str, value: &str) -> Result<()> {
        self.git(&["config", "--local", key, value]).map(drop)
    }

    /// `(user.name, user.email)` from git config; `None` without a name.
    pub fn user_identity(&self) -> Option<(String, Option<String>)> {
        let name = self.config_get("user.name").ok().flatten().filter(|n| !n.is_empty())?;
        let email = self.config_get("user.email").ok().flatten().filter(|e| !e.is_empty());
        Some((name, email))
    }

    /// Paths changed in `base...head` (since the merge base), sorted. A
    /// rename lists both the old and the new path.
    pub fn changed_paths(&self, base: &str, head: &str) -> Result<Vec<String>> {
        let range = format!("{base}...{head}");
        let out = self.git_raw(&["diff", "--name-only", "-z", "--no-renames", &range, "--"])?;
        Ok(cli::nul_fields(&out).collect::<BTreeSet<_>>().into_iter().collect())
    }

    /// Diffstat of `base...head`, with rename detection.
    pub fn diffstat(&self, base: &str, head: &str) -> Result<DiffStat> {
        let range = format!("{base}...{head}");
        parse_numstat(&self.git_raw(&[
            "diff",
            "--numstat",
            "-z",
            "--find-renames",
            &range,
            "--",
        ])?)
    }

    /// Diffstat of one commit against its first parent (a root commit
    /// against the empty tree).
    pub fn commit_diffstat(&self, sha: &str) -> Result<DiffStat> {
        let repo = self.local();
        let id = resolve_commit(&repo, sha)?.with_context(|| format!("unknown commit {sha}"))?;
        let parent = repo.find_commit(id)?.parent_ids().next().map(|p| p.detach());
        let from = parent.unwrap_or_else(|| objects::empty_tree(&repo)).to_string();
        parse_numstat(&self.git_raw(&[
            "diff",
            "--numstat",
            "-z",
            "--find-renames",
            &from,
            &id.to_string(),
            "--",
        ])?)
    }

    /// Commits in `base..head`, oldest first.
    pub fn commits_between(&self, base: &str, head: &str) -> Result<Vec<String>> {
        let range = format!("{base}..{head}");
        let out = self.git(&["rev-list", "--reverse", &range, "--"])?;
        Ok(out.lines().map(str::to_owned).collect())
    }

    pub fn commit_subject(&self, sha: &str) -> Result<String> {
        let repo = self.local();
        let id = resolve_commit(&repo, sha)?.with_context(|| format!("unknown commit {sha}"))?;
        let commit = repo.find_commit(id)?;
        Ok(commit.message()?.summary().to_string())
    }

    /// Trailers of a commit message as git parses them (unfolded).
    pub fn commit_trailers(&self, sha: &str) -> Result<Vec<(String, String)>> {
        let out = self.git(&["show", "-s", "--format=%(trailers:only,unfold)", sha, "--"])?;
        Ok(out
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            .collect())
    }

    /// Adds trailers to a commit message file, joining its existing trailer
    /// block (so `Co-Authored-By:` stays contiguous). An identical trailer is
    /// not added twice.
    pub fn add_trailers(&self, msg_file: &Path, trailers: &[(String, String)]) -> Result<()> {
        if trailers.is_empty() {
            return Ok(());
        }
        let mut args = vec!["interpret-trailers".to_owned(), "--in-place".into()];
        args.extend(
            ["--where", "end", "--if-exists", "addIfDifferent", "--if-missing", "add"]
                .map(String::from),
        );
        for (key, value) in trailers {
            if key.is_empty() || key.contains([':', '\n']) || value.contains('\n') {
                bail!("invalid trailer {key:?}: {value:?}");
            }
            args.push("--trailer".into());
            args.push(format!("{key}: {value}"));
        }
        let mut cmd = cli::command(self.cwd(), &args);
        cmd.arg("--").arg(std::path::absolute(msg_file)?);
        cli::run(cmd).map(drop)
    }

    /// `git worktree add -b <new_branch> <path> <base>`.
    pub fn add_worktree(&self, path: &Path, new_branch: &str, base: &str) -> Result<()> {
        let mut cmd = cli::command(self.cwd(), ["worktree", "add", "-q", "-b", new_branch]);
        cmd.arg(std::path::absolute(path)?).arg(base);
        cli::run(cmd).map(drop)
    }

    pub fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        let mut cmd = cli::command(self.cwd(), ["worktree", "remove"]);
        if force {
            cmd.arg("--force");
        }
        cmd.arg(std::path::absolute(path)?);
        cli::run(cmd).map(drop)
    }

    /// Per-worktree git dir of the worktree at `worktree`.
    pub fn worktree_git_dir(&self, worktree: &Path) -> Result<PathBuf> {
        let dir = self.git_in(worktree, &["rev-parse", "--absolute-git-dir"])?;
        Ok(canonical(Path::new(&dir)))
    }

    /// The worktree that has `branch_short` checked out, if any.
    pub fn checked_out_in(&self, branch_short: &str) -> Result<Option<PathBuf>> {
        cli::require_version(2, 36, "`git worktree list -z`")?;
        let out = self.git_raw(&["worktree", "list", "--porcelain", "-z"])?;
        let want = format!("branch refs/heads/{branch_short}");
        let mut current: Option<&str> = None;
        for field in out.split(|b| *b == 0).map(|f| std::str::from_utf8(f).unwrap_or("")) {
            if let Some(path) = field.strip_prefix("worktree ") {
                current = Some(path);
            } else if field == want {
                return Ok(current.map(PathBuf::from));
            }
        }
        Ok(None)
    }

    /// Merges `head` into branch `base_short` with a real merge commit and
    /// returns it. Conflicts are a [`LedgerError::MergeConflict`] and change
    /// nothing. If `head` is already merged, returns `base`'s commit. A
    /// checked-out `base` is moved with `merge --ff-only` in its worktree so
    /// the files update; otherwise the ref is compare-and-swapped.
    pub fn merge_into(&self, base_short: &str, head: &str, message: &str) -> Result<String> {
        cli::require_version(2, 38, "`git merge-tree --write-tree`")?;
        let base_ref = format!("refs/heads/{base_short}");
        let base = self
            .resolve(&base_ref)?
            .with_context(|| format!("branch {base_short} does not exist"))?;
        let head = self.resolve(head)?.with_context(|| format!("unknown revision {head}"))?;
        if self.is_ancestor(&head, &base)? {
            return Ok(base);
        }

        let out = cli::output(cli::command(
            self.cwd(),
            ["merge-tree", "--write-tree", "-z", "--name-only", "--no-messages", &base, &head],
        ))?;
        let mut fields = cli::nul_fields(&out.stdout);
        match out.status.code() {
            Some(0) => {}
            Some(1) => {
                let paths: BTreeSet<_> = fields.skip(1).collect();
                return Err(
                    LedgerError::MergeConflict { paths: paths.into_iter().collect() }.into()
                );
            }
            _ => bail!(
                "`git merge-tree` failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        }
        let tree = fields.next().context("`git merge-tree` printed no tree")?;

        // The user's identity signs accepted merges; gitbots's only if they have none.
        let mut args = Vec::new();
        if self.config_get("user.name")?.is_none() {
            args.extend(["-c".to_owned(), format!("user.name={}", objects::GITBOTS_NAME)]);
        }
        if self.config_get("user.email")?.is_none() {
            args.extend(["-c".to_owned(), format!("user.email={}", objects::GITBOTS_EMAIL)]);
        }
        args.extend(
            ["commit-tree", &tree, "-p", &base, "-p", &head, "-m", message].map(String::from),
        );
        let merged = cli::text(cli::run_in(self.cwd(), &args)?);

        match self.checked_out_in(base_short)? {
            Some(worktree) if worktree.is_dir() => {
                self.git_in(&worktree, &["merge", "-q", "--ff-only", &merged])?;
            }
            _ => {
                let log = format!("gitbots: merge {head} into {base_short}");
                self.git(&["update-ref", "-m", &log, &base_ref, &merged, &base])?;
            }
        }
        Ok(merged)
    }
}

pub(crate) fn resolve_commit(repo: &gix::Repository, rev: &str) -> Result<Option<ObjectId>> {
    let Ok(id) = repo.rev_parse_single(rev) else { return Ok(None) };
    Ok(Some(id.object()?.peel_to_commit()?.id))
}

fn resolve_tree(repo: &gix::Repository, rev: &str) -> Result<Option<ObjectId>> {
    let Ok(id) = repo.rev_parse_single(rev) else { return Ok(None) };
    Ok(Some(id.object()?.peel_to_tree()?.id))
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_owned())
}

/// Parses `git diff --numstat -z`. A record is `added\tdeleted\tpath\0`, or
/// for a rename `added\tdeleted\t\0old\0new\0`; binary files count `-`.
fn parse_numstat(out: &[u8]) -> Result<DiffStat> {
    let mut stat = DiffStat::default();
    let mut fields = out.split(|b| *b == 0);
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let record = String::from_utf8_lossy(record);
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            bail!("unexpected numstat record {record:?}");
        };
        if path.is_empty() {
            fields.next();
            fields.next();
        }
        stat.files += 1;
        stat.insertions += added.parse::<u32>().unwrap_or(0);
        stat.deletions += deleted.parse::<u32>().unwrap_or(0);
    }
    Ok(stat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat() {
        let out = b"1\t2\ta.txt\x000\t0\t\x00old name\x00new name\x00-\t-\tbin\x00";
        assert_eq!(parse_numstat(out).unwrap(), DiffStat { files: 3, insertions: 1, deletions: 2 });
        assert_eq!(parse_numstat(b"").unwrap(), DiffStat::default());
    }
}

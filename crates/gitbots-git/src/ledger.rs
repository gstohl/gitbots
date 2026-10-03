//! An append-only ledger on an orphan branch.
//!
//! Writes build a commit in memory on top of the current tip and move the
//! branch with a compare-and-swap; a lost race re-reads the tip and retries.
//! The working tree and the index are never touched.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gitbots_core::ledger::{LEDGER_META_PATH, LedgerKind, LedgerMeta};
use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind;

use crate::binding::write_atomic;
use crate::objects::{self, check_path, parents};
use crate::repo::resolve_commit;
use crate::{CAS_RETRIES, LedgerError, Repo};

/// Result of [`Ledger::union_merge`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The other commit is already in our history.
    UpToDate,
    /// Our tip was in the other history; the branch moved forward to it.
    FastForward,
    /// A union-merge commit. `conflicts` are paths both sides wrote with
    /// different contents: the blob with the smaller id stays at the path and
    /// both are kept at [`quarantine_path`].
    Merged { commit: String, conflicts: Vec<String> },
}

/// Where union merge keeps blob `oid` of a conflicting `path`.
pub fn quarantine_path(path: &str, oid: &str) -> String {
    format!("conflicts/{path}/{oid}")
}

/// What one round of [`Ledger::update`] decided.
pub(crate) enum Plan<T> {
    /// Leave the branch where it is.
    Keep(T),
    /// Compare-and-swap the branch to `to`.
    Move { to: ObjectId, log: String, value: T },
}

/// One ledger branch, e.g. `gitbots/activity`.
#[derive(Clone, Debug)]
pub struct Ledger<'r> {
    repo: &'r Repo,
    branch: String,
    full: String,
    kind: LedgerKind,
}

impl<'r> Ledger<'r> {
    pub fn new(repo: &'r Repo, branch_short: &str, kind: LedgerKind) -> Self {
        let branch = branch_short.trim_start_matches("refs/heads/").to_owned();
        Ledger { repo, full: format!("refs/heads/{branch}"), branch, kind }
    }

    /// Short branch name.
    pub fn branch(&self) -> &str {
        &self.branch
    }

    pub fn kind(&self) -> LedgerKind {
        self.kind
    }

    pub fn repo(&self) -> &'r Repo {
        self.repo
    }

    pub fn tip(&self) -> Result<Option<String>> {
        Ok(objects::tip_of(&self.repo.local(), &self.full)?.map(|id| id.to_string()))
    }

    /// `LEDGER.json` at the tip.
    pub fn meta(&self) -> Result<Option<LedgerMeta>> {
        let repo = self.repo.local();
        match objects::tip_of(&repo, &self.full)? {
            Some(tip) => read_meta(&repo, tip),
            None => Ok(None),
        }
    }

    /// Creates the branch with a root commit holding `LEDGER.json` if it is
    /// missing; returns whether it did. An existing branch must carry a
    /// compatible `LEDGER.json`, else [`LedgerError::Incompatible`].
    ///
    /// The root commit is deterministic (fixed identity, time of the project
    /// id), so clones that create the same ledger independently agree on it.
    pub fn ensure(&self, meta: &LedgerMeta) -> Result<bool> {
        if meta.kind != self.kind {
            bail!(
                "LEDGER.json kind {:?} does not match the {:?} ledger on {}",
                meta.kind,
                self.kind,
                self.branch
            );
        }
        let repo = self.repo.local();
        let mut json = serde_json::to_vec_pretty(meta)?;
        json.push(b'\n');
        let blob = repo.write_blob(&json)?.detach();
        self.update(&repo, |tip| {
            if let Some(tip) = tip {
                let ours = read_meta(&repo, tip)?.with_context(|| {
                    format!("branch {} exists but has no {LEDGER_META_PATH}", self.branch)
                })?;
                if !ours.compatible(meta) {
                    return Err(self.incompatible(ours, meta.clone()));
                }
                return Ok(Plan::Keep(false));
            }
            let mut editor = repo.edit_tree(objects::empty_tree(&repo))?;
            editor.upsert(LEDGER_META_PATH, EntryKind::Blob, blob)?;
            let tree = editor.write()?.detach();
            let seconds = i64::try_from(meta.project.ulid().timestamp_ms() / 1000).unwrap_or(0);
            let kind = match meta.kind {
                LedgerKind::Activity => "activity",
                LedgerKind::Logs => "logs",
            };
            let message = format!("gitbots: create {kind} ledger for {}\n", meta.project);
            let root = objects::write_commit(
                &repo,
                &message,
                tree,
                vec![],
                gix::date::Time::new(seconds, 0),
            )?;
            Ok(Plan::Move { to: root, log: "gitbots: create ledger".into(), value: true })
        })
    }

    /// Adds or replaces `files` in one commit on top of the tip and returns
    /// it (the unchanged tip if `files` is empty). Bytes are stored verbatim.
    pub fn write(&self, files: Vec<(String, Vec<u8>)>, message: &str) -> Result<String> {
        for (path, _) in &files {
            if path == LEDGER_META_PATH {
                bail!("{LEDGER_META_PATH} is written by `ensure` only");
            }
        }
        let repo = self.repo.local();
        let mut blobs = Vec::with_capacity(files.len());
        for (path, bytes) in files {
            blobs.push((path, repo.write_blob(&bytes)?.detach()));
        }
        self.update(&repo, |tip| {
            let tip = self.require(tip)?;
            if blobs.is_empty() {
                return Ok(Plan::Keep(tip.to_string()));
            }
            let commit = self.commit_files(&repo, tip, &blobs, message)?;
            Ok(Plan::Move { to: commit, log: log_line(message), value: commit.to_string() })
        })
    }

    pub fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let repo = self.repo.local();
        let Some(tree) = self.tip_tree(&repo)? else { return Ok(None) };
        Ok(objects::read_blob(&repo, tree, path)?.map(|(_, data)| data))
    }

    pub fn exists(&self, path: &str) -> Result<bool> {
        let repo = self.repo.local();
        let Some(tree) = self.tip_tree(&repo)? else { return Ok(false) };
        blob_exists(&repo, tree, path)
    }

    /// Paths of every file below directory `prefix` (`""` for all), sorted.
    pub fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let repo = self.repo.local();
        let Some(tree) = self.tip_tree(&repo)? else { return Ok(vec![]) };
        Ok(objects::list_blobs(&repo, tree, prefix)?.into_iter().map(|(path, _)| path).collect())
    }

    /// Every file below directory `prefix` with its contents, sorted by path.
    pub fn read_all(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let repo = self.repo.local();
        let Some(tree) = self.tip_tree(&repo)? else { return Ok(vec![]) };
        objects::list_blobs(&repo, tree, prefix)?
            .into_iter()
            .map(|(path, id)| Ok((path, repo.find_blob(id)?.take_data())))
            .collect()
    }

    /// Merges `other_commit` (any revision) into the branch by tree union.
    ///
    /// The merge commit is deterministic: sorted parents, the fixed gitbots
    /// identity, the later parent's commit time and a fixed message, so two
    /// machines merging the same pair produce the same commit.
    pub fn union_merge(&self, other_commit: &str) -> Result<MergeOutcome> {
        let repo = self.repo.local();
        let other = resolve_commit(&repo, other_commit)?
            .with_context(|| format!("unknown commit {other_commit}"))?;
        self.update(&repo, |tip| {
            let Some(ours) = tip else {
                return Ok(Plan::Move {
                    to: other,
                    log: "gitbots: adopt".into(),
                    value: MergeOutcome::FastForward,
                });
            };
            if objects::is_ancestor(&repo, other, ours)? {
                return Ok(Plan::Keep(MergeOutcome::UpToDate));
            }
            self.check_compatible(&repo, ours, other)?;
            if objects::is_ancestor(&repo, ours, other)? {
                return Ok(Plan::Move {
                    to: other,
                    log: "gitbots: fast-forward".into(),
                    value: MergeOutcome::FastForward,
                });
            }
            let (tree, conflicts) = union_tree(&repo, ours, other)?;
            let ours_time = repo.find_commit(ours)?.time()?.seconds;
            let other_time = repo.find_commit(other)?.time()?.seconds;
            let time = gix::date::Time::new(ours_time.max(other_time), 0);
            let mut parents = vec![ours, other];
            parents.sort();
            let commit =
                objects::write_commit(&repo, "gitbots: union merge\n", tree, parents, time)?;
            let value = MergeOutcome::Merged { commit: commit.to_string(), conflicts };
            Ok(Plan::Move { to: commit, log: "gitbots: union merge".into(), value })
        })
    }

    /// Acknowledges a rewrite: the current tip becomes the remembered one.
    pub fn accept_rewrite(&self) -> Result<()> {
        match objects::tip_of(&self.repo.local(), &self.full)? {
            Some(tip) => self.remember(tip),
            None => match std::fs::remove_file(self.tips_file()) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            },
        }
    }

    /// The compare-and-swap loop behind every ledger update.
    ///
    /// Each round reads the remembered tip, then the current tip (in that
    /// order, so a concurrent writer can only make the remembered tip older),
    /// checks for a rewrite and asks `plan` what to do.
    ///
    /// Updates from one process are serialized per repository and branch.
    /// Building a commit takes far longer than the gap between two updates of
    /// a busy writer, so with compare-and-swap alone the winner of a race
    /// starts its next round first and a second thread can lose every retry.
    /// Other processes are still handled by the compare-and-swap.
    pub(crate) fn update<T>(
        &self,
        repo: &gix::Repository,
        mut plan: impl FnMut(Option<ObjectId>) -> Result<Plan<T>>,
    ) -> Result<T> {
        let lock = self.process_lock();
        let _serialized = lock.lock().unwrap_or_else(PoisonError::into_inner);
        for attempt in 0..=CAS_RETRIES {
            let seen = self.remembered();
            let tip = objects::tip_of(repo, &self.full)?;
            if let Some(seen) = seen {
                let kept =
                    tip.is_some_and(|tip| objects::is_ancestor(repo, seen, tip).unwrap_or(false));
                if !kept {
                    return Err(LedgerError::Rewritten { branch: self.branch.clone() }.into());
                }
            }
            match plan(tip)? {
                Plan::Keep(value) => {
                    if let Some(tip) = tip {
                        self.remember(tip)?;
                    }
                    return Ok(value);
                }
                Plan::Move { to, log, value } => {
                    match objects::cas(repo, &self.full, tip, to, &log) {
                        Ok(()) => {
                            self.remember(to)?;
                            return Ok(value);
                        }
                        // Lost the race to another process: retry at once. A lock
                        // held past gix's wait (`core.filesRefLockTimeout`) gets a
                        // jittered pause first.
                        Err(e) if objects::is_ref_conflict(&e) => {
                            if objects::is_lock_timeout(&e) {
                                backoff(attempt + 1);
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        Err(LedgerError::CasExhausted.into())
    }

    fn process_lock(&self) -> Arc<Mutex<()>> {
        type Locks = Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>;
        static LOCKS: LazyLock<Locks> = LazyLock::new(Locks::default);
        let key = self.repo.common_dir().join(&self.full);
        LOCKS.lock().unwrap_or_else(PoisonError::into_inner).entry(key).or_default().clone()
    }

    pub(crate) fn require(&self, tip: Option<ObjectId>) -> Result<ObjectId> {
        tip.ok_or_else(|| LedgerError::Missing { branch: self.branch.clone() }.into())
    }

    /// Commit of `files` (already written blobs) on top of `tip`.
    pub(crate) fn commit_files(
        &self,
        repo: &gix::Repository,
        tip: ObjectId,
        files: &[(String, ObjectId)],
        message: &str,
    ) -> Result<ObjectId> {
        let base = objects::tree_of(repo, tip)?;
        let paths: BTreeSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        let mut dirs_checked = HashSet::new();
        for path in &paths {
            check_path(path)?;
            if let Some((mode, _)) = objects::lookup(repo, base, path)?
                && mode.is_tree()
            {
                bail!("cannot write file {path}: it is a directory on {}", self.branch);
            }
            for dir in parents(path) {
                if paths.contains(dir) {
                    bail!("cannot write both {dir} and {path}");
                }
                if dirs_checked.insert(dir)
                    && let Some((mode, _)) = objects::lookup(repo, base, dir)?
                    && !mode.is_tree()
                {
                    bail!("cannot write {path}: {dir} is a file on {}", self.branch);
                }
            }
        }
        let mut editor = repo.edit_tree(base)?;
        for (path, id) in files {
            editor.upsert(path.as_str(), EntryKind::Blob, *id)?;
        }
        let tree = editor.write()?.detach();
        objects::write_commit(repo, message, tree, vec![tip], objects::now())
    }

    pub(crate) fn tip_tree(&self, repo: &gix::Repository) -> Result<Option<ObjectId>> {
        objects::tip_of(repo, &self.full)?.map(|tip| objects::tree_of(repo, tip)).transpose()
    }

    fn check_compatible(
        &self,
        repo: &gix::Repository,
        ours: ObjectId,
        theirs: ObjectId,
    ) -> Result<()> {
        match (read_meta(repo, ours)?, read_meta(repo, theirs)?) {
            (Some(a), Some(b)) if !a.compatible(&b) => Err(self.incompatible(a, b)),
            (Some(_), None) => {
                bail!("cannot merge {theirs} into {}: it has no {LEDGER_META_PATH}", self.branch)
            }
            (None, Some(_)) => {
                bail!("cannot merge into {}: the tip has no {LEDGER_META_PATH}", self.branch)
            }
            _ => Ok(()),
        }
    }

    fn incompatible(&self, ours: LedgerMeta, theirs: LedgerMeta) -> anyhow::Error {
        LedgerError::Incompatible { branch: self.branch.clone(), ours, theirs }.into()
    }

    /// `<common_dir>/gitbots/tips/<branch, / as %2F>`: the last tip this
    /// repository saw or wrote, for rewrite detection.
    fn tips_file(&self) -> PathBuf {
        let name = self.branch.replace('%', "%25").replace('/', "%2F");
        self.repo.common_dir().join("gitbots").join("tips").join(name)
    }

    fn remembered(&self) -> Option<ObjectId> {
        let text = std::fs::read_to_string(self.tips_file()).ok()?;
        ObjectId::from_hex(text.trim().as_bytes()).ok()
    }

    fn remember(&self, tip: ObjectId) -> Result<()> {
        if self.remembered() == Some(tip) {
            return Ok(());
        }
        write_atomic(&self.tips_file(), format!("{tip}\n").as_bytes())
    }
}

fn read_meta(repo: &gix::Repository, commit: ObjectId) -> Result<Option<LedgerMeta>> {
    let tree = objects::tree_of(repo, commit)?;
    match objects::read_blob(repo, tree, LEDGER_META_PATH)? {
        Some((_, bytes)) => Ok(Some(
            serde_json::from_slice(&bytes)
                .with_context(|| format!("invalid {LEDGER_META_PATH} at {commit}"))?,
        )),
        None => Ok(None),
    }
}

pub(crate) fn blob_exists(repo: &gix::Repository, tree: ObjectId, path: &str) -> Result<bool> {
    Ok(objects::lookup(repo, tree, path)?.is_some_and(|(mode, _)| mode.is_blob()))
}

/// First line of a commit message, for the reflog.
fn log_line(message: &str) -> String {
    format!("gitbots: {}", message.lines().next().unwrap_or(""))
}

/// A few milliseconds, growing with `attempt`, plus jitter so writers
/// queued on the same lock fall out of step.
fn backoff(attempt: usize) {
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()) % 4000);
    std::thread::sleep(Duration::from_micros(2000 * attempt as u64 + jitter));
}

/// The union of both trees. Order-independent, so either side may be "ours".
fn union_tree(
    repo: &gix::Repository,
    ours: ObjectId,
    theirs: ObjectId,
) -> Result<(ObjectId, Vec<String>)> {
    let our_tree = objects::tree_of(repo, ours)?;
    let their_tree = objects::tree_of(repo, theirs)?;
    let mut union =
        Union { files: HashMap::new(), dirs: HashSet::new(), editor: repo.edit_tree(our_tree)? };
    for (path, id) in objects::all_blobs(repo, our_tree)? {
        let path = utf8_path(path)?;
        union.dirs.extend(parents(&path).map(str::to_owned));
        union.files.insert(path, id);
    }
    let mut conflicts = BTreeSet::new();
    for (path, id) in objects::all_blobs(repo, their_tree)? {
        let path = utf8_path(path)?;
        match union.files.get(&path).copied() {
            Some(our_id) if our_id == id => {}
            Some(our_id) => {
                let keep = our_id.min(id);
                if keep != our_id {
                    union.editor.upsert(path.as_str(), EntryKind::Blob, keep)?;
                    union.files.insert(path.clone(), keep);
                }
                for blob in [our_id, id] {
                    union.add(quarantine_path(&path, &blob.to_string()), blob)?;
                }
                conflicts.insert(path);
            }
            None => union.add(path, id)?,
        }
    }
    Ok((union.editor.write()?.detach(), conflicts.into_iter().collect()))
}

struct Union<'repo> {
    files: HashMap<String, ObjectId>,
    dirs: HashSet<String>,
    editor: gix::object::tree::Editor<'repo>,
}

impl Union<'_> {
    /// Adds a file, refusing to replace a directory with a file or the
    /// other way round (that would silently drop data).
    fn add(&mut self, path: String, id: ObjectId) -> Result<()> {
        match self.files.get(&path) {
            Some(existing) if *existing == id => return Ok(()),
            Some(existing) => {
                bail!("union merge: {path} is {existing} on one side and {id} on the other")
            }
            None => {}
        }
        if self.dirs.contains(&path) {
            bail!("union merge: {path} is a file on one side and a directory on the other");
        }
        if let Some(dir) = parents(&path).find(|dir| self.files.contains_key(*dir)) {
            bail!("union merge: {dir} is a file on one side and a directory on the other");
        }
        self.editor.upsert(path.as_str(), EntryKind::Blob, id)?;
        self.dirs.extend(parents(&path).map(str::to_owned));
        self.files.insert(path, id);
        Ok(())
    }
}

fn utf8_path(path: gix::bstr::BString) -> Result<String> {
    path.to_str().map(str::to_owned).map_err(|_| anyhow::anyhow!("non-UTF-8 ledger path {path:?}"))
}

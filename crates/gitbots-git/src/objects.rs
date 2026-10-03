//! gix plumbing shared by the ledger and the repo helpers.

use anyhow::{Result, bail};
use gix::ObjectId;
use gix::bstr::{BString, ByteSlice};
use gix::refs::transaction::{PreviousValue, RefEdit};
use gix::refs::{FullName, Target};

/// Every ledger commit is made by this identity, never the user's config.
pub(crate) const GITBOTS_NAME: &str = "gitbots";
pub(crate) const GITBOTS_EMAIL: &str = "gitbots@localhost";

pub(crate) fn signature(time: gix::date::Time) -> gix::actor::Signature {
    gix::actor::Signature { name: GITBOTS_NAME.into(), email: GITBOTS_EMAIL.into(), time }
}

pub(crate) fn now() -> gix::date::Time {
    gix::date::Time::now_utc()
}

/// Commit id a full ref name points to, `None` if the ref does not exist.
pub(crate) fn tip_of(repo: &gix::Repository, full: &str) -> Result<Option<ObjectId>> {
    Ok(match repo.try_find_reference(full)? {
        Some(mut r) => Some(r.peel_to_id()?.detach()),
        None => None,
    })
}

/// Points `full` at `new` only if it currently is `expected` (`None`: must
/// not exist). The reflog committer is explicit, so no identity is needed.
pub(crate) fn cas(
    repo: &gix::Repository,
    full: &str,
    expected: Option<ObjectId>,
    new: ObjectId,
    log: &str,
) -> Result<()> {
    let name: FullName = full.try_into()?;
    let previous = match expected {
        Some(id) => PreviousValue::MustExistAndMatch(Target::Object(id)),
        None => PreviousValue::MustNotExist,
    };
    let sig = signature(now());
    let mut time = gix::date::parse::TimeBuf::default();
    repo.edit_references_as(
        [RefEdit::update(name, new, previous, BString::from(log))],
        Some(sig.to_ref(&mut time)),
    )?;
    Ok(())
}

/// True if `err` is a lost compare-and-swap race (ref moved or already
/// exists) or lock contention; re-read the tip and retry.
pub(crate) fn is_ref_conflict(err: &anyhow::Error) -> bool {
    use gix::refs::file::transaction::prepare::{MustNotExist, ReferenceOutOfDate};
    err.chain().filter_map(|e| e.downcast_ref::<gix::Error>()).any(|e| {
        e.downcast_any_ref::<ReferenceOutOfDate>().is_some()
            || e.downcast_any_ref::<MustNotExist>().is_some()
            || e.is_retryable()
    })
}

/// True if `err` is a ref lock still held after gix's wait.
pub(crate) fn is_lock_timeout(err: &anyhow::Error) -> bool {
    err.chain().filter_map(|e| e.downcast_ref::<gix::Error>()).any(gix::Error::is_retryable)
}

pub(crate) fn write_commit(
    repo: &gix::Repository,
    message: &str,
    tree: ObjectId,
    parents: Vec<ObjectId>,
    time: gix::date::Time,
) -> Result<ObjectId> {
    let sig = signature(time);
    let (mut b1, mut b2) = Default::default();
    Ok(repo.new_commit_as(sig.to_ref(&mut b1), sig.to_ref(&mut b2), message, tree, parents)?.id)
}

/// Whether `a` is an ancestor of (or equal to) `b`. Unrelated histories are
/// fine (`merge_base` would error on them).
pub(crate) fn is_ancestor(repo: &gix::Repository, a: ObjectId, b: ObjectId) -> Result<bool> {
    if a == b {
        return Ok(true);
    }
    let cache = repo.commit_graph_if_enabled()?;
    let mut graph = repo.revision_graph(cache.as_ref());
    let bases = repo.merge_bases_many_with_graph(a, &[b], &mut graph)?;
    Ok(bases.iter().any(|id| *id == a))
}

pub(crate) fn tree_of(repo: &gix::Repository, commit: ObjectId) -> Result<ObjectId> {
    Ok(repo.find_commit(commit)?.tree_id()?.detach())
}

pub(crate) fn empty_tree(repo: &gix::Repository) -> ObjectId {
    ObjectId::empty_tree(repo.object_hash())
}

/// The entry at `path` (`a/b/c`) below `tree`.
pub(crate) fn lookup(
    repo: &gix::Repository,
    tree: ObjectId,
    path: &str,
) -> Result<Option<(gix::objs::tree::EntryMode, ObjectId)>> {
    let path = path.trim_matches('/');
    if path.is_empty() {
        return Ok(Some((gix::objs::tree::EntryKind::Tree.into(), tree)));
    }
    let tree = repo.find_tree(tree)?;
    Ok(tree.lookup_entry(path.split('/'))?.map(|e| (e.mode(), e.object_id())))
}

/// Blob contents at `path` below `tree`; `None` if missing or not a blob.
pub(crate) fn read_blob(
    repo: &gix::Repository,
    tree: ObjectId,
    path: &str,
) -> Result<Option<(ObjectId, Vec<u8>)>> {
    match lookup(repo, tree, path)? {
        Some((mode, id)) if mode.is_blob() => Ok(Some((id, repo.find_blob(id)?.take_data()))),
        _ => Ok(None),
    }
}

/// Every blob below `prefix` (a directory; `""` for all), recursively, as
/// sorted `(full path, blob id)`. Empty if `prefix` is missing or a file.
pub(crate) fn list_blobs(
    repo: &gix::Repository,
    tree: ObjectId,
    prefix: &str,
) -> Result<Vec<(String, ObjectId)>> {
    let dir = prefix.trim_matches('/');
    let Some((mode, sub)) = lookup(repo, tree, dir)? else { return Ok(vec![]) };
    if !mode.is_tree() {
        return Ok(vec![]);
    }
    let mut out: Vec<_> = all_blobs(repo, sub)?
        .into_iter()
        .map(|(p, id)| {
            let p = p.to_str_lossy();
            (if dir.is_empty() { p.into_owned() } else { format!("{dir}/{p}") }, id)
        })
        .collect();
    out.sort();
    Ok(out)
}

/// Every blob below `tree` with its path relative to `tree`.
pub(crate) fn all_blobs(
    repo: &gix::Repository,
    tree: ObjectId,
) -> Result<Vec<(BString, ObjectId)>> {
    let tree = repo.find_tree(tree)?;
    Ok(tree
        .traverse()
        .breadthfirst
        .files()?
        .into_iter()
        .filter(|e| e.mode.is_blob())
        .map(|e| (e.filepath, e.oid))
        .collect())
}

/// Direct children of `tree`, as `(name, is_tree, id)` sorted by name descending.
pub(crate) fn children_desc(
    repo: &gix::Repository,
    tree: ObjectId,
) -> Result<Vec<(String, bool, ObjectId)>> {
    let tree = repo.find_tree(tree)?;
    let mut out = Vec::new();
    for entry in tree.iter() {
        let entry = entry?;
        out.push((
            entry.filename().to_str_lossy().into_owned(),
            entry.mode().is_tree(),
            entry.object_id(),
        ));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(out)
}

/// Rejects paths that would make an invalid or surprising tree entry.
pub(crate) fn check_path(path: &str) -> Result<()> {
    let bad = path.is_empty()
        || path.contains(['\0', '\\'])
        || path.split('/').any(|seg| {
            seg.is_empty() || seg == "." || seg == ".." || seg.eq_ignore_ascii_case(".git")
        });
    if bad {
        bail!("invalid ledger path {path:?}");
    }
    Ok(())
}

/// Proper parent directories of `path`: `a/b/c` -> `a`, `a/b`.
pub(crate) fn parents(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(|(i, _)| &path[..i])
}

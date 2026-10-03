//! Read-only access to a repo's git objects, as the Artifacts Workers
//! binding offers it (`readCommit`, `readTree`, `readBlob`, `log`).
//!
//! The indexer ([`crate::index`]) and the diff ([`crate::diff`]) are written
//! against [`TreeSource`], so they are unit-tested natively with an in-memory
//! fake and run in the Worker over the real binding.

use std::collections::HashSet;

/// What a tree entry points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Blob,
    Tree,
    /// A submodule (gitlink, mode `160000`).
    Commit,
}

/// One immediate child of a tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    pub sha: String,
    pub kind: EntryKind,
    /// Git file mode as a number, e.g. `0o100644`, `0o100755`, `0o120000`, `0o40000`.
    pub mode: u32,
}

impl TreeEntry {
    /// Parses a git mode written in octal (`"100644"`, `"40000"`).
    pub fn parse_mode(mode: &str) -> Option<u32> {
        u32::from_str_radix(mode, 8).ok()
    }

    /// The kind a git mode implies.
    pub fn kind_of_mode(mode: u32) -> EntryKind {
        match mode & 0o170000 {
            0o040000 => EntryKind::Tree,
            0o160000 => EntryKind::Commit,
            _ => EntryKind::Blob,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub tree: String,
    /// First parent first.
    pub parents: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("{what} `{id}` not found")]
    NotFound { what: &'static str, id: String },
    #[error("{0}")]
    Backend(String),
}

/// Object reads against one repo. Futures are not `Send`: the Worker runtime
/// is single-threaded and JS promises are not `Send` either.
#[allow(async_fn_in_trait)]
pub trait TreeSource {
    async fn read_commit(&self, sha: &str) -> Result<Commit, SourceError>;

    /// Immediate children of a tree.
    async fn read_tree(&self, sha: &str) -> Result<Vec<TreeEntry>, SourceError>;

    async fn read_blob(&self, sha: &str) -> Result<Vec<u8>, SourceError>;

    /// First-parent history from `sha` (inclusive), newest first, at most
    /// `limit` commits. The Worker overrides this with one `log()` call.
    async fn first_parent_history(
        &self,
        sha: &str,
        limit: usize,
    ) -> Result<Vec<String>, SourceError> {
        walk_first_parents(self, sha, limit).await
    }
}

/// [`TreeSource::first_parent_history`] by reading one commit at a time.
pub async fn walk_first_parents<S: TreeSource + ?Sized>(
    src: &S,
    sha: &str,
    limit: usize,
) -> Result<Vec<String>, SourceError> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = Some(sha.to_owned());
    while let Some(sha) = cursor {
        if out.len() >= limit || !seen.insert(sha.clone()) {
            break;
        }
        cursor = src.read_commit(&sha).await?.parents.into_iter().next();
        out.push(sha);
    }
    Ok(out)
}

/// The entry at `path` (`/`-separated, relative) below `root`, if any.
pub async fn lookup<S: TreeSource + ?Sized>(
    src: &S,
    root: &str,
    path: &str,
) -> Result<Option<TreeEntry>, SourceError> {
    let mut tree = root.to_owned();
    let mut parts = path.split('/').filter(|p| !p.is_empty()).peekable();
    while let Some(name) = parts.next() {
        let Some(entry) = src.read_tree(&tree).await?.into_iter().find(|e| e.name == name) else {
            return Ok(None);
        };
        if parts.peek().is_none() {
            return Ok(Some(entry));
        }
        if entry.kind != EntryKind::Tree {
            return Ok(None);
        }
        tree = entry.sha;
    }
    Ok(None)
}

/// Every blob below `dir` of `root`, as `(path, sha)` with full paths.
pub async fn list_blobs<S: TreeSource + ?Sized>(
    src: &S,
    root: &str,
    dir: &str,
) -> Result<Vec<(String, String)>, SourceError> {
    let dir = dir.trim_end_matches('/');
    let Some(start) = lookup(src, root, dir).await? else { return Ok(vec![]) };
    if start.kind != EntryKind::Tree {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_owned(), start.sha)];
    while let Some((prefix, sha)) = stack.pop() {
        for e in src.read_tree(&sha).await? {
            let path = format!("{prefix}/{}", e.name);
            match e.kind {
                EntryKind::Tree => stack.push((path, e.sha)),
                EntryKind::Blob => out.push((path, e.sha)),
                EntryKind::Commit => {}
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Polls a future that never waits on real IO to completion. For tests
/// against [`mem::MemRepo`], whose futures are always ready.
#[cfg(test)]
pub(crate) fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// An in-memory object store for tests.
#[cfg(test)]
pub(crate) mod mem {
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, HashMap};

    use sha2::{Digest, Sha256};

    use super::*;

    enum Obj {
        Blob(Vec<u8>),
        Tree(Vec<TreeEntry>),
        Commit(Commit),
    }

    #[derive(Default)]
    pub struct MemRepo {
        objects: RefCell<HashMap<String, Obj>>,
        pub tree_reads: Cell<usize>,
        pub blob_reads: Cell<usize>,
    }

    fn hash(kind: &str, bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(kind.as_bytes());
        h.update(bytes);
        h.finalize().iter().take(20).map(|b| format!("{b:02x}")).collect()
    }

    enum Node {
        File(Vec<u8>, u32),
        Dir(BTreeMap<String, Node>),
    }

    impl MemRepo {
        pub fn blob(&self, bytes: &[u8]) -> String {
            let sha = hash("blob", bytes);
            self.objects.borrow_mut().insert(sha.clone(), Obj::Blob(bytes.to_vec()));
            sha
        }

        fn tree_of(&self, entries: Vec<TreeEntry>) -> String {
            let key: String =
                entries.iter().map(|e| format!("{:o} {} {}\n", e.mode, e.name, e.sha)).collect();
            let sha = hash("tree", key.as_bytes());
            self.objects.borrow_mut().insert(sha.clone(), Obj::Tree(entries));
            sha
        }

        fn build(&self, dir: &BTreeMap<String, Node>) -> String {
            let entries = dir
                .iter()
                .map(|(name, node)| match node {
                    Node::File(bytes, mode) => TreeEntry {
                        name: name.clone(),
                        sha: self.blob(bytes),
                        kind: TreeEntry::kind_of_mode(*mode),
                        mode: *mode,
                    },
                    Node::Dir(children) => TreeEntry {
                        name: name.clone(),
                        sha: self.build(children),
                        kind: EntryKind::Tree,
                        mode: 0o40000,
                    },
                })
                .collect();
            self.tree_of(entries)
        }

        /// Builds nested trees from `(path, content)` pairs; returns the root tree.
        pub fn tree(&self, files: &[(&str, &[u8])]) -> String {
            let with_modes: Vec<(&str, &[u8], u32)> =
                files.iter().map(|(p, b)| (*p, *b, 0o100644)).collect();
            self.tree_with_modes(&with_modes)
        }

        pub fn tree_with_modes(&self, files: &[(&str, &[u8], u32)]) -> String {
            let mut root: BTreeMap<String, Node> = BTreeMap::new();
            for (path, bytes, mode) in files {
                let parts: Vec<&str> = path.split('/').collect();
                let mut dir = &mut root;
                for part in &parts[..parts.len() - 1] {
                    let node =
                        dir.entry((*part).to_owned()).or_insert_with(|| Node::Dir(BTreeMap::new()));
                    dir = match node {
                        Node::Dir(d) => d,
                        Node::File(..) => panic!("{path}: file in the way"),
                    };
                }
                dir.insert(parts[parts.len() - 1].to_owned(), Node::File(bytes.to_vec(), *mode));
            }
            self.build(&root)
        }

        pub fn commit(&self, tree: &str, parents: &[&str], message: &str) -> String {
            let key = format!("{tree} {parents:?} {message}");
            let sha = hash("commit", key.as_bytes());
            let commit = Commit {
                sha: sha.clone(),
                tree: tree.to_owned(),
                parents: parents.iter().map(|p| (*p).to_owned()).collect(),
                message: message.to_owned(),
            };
            self.objects.borrow_mut().insert(sha.clone(), Obj::Commit(commit));
            sha
        }
    }

    fn missing(what: &'static str, id: &str) -> SourceError {
        SourceError::NotFound { what, id: id.to_owned() }
    }

    impl TreeSource for MemRepo {
        async fn read_commit(&self, sha: &str) -> Result<Commit, SourceError> {
            match self.objects.borrow().get(sha) {
                Some(Obj::Commit(c)) => Ok(c.clone()),
                _ => Err(missing("commit", sha)),
            }
        }

        async fn read_tree(&self, sha: &str) -> Result<Vec<TreeEntry>, SourceError> {
            self.tree_reads.set(self.tree_reads.get() + 1);
            match self.objects.borrow().get(sha) {
                Some(Obj::Tree(t)) => Ok(t.clone()),
                _ => Err(missing("tree", sha)),
            }
        }

        async fn read_blob(&self, sha: &str) -> Result<Vec<u8>, SourceError> {
            self.blob_reads.set(self.blob_reads.get() + 1);
            match self.objects.borrow().get(sha) {
                Some(Obj::Blob(b)) => Ok(b.clone()),
                _ => Err(missing("blob", sha)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mem::MemRepo;
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(TreeEntry::parse_mode("100644"), Some(0o100644));
        assert_eq!(TreeEntry::kind_of_mode(0o40000), EntryKind::Tree);
        assert_eq!(TreeEntry::kind_of_mode(0o160000), EntryKind::Commit);
        assert_eq!(TreeEntry::kind_of_mode(0o120000), EntryKind::Blob);
    }

    #[test]
    fn lookup_and_list() {
        let repo = MemRepo::default();
        let root = repo.tree(&[("a/b/c.txt", b"c"), ("a/d.txt", b"d"), ("e.txt", b"e")]);
        let hit = block_on(lookup(&repo, &root, "a/b/c.txt")).unwrap().unwrap();
        assert_eq!(block_on(repo.read_blob(&hit.sha)).unwrap(), b"c");
        assert!(block_on(lookup(&repo, &root, "a/x")).unwrap().is_none());
        assert!(block_on(lookup(&repo, &root, "e.txt/x")).unwrap().is_none());
        let all = block_on(list_blobs(&repo, &root, "a/")).unwrap();
        let paths: Vec<&str> = all.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["a/b/c.txt", "a/d.txt"]);
    }

    #[test]
    fn default_history_walks_first_parents() {
        let repo = MemRepo::default();
        let t = repo.tree(&[("f", b"1")]);
        let c1 = repo.commit(&t, &[], "one");
        let side = repo.commit(&t, &[], "side");
        let c2 = repo.commit(&t, &[&c1, &side], "two");
        let c3 = repo.commit(&t, &[&c2], "three");
        assert_eq!(
            block_on(repo.first_parent_history(&c3, 10)).unwrap(),
            [c3.clone(), c2.clone(), c1]
        );
        assert_eq!(block_on(repo.first_parent_history(&c3, 2)).unwrap(), [c3, c2]);
    }
}

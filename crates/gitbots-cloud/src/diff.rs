//! Tree-to-tree diffs computed from git objects, rendered like `git diff`.
//!
//! The Artifacts binding has no diff API, so `/api/attempts/{id}/diff` walks
//! the two root trees (skipping subtrees with equal hashes), reads the
//! changed blobs and renders a unified diff with `similar`. Renames are not
//! detected: a rename shows as a delete plus an add.

use similar::TextDiff;

use crate::source::{EntryKind, SourceError, TreeEntry, TreeSource};

/// `/api/attempts/{id}/diff` is cut at this size (as in `gitbots ui`).
pub const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
/// Files larger than this on either side get a one-line note, not a diff.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
const TRUNCATED: &str = "\n[gitbots: diff truncated at 2 MB]\n";
const NULL_SHA: &str = "0000000";

/// One side of a changed path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Side {
    pub sha: String,
    pub mode: u32,
    pub kind: EntryKind,
}

/// A path whose content or mode differs between two trees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    /// `None`: added.
    pub old: Option<Side>,
    /// `None`: deleted.
    pub new: Option<Side>,
}

fn side(e: &TreeEntry) -> Side {
    Side { sha: e.sha.clone(), mode: e.mode, kind: e.kind }
}

/// Changed files between two root trees, sorted by path.
pub async fn tree_changes<S: TreeSource + ?Sized>(
    src: &S,
    old_tree: Option<&str>,
    new_tree: Option<&str>,
) -> Result<Vec<FileChange>, SourceError> {
    let mut out = Vec::new();
    let mut stack: Vec<(String, Option<String>, Option<String>)> =
        vec![(String::new(), old_tree.map(str::to_owned), new_tree.map(str::to_owned))];
    while let Some((prefix, old, new)) = stack.pop() {
        if old.is_some() && old == new {
            continue;
        }
        let old_entries = match &old {
            Some(sha) => src.read_tree(sha).await?,
            None => vec![],
        };
        let new_entries = match &new {
            Some(sha) => src.read_tree(sha).await?,
            None => vec![],
        };
        let mut names: Vec<&str> =
            old_entries.iter().chain(&new_entries).map(|e| e.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        for name in names {
            let path = if prefix.is_empty() { name.to_owned() } else { format!("{prefix}/{name}") };
            let o = old_entries.iter().find(|e| e.name == name);
            let n = new_entries.iter().find(|e| e.name == name);
            if let (Some(o), Some(n)) = (o, n)
                && o.sha == n.sha
                && o.mode == n.mode
            {
                continue;
            }
            let is_tree = |e: Option<&TreeEntry>| e.is_some_and(|e| e.kind == EntryKind::Tree);
            let (o_tree, n_tree) = (is_tree(o), is_tree(n));
            if o_tree || n_tree {
                stack.push((
                    path.clone(),
                    o.filter(|_| o_tree).map(|e| e.sha.clone()),
                    n.filter(|_| n_tree).map(|e| e.sha.clone()),
                ));
            }
            let o_file = o.filter(|_| !o_tree).map(side);
            let n_file = n.filter(|_| !n_tree).map(side);
            if o_file.is_some() || n_file.is_some() {
                out.push(FileChange { path, old: o_file, new: n_file });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Unified diff of `changes`, reading blobs from `src`, cut at `cap` bytes.
pub async fn unified_diff<S: TreeSource + ?Sized>(
    src: &S,
    changes: &[FileChange],
    cap: usize,
) -> Result<String, SourceError> {
    let mut out = String::new();
    for change in changes {
        let old = read_side(src, change.old.as_ref()).await?;
        let new = read_side(src, change.new.as_ref()).await?;
        out.push_str(&render_change(change, &old, &new));
        if out.len() > cap {
            let mut cut = cap;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
            out.push_str(TRUNCATED);
            break;
        }
    }
    Ok(out)
}

async fn read_side<S: TreeSource + ?Sized>(
    src: &S,
    side: Option<&Side>,
) -> Result<Vec<u8>, SourceError> {
    match side {
        Some(s) if s.kind == EntryKind::Commit => {
            Ok(format!("Subproject commit {}\n", s.sha).into())
        }
        Some(s) => src.read_blob(&s.sha).await,
        None => Ok(vec![]),
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8000)].contains(&0) || std::str::from_utf8(bytes).is_err()
}

/// One file's section of a `git diff`-style unified diff.
pub fn render_change(change: &FileChange, old: &[u8], new: &[u8]) -> String {
    let path = &change.path;
    let mut out = format!("diff --git a/{path} b/{path}\n");
    let old_sha = change.old.as_ref().map_or(NULL_SHA, |s| short(&s.sha));
    let new_sha = change.new.as_ref().map_or(NULL_SHA, |s| short(&s.sha));
    match (&change.old, &change.new) {
        (None, Some(n)) => {
            out.push_str(&format!("new file mode {:06o}\nindex {old_sha}..{new_sha}\n", n.mode));
        }
        (Some(o), None) => {
            out.push_str(&format!(
                "deleted file mode {:06o}\nindex {old_sha}..{new_sha}\n",
                o.mode
            ));
        }
        (Some(o), Some(n)) if o.mode != n.mode => {
            out.push_str(&format!("old mode {:06o}\nnew mode {:06o}\n", o.mode, n.mode));
            if o.sha != n.sha {
                out.push_str(&format!("index {old_sha}..{new_sha}\n"));
            }
        }
        (Some(_), Some(n)) => out.push_str(&format!("index {old_sha}..{new_sha} {:06o}\n", n.mode)),
        (None, None) => return String::new(),
    }
    let a = if change.old.is_some() { format!("a/{path}") } else { "/dev/null".to_owned() };
    let b = if change.new.is_some() { format!("b/{path}") } else { "/dev/null".to_owned() };
    if old.len() > MAX_FILE_BYTES || new.len() > MAX_FILE_BYTES {
        out.push_str(&format!("[gitbots: {path} is too large to diff]\n"));
        return out;
    }
    if is_binary(old) || is_binary(new) {
        out.push_str(&format!("Binary files {a} and {b} differ\n"));
        return out;
    }
    let (old, new) = (String::from_utf8_lossy(old), String::from_utf8_lossy(new));
    let diff = TextDiff::from_lines(old.as_ref(), new.as_ref());
    let mut unified = diff.unified_diff();
    unified.context_radius(3);
    let mut first = true;
    for hunk in unified.iter_hunks() {
        if first {
            out.push_str(&format!("--- {a}\n+++ {b}\n"));
            first = false;
        }
        out.push_str(&hunk.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::block_on;
    use crate::source::mem::MemRepo;

    #[test]
    fn modified_added_deleted_and_binary() {
        let repo = MemRepo::default();
        let shared: Vec<(String, Vec<u8>)> =
            (0..20).map(|i| (format!("big/f{i}.txt"), format!("{i}\n").into_bytes())).collect();
        let mut old_files: Vec<(&str, &[u8])> =
            shared.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        let mut new_files = old_files.clone();
        old_files.extend([
            ("src/lib.rs", b"a\nb\nc\nd\ne\nf\ng\nh\n".as_slice()),
            ("gone.txt", b"bye\n"),
            ("img.png", b"\x89PNG\0\x01"),
        ]);
        new_files.extend([
            ("src/lib.rs", b"a\nb\nc\nD\ne\nf\ng\nh\n".as_slice()),
            ("src/new.rs", b"fn main() {}"),
            ("img.png", b"\x89PNG\0\x02"),
        ]);
        let old = repo.tree(&old_files);
        let new = repo.tree(&new_files);

        repo.tree_reads.set(0);
        let changes = block_on(tree_changes(&repo, Some(&old), Some(&new))).unwrap();
        let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["gone.txt", "img.png", "src/lib.rs", "src/new.rs"]);
        // `big/` has the same hash on both sides and is never read.
        assert_eq!(repo.tree_reads.get(), 4);

        let text = block_on(unified_diff(&repo, &changes, MAX_DIFF_BYTES)).unwrap();
        assert!(text.contains("diff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\n"));
        assert!(text.contains("--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n"));
        assert!(text.contains("Binary files a/img.png and b/img.png differ\n"));
        assert!(text.contains(
            "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,7 +1,7 @@\n a\n b\n c\n-d\n+D\n e\n"
        ));
        assert!(text.contains("new file mode 100644\nindex 0000000.."));
        assert!(text.contains(
            "--- /dev/null\n+++ b/src/new.rs\n@@ -0,0 +1 @@\n+fn main() {}\n\\ No newline at end of file\n"
        ));
    }

    #[test]
    fn file_becomes_directory_and_mode_change() {
        let repo = MemRepo::default();
        let old =
            repo.tree_with_modes(&[("x", b"file\n", 0o100644), ("run.sh", b"echo\n", 0o100644)]);
        let new = repo.tree_with_modes(&[
            ("x/inner", b"now a dir\n", 0o100644),
            ("run.sh", b"echo\n", 0o100755),
        ]);
        let changes = block_on(tree_changes(&repo, Some(&old), Some(&new))).unwrap();
        let paths: Vec<(&str, bool, bool)> =
            changes.iter().map(|c| (c.path.as_str(), c.old.is_some(), c.new.is_some())).collect();
        assert_eq!(paths, [("run.sh", true, true), ("x", true, false), ("x/inner", false, true)]);
        let text = block_on(unified_diff(&repo, &changes, MAX_DIFF_BYTES)).unwrap();
        assert!(
            text.starts_with("diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n")
        );
    }

    #[test]
    fn caps_output() {
        let repo = MemRepo::default();
        let big: String = (0..2000).map(|i| format!("line {i}\n")).collect();
        let old = repo.tree(&[("a.txt", b"")]);
        let new = repo.tree(&[("a.txt", big.as_bytes())]);
        let changes = block_on(tree_changes(&repo, Some(&old), Some(&new))).unwrap();
        let text = block_on(unified_diff(&repo, &changes, 1000)).unwrap();
        assert!(text.ends_with(TRUNCATED));
        assert!(text.len() <= 1000 + TRUNCATED.len());
    }
}

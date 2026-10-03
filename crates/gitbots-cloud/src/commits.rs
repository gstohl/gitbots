//! Commit-graph helpers over [`TreeSource`]: merge bases, `base..head`
//! ranges and commit message parsing, for `/api/attempts/{id}[/diff]`.
//!
//! The binding's `log()` follows first parents only, so these follow first
//! parents too. For attempt branches (linear work on top of a base) this is
//! what `git` computes; across merges of the base into the attempt it can
//! pick an older merge base than git would.

use std::collections::HashSet;

use crate::source::{Commit, SourceError, TreeSource};

/// How far back the first-parent walks look.
pub const HISTORY_LIMIT: usize = 1000;
/// At most this many commits are listed for an attempt.
pub const MAX_RANGE: usize = 250;

/// The newest commit on `head`'s first-parent chain that is also on
/// `base`'s, if one is within [`HISTORY_LIMIT`].
pub async fn merge_base<S: TreeSource + ?Sized>(
    src: &S,
    base: &str,
    head: &str,
) -> Result<Option<String>, SourceError> {
    let on_base: HashSet<String> =
        src.first_parent_history(base, HISTORY_LIMIT).await?.into_iter().collect();
    Ok(src
        .first_parent_history(head, HISTORY_LIMIT)
        .await?
        .into_iter()
        .find(|c| on_base.contains(c)))
}

/// Commits on `head`'s first-parent chain down to (excluding) `base`,
/// oldest first, at most [`MAX_RANGE`]. Empty if `head == base`.
pub async fn commits_between<S: TreeSource + ?Sized>(
    src: &S,
    base: &str,
    head: &str,
) -> Result<Vec<Commit>, SourceError> {
    let mut shas: Vec<String> = src
        .first_parent_history(head, MAX_RANGE + 1)
        .await?
        .into_iter()
        .take_while(|c| c != base)
        .collect();
    shas.truncate(MAX_RANGE);
    let mut out = Vec::with_capacity(shas.len());
    for sha in shas.iter().rev() {
        out.push(src.read_commit(sha).await?);
    }
    Ok(out)
}

/// The subject line: the first paragraph, its lines joined by spaces
/// (what `git log --format=%s` prints).
pub fn subject(message: &str) -> String {
    message
        .trim_start_matches('\n')
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Trailers of the message's last paragraph, unfolded, like
/// `git show -s --format=%(trailers:only,unfold)`. A paragraph counts as a
/// trailer block only if every line is a `Key: value` trailer or a
/// continuation line, and it is not the subject paragraph.
pub fn trailers(message: &str) -> Vec<(String, String)> {
    let paragraphs: Vec<Vec<&str>> = message
        .split("\n\n")
        .map(|p| p.lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>())
        .filter(|p| !p.is_empty())
        .collect();
    let [_, .., last] = paragraphs.as_slice() else { return vec![] };
    let mut out: Vec<(String, String)> = Vec::new();
    for line in last {
        if line.starts_with([' ', '\t']) {
            match out.last_mut() {
                Some((_, value)) => {
                    value.push(' ');
                    value.push_str(line.trim());
                }
                None => return vec![],
            }
            continue;
        }
        let Some((key, value)) = line.split_once(':') else { return vec![] };
        let valid_key = !key.is_empty()
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !key.starts_with('-');
        if !valid_key {
            return vec![];
        }
        out.push((key.to_owned(), value.trim().to_owned()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::block_on;
    use crate::source::mem::MemRepo;

    #[test]
    fn subjects_and_trailers() {
        let msg = "Fix login\nfor real\n\nBody text: not a trailer\nmore\n\nGitbots-Session: ses_1\nCo-Authored-By: Claude\n  <noreply@anthropic.com>\n";
        assert_eq!(subject(msg), "Fix login for real");
        assert_eq!(
            trailers(msg),
            [
                ("Gitbots-Session".to_owned(), "ses_1".to_owned()),
                ("Co-Authored-By".to_owned(), "Claude <noreply@anthropic.com>".to_owned()),
            ]
        );
        assert!(trailers("Key: value only subject").is_empty());
        assert!(trailers("subject\n\nnot a trailer line\nKey: v").is_empty());
    }

    #[test]
    fn ranges_and_merge_base() {
        let repo = MemRepo::default();
        let t = repo.tree(&[("f", b"1")]);
        let base0 = repo.commit(&t, &[], "base 0");
        let a1 = repo.commit(&t, &[&base0], "a1");
        let a2 = repo.commit(&t, &[&a1], "a2\n\nX-Y: z");
        let main1 = repo.commit(&t, &[&base0], "main moved on");

        assert_eq!(block_on(merge_base(&repo, &main1, &a2)).unwrap(), Some(base0.clone()));
        let range = block_on(commits_between(&repo, &base0, &a2)).unwrap();
        let shas: Vec<&str> = range.iter().map(|c| c.sha.as_str()).collect();
        assert_eq!(shas, [a1.as_str(), a2.as_str()]);
        assert!(block_on(commits_between(&repo, &a2, &a2)).unwrap().is_empty());

        let unrelated = repo.commit(&t, &[], "orphan");
        assert_eq!(block_on(merge_base(&repo, &unrelated, &a2)).unwrap(), None);
    }
}

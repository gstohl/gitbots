//! Repo porcelain: lookups, diffs, trailers, worktrees, bindings, merges, hooks.

use std::path::Path;

use anyhow::Result;
use gitbots_core::event::DiffStat;
use gitbots_core::ledger::LedgerKind;

use super::*;
use crate::{
    ACTIVITY_BRANCH, Activity, HOOK_MARKER, LedgerError, read_binding, remove_binding,
    write_binding,
};

fn write(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    std::fs::write(path, text).expect("write");
}

fn commit_all(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

#[test]
fn lookups_config_and_init() -> Result<()> {
    let f = fixture();
    let main = git(&f.dir, &["rev-parse", "main"]);
    assert_eq!(f.repo.resolve("main")?, Some(main.clone()));
    assert_eq!(f.repo.resolve("HEAD")?, Some(main.clone()));
    assert_eq!(f.repo.resolve("nope")?, None);
    assert_eq!(f.repo.current_branch()?, Some("main".into()));
    assert!(f.repo.branch_exists("main")? && !f.repo.branch_exists("nope")?);
    assert_eq!(f.repo.workdir(), Some(f.dir.clone()));
    assert_eq!(f.repo.main_workdir()?, f.dir);
    assert_eq!(f.repo.git_dir(), f.dir.join(".git"));
    assert_eq!(f.repo.common_dir(), f.dir.join(".git"));

    let readme = git(&f.dir, &["rev-parse", "main:README.md"]);
    assert_eq!(f.repo.read_blob_at("main", "README.md")?, Some((readme, b"hello\n".to_vec())));
    assert_eq!(f.repo.read_blob_at("main", "missing")?, None);
    assert_eq!(f.repo.read_blob_at("nope", "README.md")?, None);
    write(&f.dir.join("docs/a/b.md"), "b\n");
    commit_all(&f.dir, "docs");
    assert_eq!(
        f.repo.list_blobs_at("main", "docs")?,
        vec![("docs/a/b.md".to_owned(), b"b\n".to_vec())]
    );
    assert!(f.repo.list_blobs_at("main", "missing/")?.is_empty());
    assert!(f.repo.list_blobs_at("nope", "")?.is_empty());

    assert_eq!(f.repo.config_get("gitbots.test")?, None);
    f.repo.config_set_local("gitbots.test", "yes")?;
    assert_eq!(f.repo.config_get("gitbots.test")?.as_deref(), Some("yes"));
    assert_eq!(f.repo.user_identity(), None);
    f.repo.config_set_local("user.name", "Dana")?;
    assert_eq!(f.repo.user_identity(), Some(("Dana".into(), None)));

    let err = f.repo.git(&["rev-parse", "--verify", "nope"]).unwrap_err();
    assert!(format!("{err:#}").contains("fatal"), "stderr is in the error: {err:#}");
    assert_eq!(f.repo.git(&["rev-parse", "main"])?, git(&f.dir, &["rev-parse", "main"]));

    // init: reuses an existing repository, creates a new one on `main`.
    assert_eq!(Repo::init(&f.dir.join("docs"))?.git_dir(), f.repo.git_dir());
    let fresh = f.tmp.path().join("fresh");
    let repo = Repo::init(&fresh)?;
    assert_eq!(repo.current_branch()?, None, "unborn");
    assert_eq!(git(&fresh, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    Ok(())
}

#[test]
fn changed_paths_and_diffstats() -> Result<()> {
    let f = fixture();
    let root = git(&f.dir, &["rev-parse", "HEAD"]);
    assert_eq!(f.repo.commit_diffstat(&root)?, DiffStat { files: 1, insertions: 1, deletions: 0 });

    write(&f.dir.join("a.txt"), "1\n2\n3\n");
    write(&f.dir.join("old name.txt"), "x\ny\nz\nw\n");
    commit_all(&f.dir, "base");
    git(&f.dir, &["checkout", "-q", "-b", "feature"]);
    write(&f.dir.join("a.txt"), "1\ntwo\n3\n");
    let c1 = commit_all(&f.dir, "edit a");
    git(&f.dir, &["mv", "old name.txt", "new name.txt"]);
    write(&f.dir.join("sp ace/ü.txt"), "u\nv\n");
    let c2 = commit_all(&f.dir, "rename and add");
    git(&f.dir, &["checkout", "-q", "main"]);
    write(&f.dir.join("main-only.txt"), "m\n");
    commit_all(&f.dir, "main moves on");

    assert_eq!(
        f.repo.changed_paths("main", "feature")?,
        ["a.txt", "new name.txt", "old name.txt", "sp ace/ü.txt"]
    );
    assert_eq!(
        f.repo.diffstat("main", "feature")?,
        DiffStat { files: 3, insertions: 3, deletions: 1 }
    );
    assert_eq!(f.repo.commits_between("main", "feature")?, [c1.clone(), c2.clone()]);
    assert_eq!(f.repo.commit_diffstat(&c1)?, DiffStat { files: 1, insertions: 1, deletions: 1 });
    assert_eq!(f.repo.commit_diffstat(&c2)?, DiffStat { files: 2, insertions: 2, deletions: 0 });
    assert_eq!(f.repo.commit_subject(&c2)?, "rename and add");
    Ok(())
}

#[test]
fn trailers_join_the_existing_block() -> Result<()> {
    let f = fixture();
    let msg = f.tmp.path().join("COMMIT_MSG");
    let trailers = [
        ("Gitbots-Session".to_owned(), "ses_1".to_owned()),
        ("Gitbots-Agent".to_owned(), "anthropic/claude@test".to_owned()),
    ];
    let co_author = "Co-Authored-By: Claude <noreply@anthropic.com>";
    write(&msg, &format!("Add feature\n\nLonger body.\n\n{co_author}\n"));
    f.repo.add_trailers(&msg, &trailers)?;
    let expected = format!(
        "Add feature\n\nLonger body.\n\n{co_author}\nGitbots-Session: ses_1\nGitbots-Agent: anthropic/claude@test\n"
    );
    assert_eq!(std::fs::read_to_string(&msg)?, expected);
    f.repo.add_trailers(&msg, &trailers)?;
    assert_eq!(std::fs::read_to_string(&msg)?, expected, "identical trailers are not added twice");

    write(&f.dir.join("f.txt"), "f\n");
    git(&f.dir, &["add", "f.txt"]);
    git(&f.dir, &["commit", "-q", "-F", msg.to_str().expect("utf-8")]);
    let head = git(&f.dir, &["rev-parse", "HEAD"]);
    assert_eq!(f.repo.commit_subject(&head)?, "Add feature");
    let got = f.repo.commit_trailers(&head)?;
    assert_eq!(got[0], ("Co-Authored-By".to_owned(), "Claude <noreply@anthropic.com>".to_owned()));
    assert_eq!(&got[1..], &trailers);

    write(&msg, "Fix\n");
    f.repo.add_trailers(&msg, &trailers[..1])?;
    assert_eq!(std::fs::read_to_string(&msg)?, "Fix\n\nGitbots-Session: ses_1\n");
    assert!(f.repo.add_trailers(&msg, &[("Bad:Key".into(), "v".into())]).is_err());
    assert!(f.repo.commit_trailers(&git(&f.dir, &["rev-parse", "HEAD~1"]))?.is_empty());
    Ok(())
}

#[test]
fn worktrees_bindings_and_discovery() -> Result<()> {
    let f = fixture();
    let wt = f.tmp.path().canonicalize()?.join("wt-a");
    f.repo.add_worktree(&wt, "gitbots/attempt/a", "main")?;
    let wt_git = f.repo.worktree_git_dir(&wt)?;
    assert_eq!(wt_git, f.repo.common_dir().join("worktrees/wt-a"));

    write_binding(&wt_git, "session", " ses_x\n")?;
    assert_eq!(read_binding(&wt_git, "session")?.as_deref(), Some("ses_x"));
    assert_eq!(read_binding(&f.repo.git_dir(), "session")?, None, "bindings are per worktree");
    assert!(write_binding(&wt_git, "../escape", "x").is_err());
    assert!(write_binding(&wt_git, "tips", "x").is_err(), "reserved");

    std::fs::create_dir(wt.join("sub"))?;
    let inner = Repo::discover(&wt.join("sub"))?;
    assert_eq!(inner.git_dir(), wt_git);
    assert_eq!(inner.common_dir(), f.repo.common_dir());
    assert_eq!(inner.workdir(), Some(wt.clone()));
    assert_eq!(inner.main_workdir()?, f.dir);
    assert_eq!(read_binding(&inner.git_dir(), "session")?.as_deref(), Some("ses_x"));
    assert_eq!(inner.current_branch()?.as_deref(), Some("gitbots/attempt/a"));
    assert_eq!(f.repo.checked_out_in("gitbots/attempt/a")?, Some(wt.clone()));
    assert_eq!(f.repo.checked_out_in("main")?, Some(f.dir.clone()));
    assert_eq!(f.repo.checked_out_in("nope")?, None);

    // Ledger writes from a linked worktree land in the shared refs.
    let from_wt = Activity::new(&inner, ACTIVITY_BRANCH);
    from_wt.ensure(&meta(LedgerKind::Activity))?;
    from_wt.append(&[report(TS, 1, None)])?;
    assert_eq!(Activity::new(&f.repo, ACTIVITY_BRANCH).tip()?, from_wt.tip()?);

    remove_binding(&wt_git, "session")?;
    remove_binding(&wt_git, "session")?;
    assert_eq!(read_binding(&wt_git, "session")?, None);
    f.repo.remove_worktree(&wt, false)?;
    assert!(!wt.exists());
    assert_eq!(f.repo.checked_out_in("gitbots/attempt/a")?, None);
    assert!(f.repo.branch_exists("gitbots/attempt/a")?);
    Ok(())
}

#[test]
fn merge_into_a_checked_out_base_updates_its_worktree() -> Result<()> {
    let f = fixture();
    let wt = f.tmp.path().join("wt-a");
    f.repo.add_worktree(&wt, "gitbots/attempt/a", "main")?;
    write(&wt.join("feature.txt"), "feature\n");
    let head = commit_all(&wt, "add feature");
    let base = git(&f.dir, &["rev-parse", "main"]);

    let merged = f.repo.merge_into("main", "gitbots/attempt/a", "Merge attempt a")?;
    assert_eq!(git(&f.dir, &["rev-parse", "main"]), merged);
    assert_eq!(
        std::fs::read_to_string(f.dir.join("feature.txt"))?,
        "feature\n",
        "main worktree updated"
    );
    assert_eq!(git(&f.dir, &["status", "--porcelain"]), "");
    assert_eq!(
        git(&f.dir, &["log", "-1", "--format=%P|%s|%an <%ae>", &merged]),
        format!("{base} {head}|Merge attempt a|gitbots <gitbots@localhost>"),
        "without a user identity gitbots's is used"
    );
    assert_eq!(f.repo.merge_into("main", &head, "again")?, merged, "already merged");
    Ok(())
}

#[test]
fn merge_into_a_branch_nobody_has_checked_out() -> Result<()> {
    let f = fixture();
    f.repo.config_set_local("user.name", "Dana")?;
    f.repo.config_set_local("user.email", "dana@example.com")?;
    git(&f.dir, &["branch", "release"]);
    let wt = f.tmp.path().join("wt-b");
    f.repo.add_worktree(&wt, "gitbots/attempt/b", "release")?;
    write(&wt.join("b.txt"), "b\n");
    let head = commit_all(&wt, "add b");
    let main = git(&f.dir, &["rev-parse", "main"]);

    let merged = f.repo.merge_into("release", "gitbots/attempt/b", "Merge b")?;
    assert_eq!(git(&f.dir, &["rev-parse", "release"]), merged);
    assert_eq!(git(&f.dir, &["rev-parse", &format!("{merged}^2")]), head);
    assert_eq!(
        git(&f.dir, &["log", "-1", "--format=%an <%ae>", &merged]),
        "Dana <dana@example.com>"
    );
    assert_eq!(git(&f.dir, &["rev-parse", "main"]), main, "main untouched");
    assert!(!f.dir.join("b.txt").exists());
    Ok(())
}

#[test]
fn merge_into_reports_conflicts_without_changing_anything() -> Result<()> {
    let f = fixture();
    let wt = f.tmp.path().join("wt-c");
    f.repo.add_worktree(&wt, "gitbots/attempt/c", "main")?;
    write(&wt.join("README.md"), "theirs\n");
    commit_all(&wt, "theirs");
    write(&f.dir.join("README.md"), "ours\n");
    let base = commit_all(&f.dir, "ours");

    let err = f.repo.merge_into("main", "gitbots/attempt/c", "Merge c").unwrap_err();
    match err.downcast_ref::<LedgerError>() {
        Some(LedgerError::MergeConflict { paths }) => assert_eq!(paths, &["README.md".to_owned()]),
        other => panic!("expected MergeConflict, got {other:?} / {err:#}"),
    }
    assert_eq!(git(&f.dir, &["rev-parse", "main"]), base);
    assert_eq!(git(&f.dir, &["status", "--porcelain"]), "");
    assert!(f.repo.merge_into("nope", "gitbots/attempt/c", "m").is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn hooks_install_idempotently_and_politely() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let f = fixture();
    let bin = f.tmp.path().join("bin");
    let calls = f.tmp.path().join("calls.log");
    let fake = bin.join("gitbots");
    write(&fake, &format!("#!/bin/sh\necho \"$@\" >> '{}'\n", calls.display()));
    let failing = bin.join("gitbots-broken");
    write(&failing, "#!/bin/sh\nexit 3\n");
    for p in [&fake, &failing] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
    }

    let report = f.repo.install_hooks(&fake)?;
    assert_eq!(report.dir, f.repo.common_dir().join("hooks"));
    assert_eq!(report.installed, ["prepare-commit-msg", "post-commit"]);
    assert!(report.skipped.is_empty());
    let hook = report.dir.join("post-commit");
    let script = std::fs::read_to_string(&hook)?;
    assert!(script.starts_with("#!/bin/sh\n") && script.contains(HOOK_MARKER));
    assert_eq!(std::fs::metadata(&hook)?.permissions().mode() & 0o777, 0o755);
    assert_eq!(f.repo.install_hooks(&fake)?, report, "idempotent");
    assert_eq!(std::fs::read_to_string(&hook)?, script);

    // The hooks call gitbots with the hook's arguments...
    write(&f.dir.join("x.txt"), "x\n");
    commit_all(&f.dir, "x");
    let log = std::fs::read_to_string(&calls)?;
    assert!(log.contains("hook prepare-commit-msg .git/COMMIT_EDITMSG message\n"), "{log}");
    assert!(log.contains("hook post-commit\n"), "{log}");
    // ...prefer GITBOTS_BIN, and fail open.
    write(&f.dir.join("y.txt"), "y\n");
    git(&f.dir, &["add", "y.txt"]);
    git_env(
        &f.dir,
        &["commit", "-q", "-m", "y"],
        &[("GITBOTS_BIN", failing.to_str().expect("utf-8"))],
    );
    assert_eq!(git(&f.dir, &["log", "-1", "--format=%s"]), "y");
    assert_eq!(std::fs::read_to_string(&calls)?, log, "GITBOTS_BIN won over the baked-in path");

    // A foreign hook is left alone.
    let g = fixture();
    let foreign = g.repo.common_dir().join("hooks/post-commit");
    write(&foreign, "#!/bin/sh\nexit 0\n");
    let report = g.repo.install_hooks(&fake)?;
    assert_eq!(report.installed, ["prepare-commit-msg"]);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].0, "post-commit");
    assert_eq!(std::fs::read_to_string(&foreign)?, "#!/bin/sh\nexit 0\n");

    // core.hooksPath is honored.
    let h = fixture();
    h.repo.config_set_local("core.hooksPath", "githooks")?;
    let report = h.repo.install_hooks(&fake)?;
    assert_eq!(report.dir, h.dir.join("githooks"));
    assert!(h.dir.join("githooks/prepare-commit-msg").is_file());
    assert!(!h.repo.common_dir().join("hooks/prepare-commit-msg").exists());
    Ok(())
}

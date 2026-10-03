//! Sync through a bare remote.

use std::sync::Barrier;

use anyhow::Result;
use gitbots_core::ledger::LedgerKind;

use super::*;
use crate::{
    ACTIVITY_BRANCH, Activity, LOGS_BRANCH, Ledger, MergeOutcome, PushStatus, RemoteSpec,
    SyncReport, fetch_branch, push_branches, sync, sync_with,
};

struct Remote {
    _tmp: tempfile::TempDir,
    bare: PathBuf,
    a: Repo,
    b: Repo,
}

/// A bare remote and two clones (`a`, `b`) with `origin` pointing at it.
fn remote() -> Remote {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical");
    let bare = root.join("remote.git");
    std::fs::create_dir(&bare).expect("mkdir");
    git(&bare, &["init", "-q", "--bare", "-b", "main"]);
    let clone = |name: &str| {
        let dir = root.join(name);
        std::fs::create_dir(&dir).expect("mkdir");
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["remote", "add", "origin", bare.to_str().expect("utf-8")]);
        Repo::discover(&dir).expect("discover")
    };
    let (a, b) = (clone("a"), clone("b"));
    Remote { _tmp: tmp, bare, a, b }
}

fn report_for(
    branch: &str,
    fetched: bool,
    outcome: Option<MergeOutcome>,
    pushed: bool,
) -> SyncReport {
    SyncReport { branch: branch.into(), fetched, outcome, pushed }
}

#[test]
fn sync_two_clones_through_a_bare_remote() -> Result<()> {
    let r = remote();
    let meta = meta(LedgerKind::Activity);
    let (aa, ab) = (Activity::new(&r.a, ACTIVITY_BRANCH), Activity::new(&r.b, ACTIVITY_BRANCH));
    aa.ensure(&meta)?;
    aa.append(&[report(TS, 1, None)])?;

    // Missing on the remote: just push. A branch missing everywhere is a no-op.
    assert_eq!(
        sync(&r.a, "origin", &[ACTIVITY_BRANCH, LOGS_BRANCH], true)?,
        vec![
            report_for(ACTIVITY_BRANCH, false, None, true),
            report_for(LOGS_BRANCH, false, None, false)
        ]
    );
    // A fresh clone adopts the remote ledger; `ensure` then accepts it.
    assert_eq!(
        sync(&r.b, "origin", &[ACTIVITY_BRANCH], true)?,
        vec![report_for(ACTIVITY_BRANCH, true, Some(MergeOutcome::FastForward), false)]
    );
    assert!(!ab.ensure(&meta)?);
    assert_eq!(ab.tip()?, aa.tip()?);

    // Both clones write and sync at the same time, three rounds.
    let barrier = Barrier::new(2);
    std::thread::scope(|s| {
        let handles: Vec<_> = [(&r.a, 1u128), (&r.b, 2)]
            .into_iter()
            .map(|(repo, t)| {
                let barrier = &barrier;
                s.spawn(move || -> Result<()> {
                    let act = Activity::new(repo, ACTIVITY_BRANCH);
                    for round in 0..3u128 {
                        for i in 0..5u128 {
                            act.append(&[report(TS + 10, t * 1000 + round * 10 + i, None)])?;
                        }
                        barrier.wait();
                        let reports = sync(repo, "origin", &[ACTIVITY_BRANCH], true)?;
                        assert!(reports[0].fetched);
                        if let Some(MergeOutcome::Merged { conflicts, .. }) = &reports[0].outcome {
                            assert!(conflicts.is_empty());
                        }
                    }
                    Ok(())
                })
            })
            .collect();
        handles.into_iter().try_for_each(|h| h.join().expect("thread"))
    })?;

    sync(&r.a, "origin", &[ACTIVITY_BRANCH], true)?;
    sync(&r.b, "origin", &[ACTIVITY_BRANCH], true)?;
    sync(&r.a, "origin", &[ACTIVITY_BRANCH], true)?;
    let remote_tip = git(&r.bare, &["rev-parse", "gitbots/activity"]);
    for act in [&aa, &ab] {
        assert_eq!(act.tip()?.as_deref(), Some(remote_tip.as_str()));
        assert_eq!(act.events()?.events.len(), 31);
    }
    assert_eq!(
        sync(&r.b, "origin", &[ACTIVITY_BRANCH], true)?,
        vec![report_for(ACTIVITY_BRANCH, true, Some(MergeOutcome::UpToDate), false)]
    );
    fsck_clean(&r.bare);
    Ok(())
}

#[test]
fn diverged_push_is_rejected_as_retryable_and_sync_recovers() -> Result<()> {
    let r = remote();
    let meta = meta(LedgerKind::Activity);
    let (aa, ab) = (Activity::new(&r.a, ACTIVITY_BRANCH), Activity::new(&r.b, ACTIVITY_BRANCH));
    aa.ensure(&meta)?;
    ab.ensure(&meta)?;
    aa.append(&[report(TS, 1, None)])?;
    ab.append(&[report(TS, 2, None)])?;
    sync(&r.a, "origin", &[ACTIVITY_BRANCH], true)?;

    // b pushes without fetching first: the remote refuses, retryably.
    let reason =
        crate::sync::push_branch(&r.b, &"origin".into(), ACTIVITY_BRANCH)?.expect("rejected");
    assert!(reason.contains("rejected"), "{reason}");
    let err =
        crate::sync::push_branch(&r.b, &"no-such-remote".into(), ACTIVITY_BRANCH).unwrap_err();
    assert!(format!("{err:#}").contains("no-such-remote"), "{err:#}");

    // sync fetches, merges and pushes the merge.
    let reports = sync(&r.b, "origin", &[ACTIVITY_BRANCH], true)?;
    assert!(matches!(reports[0].outcome, Some(MergeOutcome::Merged { .. })) && reports[0].pushed);
    assert_eq!(git(&r.bare, &["rev-parse", "gitbots/activity"]), ab.tip()?.expect("tip"));
    // Without push, sync only fetches and merges.
    assert_eq!(
        sync(&r.a, "origin", &[ACTIVITY_BRANCH], false)?,
        vec![report_for(ACTIVITY_BRANCH, true, Some(MergeOutcome::FastForward), false)]
    );
    assert_eq!(aa.events()?.events.len(), 2);
    Ok(())
}

#[test]
fn per_branch_remotes_carry_their_config_only_on_the_command_line() -> Result<()> {
    let r = remote();
    let logs_bare = r.bare.with_file_name("logs.git");
    std::fs::create_dir(&logs_bare)?;
    git(&logs_bare, &["init", "-q", "--bare", "-b", "main"]);
    git(r.a.workdir().unwrap().as_path(), &["remote", "add", "logs", logs_bare.to_str().unwrap()]);
    let (act, logs) =
        (Activity::new(&r.a, ACTIVITY_BRANCH), Ledger::new(&r.a, LOGS_BRANCH, LedgerKind::Logs));
    act.ensure(&meta(LedgerKind::Activity))?;
    logs.ensure(&meta(LedgerKind::Logs))?;
    act.append(&[report(TS, 1, None)])?;

    // A file remote ignores the header; what matters is where it goes.
    let main = RemoteSpec::new("origin").with_bearer("art_v1_main_secret");
    let side = RemoteSpec::new("logs").with_bearer("art_v1_logs_secret");
    let reports = sync_with(&r.a, &[(ACTIVITY_BRANCH, &main), (LOGS_BRANCH, &side)], true)?;
    assert!(reports.iter().all(|r| r.pushed), "{reports:?}");
    assert_eq!(git(&r.bare, &["rev-parse", ACTIVITY_BRANCH]), act.tip()?.unwrap());
    assert_eq!(git(&logs_bare, &["rev-parse", LOGS_BRANCH]), logs.tip()?.unwrap());
    assert!(git(&r.bare, &["branch", "--list", LOGS_BRANCH]).is_empty());
    let config = std::fs::read_to_string(r.a.common_dir().join("config"))?;
    assert!(!config.contains("art_v1") && !config.to_lowercase().contains("extraheader"));
    assert!(!format!("{main:?}").contains("art_v1"));

    // Secrets never reach an error message.
    let bad = RemoteSpec::new("no-such-remote").with_bearer("no-such");
    let err = format!("{:#}", fetch_branch(&r.a, &bad, ACTIVITY_BRANCH).unwrap_err());
    assert!(err.contains("<redacted>") && !err.contains("no-such "), "{err}");
    let err = format!("{:#}", push_branches(&r.a, &bad, &["main"]).unwrap_err());
    assert!(err.contains("<redacted>"), "{err}");
    Ok(())
}

#[test]
fn push_branches_never_forces() -> Result<()> {
    let r = remote();
    let (a, b) = (r.a.workdir().unwrap(), r.b.workdir().unwrap());
    for dir in [&a, &b] {
        std::fs::write(dir.join("f.txt"), "base\n")?;
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "base"]);
    }
    git(&a, &["branch", "gitbots/attempt/x"]);
    git(&a, &["branch", "gitbots/attempt/y"]);
    assert_eq!(
        r.a.branches_under("gitbots/attempt")?,
        vec!["gitbots/attempt/x", "gitbots/attempt/y"]
    );

    let origin = RemoteSpec::new("origin").with_bearer("art_v1_x");
    let pushed = push_branches(&r.a, &origin, &["main", "gitbots/attempt/x"])?;
    assert!(pushed.iter().all(|p| p.status == PushStatus::Created), "{pushed:?}");
    std::fs::write(a.join("f.txt"), "more\n")?;
    git(&a, &["commit", "-qam", "more"]);
    let pushed = push_branches(&r.a, &origin, &["main", "gitbots/attempt/x", "gitbots/attempt/y"])?;
    let statuses: Vec<_> = pushed.iter().map(|p| p.status.clone()).collect();
    assert_eq!(statuses, vec![PushStatus::Updated, PushStatus::UpToDate, PushStatus::Created]);

    // b's unrelated `main` is refused, not forced, and the other branch still goes.
    git(&b, &["branch", "gitbots/attempt/z"]);
    let pushed = push_branches(&r.b, &"origin".into(), &["main", "gitbots/attempt/z"])?;
    assert!(
        matches!(&pushed[0].status, PushStatus::Rejected { reason } if reason.contains("rejected")),
        "{pushed:?}"
    );
    assert_eq!(pushed[1].status, PushStatus::Created);
    assert_eq!(git(&r.bare, &["rev-parse", "main"]), git(&a, &["rev-parse", "main"]));
    Ok(())
}

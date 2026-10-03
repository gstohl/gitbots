//! Ledger, activity and logs.

use anyhow::Result;
use gitbots_core::action::LogRef;
use gitbots_core::event::{Event, EventBody, SessionStarted};
use gitbots_core::id::{EventId, Ulid};
use gitbots_core::identity::{AgentDescriptor, Session};
use gitbots_core::ledger::{LedgerKind, LedgerMeta, event_path, idem_path, session_path};
use time::OffsetDateTime;
use time::macros::datetime;

use super::*;
use crate::ledger::Plan;
use crate::{
    ACTIVITY_BRANCH, Activity, LOGS_BRANCH, Ledger, LedgerError, Logs, MergeOutcome,
    quarantine_path,
};

fn ledger_error(err: &anyhow::Error) -> &LedgerError {
    err.downcast_ref::<LedgerError>()
        .unwrap_or_else(|| panic!("expected a LedgerError, got {err:#}"))
}

#[test]
fn append_read_list_round_trip() -> Result<()> {
    let f = fixture();
    assert_eq!(f.repo.user_identity(), None, "tests must run without a git identity");
    let act = Activity::new(&f.repo, ACTIVITY_BRANCH);
    assert_eq!(act.tip()?, None);
    assert_eq!(act.meta()?, None);
    assert_eq!(act.latest_event_id()?, None);

    let err = act.append(&[report(TS, 1, None)]).unwrap_err();
    assert!(
        matches!(ledger_error(&err), LedgerError::Missing { branch } if branch == ACTIVITY_BRANCH)
    );

    let meta = meta(LedgerKind::Activity);
    assert!(act.ensure(&meta)?);
    assert!(!act.ensure(&meta)?, "second ensure is a no-op");
    assert_eq!(act.meta()?, Some(meta.clone()));
    assert!(act.ensure(&LedgerMeta::new(LedgerKind::Logs, project(1))).is_err(), "kind mismatch");

    let ses = session_id(9);
    let events = vec![
        report(TS, 1, None),
        report(TS + 1, 2, Some(&ses)),
        report(TS + 3_600_000, 3, Some(&ses)),
    ];
    let out = act.append(&events)?;
    assert_eq!(out.written, events.iter().map(|e| e.id.clone()).collect::<Vec<_>>());
    assert!(out.duplicates.is_empty());
    assert_eq!(act.tip()?, out.commit);

    let read = act.events()?;
    assert_eq!(read.events, events);
    assert!(read.unreadable.is_empty());
    let mut paths: Vec<_> = events.iter().map(event_path).collect();
    paths.sort();
    assert_eq!(act.list("events")?, paths);
    assert_eq!(act.list("events/")?, paths);
    assert_eq!(act.list("")?.len(), 4, "events + LEDGER.json");
    assert!(act.list("nope")?.is_empty());
    assert!(act.list("LEDGER.json")?.is_empty(), "a file is not a directory");
    assert!(act.exists(&paths[0])?);
    assert!(!act.exists("events")?, "a directory is not a file");
    let back: Event =
        serde_json::from_slice(&act.read(&event_path(&events[1]))?.expect("written"))?;
    assert_eq!(back, events[1]);
    assert_eq!(act.read("nope.json")?, None);
    assert_eq!(act.read_all("events")?.len(), 3);
    assert_eq!(act.latest_event_id()?, Some(events[2].id.clone()));

    // One event: "<kind>: <summary>"; the git CLI sees the fixed identity.
    act.append(&[report(TS + 5, 4, None)])?;
    assert_eq!(
        git(&f.dir, &["log", "-1", "--format=%an <%ae>|%cn <%ce>|%s", "gitbots/activity"]),
        "gitbots <gitbots@localhost>|gitbots <gitbots@localhost>|report: report 4"
    );
    assert_eq!(
        git(&f.dir, &["log", "--format=%s", "gitbots/activity"]).lines().nth(1),
        Some("3 events")
    );
    let on_disk: LedgerMeta =
        serde_json::from_str(&git(&f.dir, &["show", "gitbots/activity:LEDGER.json"]))?;
    assert_eq!(on_disk, meta);

    // Unparseable event files are reported, not fatal.
    let garbage = "events/2026/10/03/10/_/evt_garbage.json";
    act.write(vec![(garbage.into(), b"{".to_vec())], "garbage")?;
    assert_eq!(act.events()?.unreadable, vec![garbage.to_owned()]);

    let tip = act.tip()?.expect("tip");
    assert_eq!(act.write(vec![], "nothing")?, tip, "no empty commits");

    // Paths are validated; files and directories never replace each other.
    assert!(act.write(vec![("../escape".into(), vec![])], "bad").is_err());
    assert!(act.write(vec![("a//b".into(), vec![])], "bad").is_err());
    assert!(act.write(vec![("LEDGER.json".into(), vec![])], "bad").is_err());
    assert!(act.write(vec![("events".into(), vec![])], "dir").is_err());
    assert!(act.write(vec![("LEDGER.json/x".into(), vec![])], "file").is_err());
    assert!(act.write(vec![("x".into(), vec![]), ("x/y".into(), vec![])], "both").is_err());

    // Nothing touched the working tree or the index.
    assert_eq!(git(&f.dir, &["status", "--porcelain"]), "");
    fsck_clean(&f.dir);
    Ok(())
}

#[test]
fn idempotency_keys_dedupe() -> Result<()> {
    let f = fixture();
    let act = Activity::new(&f.repo, ACTIVITY_BRANCH);
    act.ensure(&meta(LedgerKind::Activity))?;

    let a = report(TS, 1, None).with_idem("commit:abc");
    let b = report(TS, 2, None).with_idem("commit:abc");
    let c = report(TS, 3, None);
    let out = act.append(&[a.clone(), b.clone(), c.clone()])?;
    assert_eq!(out.written, vec![a.id.clone(), c.id.clone()]);
    assert_eq!(out.duplicates, vec![b.id.clone()], "same key earlier in the batch");
    assert_eq!(act.read(&idem_path("commit:abc"))?.as_deref(), Some(&b"commit:abc"[..]));

    let tip = act.tip()?;
    let out = act.append(&[b.clone(), c.clone()])?;
    assert_eq!(out.commit, None);
    assert!(out.written.is_empty());
    assert_eq!(
        out.duplicates,
        vec![b.id.clone(), c.id.clone()],
        "key already recorded; id already recorded"
    );
    assert_eq!(act.tip()?, tip, "no empty commit");
    assert_eq!(act.events()?.events, vec![a, c]);
    Ok(())
}

#[test]
fn event_strings_are_redacted() -> Result<()> {
    let f = fixture();
    let act = Activity::new(&f.repo, ACTIVITY_BRANCH);
    act.ensure(&meta(LedgerKind::Activity))?;
    let mut leaky = report(TS, 1, None);
    if let EventBody::Report(r) = &mut leaky.body {
        r.body = Some("ran with GITHUB_TOKEN=ghp_0123456789abcdefghijklmnopqrstuvwxyzAB".into());
    }
    act.append(&[leaky.clone(), report(TS, 2, None)])?;
    let read = act.events()?;
    let EventBody::Report(r) = &read.events[0].body else { panic!("report") };
    assert_eq!(r.body.as_deref(), Some("ran with GITHUB_TOKEN=[REDACTED]"));
    assert_eq!(read.events[0].id, leaky.id);
    assert_eq!(read.events[1], report(TS, 2, None), "clean events round-trip unchanged");
    let raw = git(&f.dir, &["show", &format!("gitbots/activity:{}", event_path(&read.events[1]))]);
    assert!(raw.starts_with("{\n  \"v\": 1,\n  \"id\""), "field order kept: {raw}");
    Ok(())
}

#[test]
fn sessions_are_recorded_with_their_start_event() -> Result<()> {
    let f = fixture();
    let act = Activity::new(&f.repo, ACTIVITY_BRANCH);
    act.ensure(&meta(LedgerKind::Activity))?;
    let session = |n: u128, parent: Option<u128>| Session {
        id: session_id(n),
        agent: AgentDescriptor::new("anthropic", "claude", "test"),
        parent: parent.map(session_id),
        role: Some("implementer".into()),
        operator: Some("tester".into()),
        external_id: None,
        label: None,
        started_at: OffsetDateTime::UNIX_EPOCH,
    };
    let started = |s: &Session, n: u128| {
        Event::new(
            EventId::from_ulid(Ulid::from_parts(TS, n)),
            OffsetDateTime::UNIX_EPOCH,
            s.actor(),
            EventBody::SessionStarted(SessionStarted { session: s.clone() }),
        )
        .with_idem(format!("session:{}", s.id))
    };

    let main = session(2, None);
    let commit = act.put_session(&main, &started(&main, 100))?;
    assert_eq!(act.tip()?.as_deref(), Some(commit.as_str()));
    assert!(act.exists(&session_path(&main.id))?);
    assert_eq!(act.sessions()?, vec![main.clone()]);
    assert_eq!(act.events()?.events, vec![started(&main, 100)]);
    assert_eq!(
        git(&f.dir, &["log", "-1", "--format=%s", "gitbots/activity"]),
        "session.started: anthropic/claude@test started"
    );
    assert_eq!(
        act.put_session(&main, &started(&main, 100))?,
        commit,
        "already recorded: no new commit"
    );

    let sub = session(1, Some(2));
    act.put_session(&sub, &started(&sub, 101))?;
    assert_eq!(act.sessions()?, vec![sub, main], "sorted by id");
    Ok(())
}

#[test]
fn concurrent_writers_all_land() -> Result<()> {
    let f = fixture();
    Activity::new(&f.repo, ACTIVITY_BRANCH).ensure(&meta(LedgerKind::Activity))?;
    let second = Repo::discover(&f.dir)?; // a separate handle, like another process
    std::thread::scope(|s| {
        let handles: Vec<_> = [(&f.repo, 1u128), (&second, 2)]
            .into_iter()
            .map(|(repo, t)| {
                s.spawn(move || -> Result<()> {
                    let act = Activity::new(repo, ACTIVITY_BRANCH);
                    for i in 0..20u128 {
                        act.append(&[report(TS + i as u64, t * 1000 + i, None)])?;
                    }
                    Ok(())
                })
            })
            .collect();
        handles.into_iter().try_for_each(|h| h.join().expect("thread"))
    })?;
    let act = Activity::new(&f.repo, ACTIVITY_BRANCH);
    let read = act.events()?;
    assert_eq!(read.events.len(), 40);
    assert_eq!(
        git(&f.dir, &["rev-list", "--count", "gitbots/activity"]),
        "41",
        "linear: root + 40"
    );
    fsck_clean(&f.dir);
    Ok(())
}

#[test]
fn a_lost_race_is_retried_on_the_new_tip() -> Result<()> {
    let f = fixture();
    let act = Activity::new(&f.repo, ACTIVITY_BRANCH);
    act.ensure(&meta(LedgerKind::Activity))?;
    // Another process moves the branch between our read and our swap.
    let sneak_in = |tip: &str| {
        let commit = git(
            &f.dir,
            &["commit-tree", &format!("{tip}^{{tree}}"), "-p", tip, "-m", "concurrent"],
        );
        git(&f.dir, &["update-ref", "refs/heads/gitbots/activity", &commit, tip]);
        commit
    };
    let repo = f.repo.local();
    let mut seen = Vec::new();
    let mut concurrent = String::new();
    let ours = act.0.update(&repo, |tip| {
        let tip = tip.expect("tip");
        seen.push(tip);
        if seen.len() == 1 {
            concurrent = sneak_in(&tip.to_string());
        }
        let commit = act.0.commit_files(&repo, tip, &[], "ours")?;
        Ok(Plan::Move { to: commit, log: "test".into(), value: commit.to_string() })
    })?;
    assert_eq!(seen.len(), 2, "lost once, then rebuilt on the new tip");
    assert_eq!(seen[1].to_string(), concurrent);
    assert_eq!(act.tip()?, Some(ours.clone()));
    assert_eq!(git(&f.dir, &["rev-parse", &format!("{ours}^")]), concurrent);

    // A writer that loses every round gives up.
    let mut rounds = 0;
    let err = act
        .0
        .update(&repo, |tip| {
            rounds += 1;
            let tip = tip.expect("tip");
            sneak_in(&tip.to_string());
            let commit = act.0.commit_files(&repo, tip, &[], "never lands")?;
            Ok(Plan::Move { to: commit, log: "test".into(), value: () })
        })
        .unwrap_err();
    assert!(matches!(ledger_error(&err), LedgerError::CasExhausted));
    assert_eq!(rounds, crate::CAS_RETRIES + 1);
    Ok(())
}

#[test]
fn rewrites_are_detected() -> Result<()> {
    let f = fixture();
    let act = Activity::new(&f.repo, ACTIVITY_BRANCH);
    let meta = meta(LedgerKind::Activity);
    act.ensure(&meta)?;
    act.append(&[report(TS, 1, None)])?;

    // A plain fast-forward by someone else is fine.
    let other = Repo::discover(&f.dir)?;
    Activity::new(&other, ACTIVITY_BRANCH).append(&[report(TS, 2, None)])?;
    act.append(&[report(TS, 3, None)])?;

    // Point the branch at an unrelated commit.
    let main = git(&f.dir, &["rev-parse", "main"]);
    git(&f.dir, &["update-ref", "refs/heads/gitbots/activity", &main]);
    for err in [
        act.append(&[report(TS, 4, None)]).unwrap_err(),
        act.write(vec![("x".into(), vec![])], "x").unwrap_err(),
        act.union_merge(&main).unwrap_err(),
        act.ensure(&meta).unwrap_err(),
    ] {
        assert!(
            matches!(ledger_error(&err), LedgerError::Rewritten { branch } if branch == ACTIVITY_BRANCH)
        );
    }
    act.accept_rewrite()?;
    act.write(vec![("x".into(), b"1".to_vec())], "after accepting")?;

    // Deleting the branch is a rewrite too.
    git(&f.dir, &["update-ref", "-d", "refs/heads/gitbots/activity"]);
    let err = act.ensure(&meta).unwrap_err();
    assert!(matches!(ledger_error(&err), LedgerError::Rewritten { .. }));
    act.accept_rewrite()?;
    assert!(act.ensure(&meta)?);
    Ok(())
}

/// Fetches `src`'s activity branch into `dst` and returns its commit.
fn fetch_peer(dst: &Fixture, src: &Fixture) -> String {
    let refspec = "+refs/heads/gitbots/activity:refs/remotes/peer/gitbots/activity";
    git(&dst.dir, &["fetch", "-q", src.dir.to_str().expect("utf-8"), refspec]);
    git(&dst.dir, &["rev-parse", "refs/remotes/peer/gitbots/activity"])
}

#[test]
fn union_merge_cases_and_determinism() -> Result<()> {
    let (a, b) = (fixture(), fixture());
    let (la, lb) =
        (Activity::new(&a.repo, ACTIVITY_BRANCH), Activity::new(&b.repo, ACTIVITY_BRANCH));
    let meta = meta(LedgerKind::Activity);
    la.ensure(&meta)?;
    lb.ensure(&meta)?;
    let root = la.tip()?.expect("root");
    assert_eq!(
        lb.tip()?.as_deref(),
        Some(root.as_str()),
        "independent clones create the same root"
    );

    // Fast-forward, then up to date.
    la.append(&[report(TS, 1, None)])?;
    let a_tip = fetch_peer(&b, &a);
    assert_eq!(lb.union_merge(&a_tip)?, MergeOutcome::FastForward);
    assert_eq!(lb.tip()?.as_deref(), Some(a_tip.as_str()));
    assert_eq!(lb.union_merge(&a_tip)?, MergeOutcome::UpToDate);
    assert_eq!(la.union_merge(&root)?, MergeOutcome::UpToDate);

    // Diverge, with one path written differently on each side.
    la.append(&[report(TS, 2, None)])?;
    la.write(vec![("shared/x.json".into(), b"A".to_vec())], "a")?;
    lb.append(&[report(TS, 3, None)])?;
    lb.write(vec![("shared/x.json".into(), b"B".to_vec())], "b")?;
    let oid_a = git(&a.dir, &["rev-parse", "gitbots/activity:shared/x.json"]);
    let oid_b = git(&b.dir, &["rev-parse", "gitbots/activity:shared/x.json"]);
    let (from_b, from_a) = (fetch_peer(&a, &b), fetch_peer(&b, &a));

    let MergeOutcome::Merged { commit: ma, conflicts } = la.union_merge(&from_b)? else {
        panic!("expected a merge")
    };
    let MergeOutcome::Merged { commit: mb, conflicts: cb } = lb.union_merge(&from_a)? else {
        panic!("expected a merge")
    };
    assert_eq!(ma, mb, "both machines produce the identical merge commit");
    assert_eq!(conflicts, vec!["shared/x.json".to_owned()]);
    assert_eq!(cb, conflicts);

    let (keep, keep_bytes) = if oid_a < oid_b { (&oid_a, b"A") } else { (&oid_b, b"B") };
    for l in [&la, &lb] {
        assert_eq!(
            l.read("shared/x.json")?.as_deref(),
            Some(&keep_bytes[..]),
            "smaller blob id {keep} wins"
        );
        assert_eq!(l.read(&quarantine_path("shared/x.json", &oid_a))?.as_deref(), Some(&b"A"[..]));
        assert_eq!(l.read(&quarantine_path("shared/x.json", &oid_b))?.as_deref(), Some(&b"B"[..]));
        let ids: Vec<_> = l.events()?.events.into_iter().map(|e| e.id).collect();
        assert_eq!(ids, [1, 2, 3].map(|n| report(TS, n, None).id));
    }
    let mut parents = [from_a.clone(), from_b.clone()];
    parents.sort();
    assert_eq!(
        git(&a.dir, &["log", "-1", "--format=%P|%an <%ae>|%s", &ma]),
        format!("{}|gitbots <gitbots@localhost>|gitbots: union merge", parents.join(" "))
    );
    assert_eq!(la.union_merge(&from_b)?, MergeOutcome::UpToDate, "idempotent");
    fsck_clean(&a.dir);
    fsck_clean(&b.dir);
    Ok(())
}

#[test]
fn union_merge_refuses_incompatible_ledgers_and_clashes() -> Result<()> {
    let (a, b, c) = (fixture(), fixture(), fixture());
    let la = Activity::new(&a.repo, ACTIVITY_BRANCH);
    la.ensure(&meta(LedgerKind::Activity))?;
    la.append(&[report(TS, 1, None)])?;
    let lc = Activity::new(&c.repo, ACTIVITY_BRANCH);
    lc.ensure(&LedgerMeta::new(LedgerKind::Activity, project(2)))?;
    lc.append(&[report(TS, 2, None)])?;

    let tip = la.tip()?;
    let err = la.union_merge(&fetch_peer(&a, &c)).unwrap_err();
    match ledger_error(&err) {
        LedgerError::Incompatible { branch, ours, theirs } => {
            assert_eq!(branch, ACTIVITY_BRANCH);
            assert_eq!((&ours.project, &theirs.project), (&project(1), &project(2)));
        }
        other => panic!("expected Incompatible, got {other:?}"),
    }
    assert_eq!(la.tip()?, tip, "nothing merged");

    // A file on one side, a directory on the other: refuse rather than lose data.
    let lb = Activity::new(&b.repo, ACTIVITY_BRANCH);
    lb.ensure(&meta(LedgerKind::Activity))?;
    lb.write(vec![("clash/inner.json".into(), b"dir".to_vec())], "dir")?;
    la.write(vec![("clash".into(), b"file".to_vec())], "file")?;
    let tip = la.tip()?;
    let err = la.union_merge(&fetch_peer(&a, &b)).unwrap_err();
    assert!(format!("{err:#}").contains("directory on the other"), "{err:#}");
    assert_eq!(la.tip()?, tip);
    Ok(())
}

#[test]
fn latest_event_id_descends_the_newest_shard() -> Result<()> {
    let f = fixture();
    let act = Activity::new(&f.repo, ACTIVITY_BRANCH);
    act.ensure(&meta(LedgerKind::Activity))?;
    let ms =
        |t: OffsetDateTime| u64::try_from(t.unix_timestamp_nanos() / 1_000_000).expect("positive");
    let (early, late) = (session_id(1), session_id(u128::from(u64::MAX))); // `early` sorts first
    let old = report(ms(datetime!(2025-12-31 23:59 UTC)), 1, Some(&late));
    act.append(std::slice::from_ref(&old))?;
    assert_eq!(act.latest_event_id()?, Some(old.id.clone()));

    let events = [
        report(ms(datetime!(2026-01-01 04:59 UTC)), 2, None),
        report(ms(datetime!(2026-01-01 05:10 UTC)), 3, Some(&late)),
        report(ms(datetime!(2026-01-01 05:20 UTC)), 4, None),
        report(ms(datetime!(2026-01-01 05:30 UTC)), 5, Some(&early)),
        report(ms(datetime!(2025-06-01 12:00 UTC)), 6, Some(&early)),
    ];
    act.append(&events)?;
    // A shard that sorts last but holds no event file is skipped.
    act.write(vec![("events/9999/README".into(), b"not an event".to_vec())], "junk")?;
    let newest = events[3].id.clone();
    assert_eq!(act.latest_event_id()?, Some(newest.clone()));
    assert_eq!(act.events()?.events.into_iter().map(|e| e.id).max(), Some(newest));
    Ok(())
}

#[test]
fn logs_are_redacted_and_batched() -> Result<()> {
    let f = fixture();
    let logs = Logs::new(&f.repo, LOGS_BRANCH);
    assert!(logs.ensure(&meta(LedgerKind::Activity)).is_err(), "kind mismatch");
    logs.ensure(&meta(LedgerKind::Logs))?;

    let path = "runs/2026/10/03/run_x/test.log";
    let log = logs
        .put(path, b"ok\nexport GITHUB_TOKEN=ghp_0123456789abcdefghijklmnopqrstuvwxyzAB\ndone\n")?;
    assert_eq!(log, LogRef { branch: LOGS_BRANCH.into(), path: path.into() });
    let stored = String::from_utf8(logs.read(path)?.expect("stored"))?;
    assert!(!stored.contains("ghp_0123"), "{stored}");
    assert!(
        stored.contains("[REDACTED]") && stored.starts_with("ok\n") && stored.ends_with("done\n"),
        "{stored}"
    );
    assert_eq!(git(&f.dir, &["log", "-1", "--format=%s", "gitbots/logs"]), format!("log: {path}"));

    let binary = vec![0xFF, 0xFE, b's', b'k', b'-', 0x00];
    logs.put("bin.log", &binary)?;
    assert_eq!(logs.read("bin.log")?, Some(binary), "binary is stored verbatim");

    let before = logs.tip()?.expect("tip");
    let refs =
        logs.put_many(vec![("a.log".into(), b"a".to_vec()), ("b.log".into(), b"b".to_vec())])?;
    assert_eq!(refs.len(), 2);
    let tip = logs.tip()?.expect("tip");
    assert_eq!(git(&f.dir, &["rev-parse", &format!("{tip}^")]), before, "one commit for the batch");
    assert_eq!(git(&f.dir, &["log", "-1", "--format=%s", "gitbots/logs"]), "2 logs");
    assert!(logs.put_many(vec![])?.is_empty());

    // Logs and activity are separate branches.
    assert_eq!(Ledger::new(&f.repo, ACTIVITY_BRANCH, LedgerKind::Activity).tip()?, None);
    Ok(())
}

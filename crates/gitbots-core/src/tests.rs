//! End-to-end fold: one attempt through its whole lifecycle.

use time::OffsetDateTime;

use crate::action::{ActionRun, RunStatus};
use crate::board::{AttemptState, Board, TaskStatus};
use crate::event::*;
use crate::id::*;
use crate::identity::{Actor, AgentDescriptor, Session};
use crate::stats::Stats;

struct Clock(u64);

impl Clock {
    fn event(&mut self, actor: &Actor, body: EventBody) -> Event {
        self.0 += 1;
        let id =
            EventId::from_ulid(Ulid::from_parts(1_790_000_000_000 + self.0, u128::from(self.0)));
        Event::new(id, OffsetDateTime::UNIX_EPOCH, actor.clone(), body)
    }
}

fn session(n: u64, model: &str, parent: Option<SessionId>) -> Session {
    Session {
        id: SessionId::from_ulid(Ulid::from_parts(1_790_000_000_000, u128::from(n))),
        agent: AgentDescriptor::new("anthropic", model, "claude-code"),
        parent,
        role: None,
        operator: Some("gstohl".into()),
        external_id: None,
        label: None,
        started_at: OffsetDateTime::UNIX_EPOCH,
    }
}

#[test]
fn attempt_lifecycle_folds_into_board_and_stats() {
    let mut clock = Clock(0);
    let human = Actor::human("gstohl", None);
    let worker = session(1, "claude-opus-5-5", None);
    let sub = session(2, "claude-haiku-4-5", Some(worker.id.clone()));
    let reviewer = session(3, "gpt-5-codex", None);
    let task = TaskId::from_ulid(Ulid::from_parts(1, 1));
    let attempt = AttemptId::from_ulid(Ulid::from_parts(1, 2));
    let run = RunId::from_ulid(Ulid::from_parts(1, 3));

    let events = vec![
        clock.event(
            &worker.actor(),
            EventBody::SessionStarted(SessionStarted { session: worker.clone() }),
        ),
        clock.event(
            &sub.actor(),
            EventBody::SessionStarted(SessionStarted { session: sub.clone() }),
        ),
        clock.event(
            &reviewer.actor(),
            EventBody::SessionStarted(SessionStarted { session: reviewer.clone() }),
        ),
        clock.event(
            &human,
            EventBody::TaskCreated(TaskCreated {
                task: task.clone(),
                title: "Add login".into(),
                body: None,
                recipe: None,
                labels: vec![],
            }),
        ),
        clock.event(
            &worker.actor(),
            EventBody::AttemptStarted(AttemptStarted {
                task: task.clone(),
                attempt: attempt.clone(),
                branch: "gitbots/attempt/add-login-abc123".into(),
                base: "main".into(),
                base_commit: "0".repeat(40),
                session: Some(worker.id.clone()),
            }),
        ),
        clock.event(
            &sub.actor(),
            EventBody::CommitRecorded(CommitRecorded {
                sha: "a".repeat(40),
                subject: "login form".into(),
                branch: None,
                attempt: Some(attempt.clone()),
                diff: Some(DiffStat { files: 2, insertions: 40, deletions: 3 }),
            }),
        ),
        clock.event(
            &worker.actor(),
            EventBody::AttemptSubmitted(AttemptSubmitted {
                attempt: attempt.clone(),
                head: "a".repeat(40),
                summary: Some("login works".into()),
                diff: None,
                mandate: None,
            }),
        ),
        clock.event(
            &Actor::system("actions"),
            EventBody::ActionCompleted(ActionRun {
                run,
                workflow: "ci".into(),
                trigger: kind::ATTEMPT_SUBMITTED.into(),
                attempt: Some(attempt.clone()),
                commit: Some("a".repeat(40)),
                runner: "local".into(),
                status: RunStatus::Success,
                duration_ms: 10,
                jobs: vec![],
            }),
        ),
        clock.event(
            &reviewer.actor(),
            EventBody::ReviewDecided(ReviewDecided {
                attempt: attempt.clone(),
                decision: ReviewDecision::Accept,
                reason: None,
                mandate: None,
            }),
        ),
    ];

    // Fold order comes from ids, not input order.
    let mut shuffled = events.clone();
    shuffled.reverse();
    let board = Board::from_events(&shuffled);

    let a = board.find_attempt(&attempt.short()).unwrap();
    assert_eq!(a.state, AttemptState::Accepted);
    assert_eq!(a.checks_passed(), Some(true));
    assert_eq!(board.task_status(board.find_task(task.as_str()).unwrap()), TaskStatus::Accepted);
    assert_eq!(board.orphans, 0);

    let family = board.session_family(&sub.id);
    assert!(family.contains(&worker.id) && family.contains(&sub.id));
    assert!(!family.contains(&reviewer.id));

    let stats = Stats::from_events(&events);
    let opus = &stats.by_actor["anthropic/claude-opus-5-5@claude-code"];
    assert_eq!((opus.attempts_started, opus.attempts_submitted, opus.accepted), (1, 1, 1));
    assert_eq!((opus.runs, opus.runs_passed), (1, 1));
    assert_eq!(opus.acceptance_rate(), Some(1.0));
    let haiku = &stats.by_actor["anthropic/claude-haiku-4-5@claude-code"];
    assert_eq!((haiku.commits, haiku.lines_added, haiku.subagent_sessions), (1, 40, 1));
    assert_eq!(stats.by_actor["@gstohl"].tasks_created, 1);
}

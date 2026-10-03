//! Response bodies of the hosted dashboard API (`/api`, `docs/API.md`).
//!
//! Folded from indexed events with `gitbots-core` and shaped exactly like
//! `gitbots ui` (`crates/gitbots/src/ui.rs`), so the SolidJS app works against
//! either server. Pure: the Worker loads events and git objects and passes
//! them in.

use gitbots_core::event::ReportLevel;
use gitbots_core::manifest::{Manifest, PathViolation, Role};
use gitbots_core::{
    Actor, AttemptView, Board, Event, EventBody, LookupError, Session, SessionId, Stats, TaskView,
};
use serde_json::{Value, json};

use crate::commits;
use crate::source::Commit;

pub type Json = Result<Value, serde_json::Error>;

pub fn task_json(board: &Board, t: &TaskView) -> Json {
    let mut v = serde_json::to_value(t)?;
    v["status"] = serde_json::to_value(board.task_status(t))?;
    Ok(v)
}

pub fn attempt_json(a: &AttemptView) -> Json {
    let mut v = serde_json::to_value(a)?;
    v["checks_passed"] = json!(a.checks_passed());
    Ok(v)
}

/// `GET /api/board`.
pub fn board_json(board: &Board) -> Json {
    let tasks = board.tasks.values().map(|t| task_json(board, t)).collect::<Result<Vec<_>, _>>()?;
    let attempts = board.attempts.values().map(attempt_json).collect::<Result<Vec<_>, _>>()?;
    let awaiting: Vec<_> = board.awaiting_review().map(|a| &a.id).collect();
    Ok(json!({
        "tasks": tasks,
        "attempts": attempts,
        "sessions": board.sessions.values().collect::<Vec<_>>(),
        "awaiting_review": awaiting,
        "orphans": board.orphans,
    }))
}

/// `GET /api/inbox`: submitted attempts, and up to 20 reports, blockers first.
pub fn inbox_json(board: &Board) -> Json {
    let mut reports = board.reports.clone();
    reports.sort_by(|a, b| rank(b.report.level).cmp(&rank(a.report.level)).then(b.id.cmp(&a.id)));
    reports.truncate(20);
    let awaiting: Vec<&AttemptView> = board.awaiting_review().collect();
    Ok(json!({ "awaiting_review": awaiting, "reports": reports }))
}

fn rank(level: ReportLevel) -> u8 {
    match level {
        ReportLevel::Info => 0,
        ReportLevel::Warning => 1,
        ReportLevel::Blocker => 2,
    }
}

/// `GET /api/stats`.
pub fn stats_json(events: &[Event]) -> Json {
    serde_json::to_value(Stats::from_events(events))
}

/// Query of `GET /api/events`.
#[derive(Clone, Debug, Default)]
pub struct EventFilter {
    pub kind: Option<String>,
    pub session: Option<String>,
    pub task: Option<String>,
    pub attempt: Option<String>,
    pub limit: Option<usize>,
}

/// Events matching `filter`, oldest first, the newest `limit` of them.
/// `events` must be sorted by id.
pub fn query_events<'a>(
    events: &'a [Event],
    board: &Board,
    filter: &EventFilter,
) -> Result<Vec<&'a Event>, LookupError> {
    let some = |s: &Option<String>| s.clone().filter(|s| !s.is_empty());
    let session = some(&filter.session)
        .map(|q| board.find_session(&q).map(|s| s.session.id.clone()))
        .transpose()?;
    let task = some(&filter.task).map(|q| board.find_task(&q).map(|t| t.id.clone())).transpose()?;
    let attempt =
        some(&filter.attempt).map(|q| board.find_attempt(&q).map(|a| a.id.clone())).transpose()?;
    let kind = some(&filter.kind);
    let mut out: Vec<&Event> = events
        .iter()
        .filter(|e| {
            kind.as_deref().is_none_or(|k| e.kind() == k || e.kind().starts_with(&format!("{k}.")))
        })
        .filter(|e| session.as_ref().is_none_or(|s| e.actor.session() == Some(s)))
        .filter(|e| {
            task.as_ref().is_none_or(|t| {
                e.body.task() == Some(t)
                    || e.body
                        .attempt()
                        .and_then(|a| board.attempts.get(a))
                        .is_some_and(|a| &a.task == t)
            })
        })
        .filter(|e| attempt.as_ref().is_none_or(|a| e.body.attempt() == Some(a)))
        .collect();
    if let Some(limit) = filter.limit {
        let skip = out.len().saturating_sub(limit);
        out.drain(..skip);
    }
    Ok(out)
}

/// The attempt's bound session, then its ancestors.
pub fn session_chain(board: &Board, start: Option<&SessionId>) -> Vec<Session> {
    let mut chain: Vec<Session> = Vec::new();
    let mut cursor = start.cloned();
    while let Some(id) = cursor {
        let Some(view) = board.sessions.get(&id) else { break };
        if chain.iter().any(|s| s.id == id) {
            break;
        }
        cursor = view.session.parent.clone();
        chain.push(view.session.clone());
    }
    chain
}

/// `CommitInfo` of `docs/API.md`.
pub fn commit_json(c: &Commit) -> Value {
    let trailers: Vec<[String; 2]> =
        commits::trailers(&c.message).into_iter().map(|(k, v)| [k, v]).collect();
    json!({ "sha": c.sha, "subject": commits::subject(&c.message), "trailers": trailers })
}

/// `GET /api/attempts/{id}`. Hosted, there is no local workroom.
pub fn attempt_detail_json(
    board: &Board,
    attempt: &AttemptView,
    events: &[Event],
    commits: &[Commit],
    violations: &[PathViolation],
) -> Json {
    let task = board.tasks.get(&attempt.task).map(|t| task_json(board, t)).transpose()?;
    let filter = EventFilter { attempt: Some(attempt.id.to_string()), ..Default::default() };
    let events = query_events(events, board, &filter).unwrap_or_default();
    let runs: Vec<&gitbots_core::ActionRun> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::ActionCompleted(run) => Some(run),
            _ => None,
        })
        .collect();
    let violations: Vec<Value> =
        violations.iter().map(|v| json!({"path": v.path, "reason": v.reason})).collect();
    Ok(json!({
        "attempt": attempt_json(attempt)?,
        "task": task,
        "workroom": null,
        "violations": violations,
        "commits": commits.iter().map(commit_json).collect::<Vec<_>>(),
        "runs": runs,
        "events": events,
        "session_chain": session_chain(board, attempt.session.as_ref()),
    }))
}

/// Fills the serde defaults of an `gitbots_actions::Workflow` into a workflow
/// TOML converted to JSON, so the hosted `/api/workflows` has the shape
/// `gitbots ui` serializes. (`gitbots-actions` does not build for wasm32.)
pub fn fill_workflow_defaults(workflow: &mut Value) {
    fn default(map: &mut serde_json::Map<String, Value>, key: &str, value: Value) {
        map.entry(key.to_owned()).or_insert(value);
    }
    let Some(top) = workflow.as_object_mut() else { return };
    default(top, "env", json!({}));
    let Some(jobs) = top.get_mut("jobs").and_then(Value::as_object_mut) else { return };
    for job in jobs.values_mut().filter_map(Value::as_object_mut) {
        default(job, "runs-on", json!("local"));
        default(job, "needs", json!([]));
        default(job, "env", json!({}));
        default(job, "timeout-secs", Value::Null);
        let steps = job.get_mut("steps").and_then(Value::as_array_mut);
        for step in steps.into_iter().flatten().filter_map(Value::as_object_mut) {
            default(step, "name", Value::Null);
            default(step, "env", json!({}));
            default(step, "workdir", Value::Null);
            default(step, "continue-on-error", json!(false));
        }
    }
}

/// The human the hosted dashboard acts as: the manifest's first owner
/// principal, else the tenancy owner. Outbox items carry this actor.
pub fn owner_actor(manifest: &Manifest) -> Actor {
    match manifest.mandate.principals.iter().find(|p| p.role == Role::Owner) {
        Some(p) => Actor::human(p.handle.clone(), p.email.clone()),
        None => Actor::human(manifest.tenancy.owner.handle.clone(), None),
    }
}

/// Where the hosted dashboard read the manifest from.
#[derive(Clone, Debug)]
pub struct ManifestAt {
    pub manifest: Manifest,
    pub branch: String,
    /// Blob id of `.gitbots/manifest.json`.
    pub oid: String,
}

/// `GET /api/project`, plus the hosted fields from `docs/CLOUD.md`.
pub fn project_json(at: &ManifestAt, producer: &str, pending_outbox: u64) -> Json {
    Ok(json!({
        "manifest": serde_json::to_value(&at.manifest)?,
        "manifest_source": {"type": "trusted", "branch": at.branch, "oid": at.oid},
        "trusted_branch": at.branch,
        "viewer": owner_actor(&at.manifest),
        "can_decide": true,
        "producer": producer,
        "hosted": true,
        "pending_outbox": pending_outbox,
    }))
}

#[cfg(test)]
mod tests {
    use gitbots_core::event::*;
    use gitbots_core::identity::AgentDescriptor;
    use gitbots_core::{AttemptId, EventId, TaskId, Ulid};
    use time::OffsetDateTime;

    use super::*;

    fn ev(n: u64, actor: &Actor, body: EventBody) -> Event {
        let id = EventId::from_ulid(Ulid::from_parts(1_790_000_000_000 + n, u128::from(n)));
        Event::new(id, OffsetDateTime::UNIX_EPOCH, actor.clone(), body)
    }

    fn fixture() -> (Vec<Event>, AttemptId) {
        let parent = Session {
            id: SessionId::from_ulid(Ulid::from_parts(1, 1)),
            agent: AgentDescriptor::new("anthropic", "claude-opus-5-5", "claude-code"),
            parent: None,
            role: None,
            operator: None,
            external_id: None,
            label: None,
            started_at: OffsetDateTime::UNIX_EPOCH,
        };
        let child = Session {
            id: SessionId::from_ulid(Ulid::from_parts(1, 2)),
            parent: Some(parent.id.clone()),
            ..parent.clone()
        };
        let task = TaskId::from_ulid(Ulid::from_parts(2, 1));
        let att = AttemptId::from_ulid(Ulid::from_parts(2, 2));
        let human = Actor::human("gstohl", None);
        let events = vec![
            ev(
                1,
                &parent.actor(),
                EventBody::SessionStarted(SessionStarted { session: parent.clone() }),
            ),
            ev(
                2,
                &child.actor(),
                EventBody::SessionStarted(SessionStarted { session: child.clone() }),
            ),
            ev(
                3,
                &human,
                EventBody::TaskCreated(TaskCreated {
                    task: task.clone(),
                    title: "t".into(),
                    body: None,
                    recipe: None,
                    labels: vec![],
                }),
            ),
            ev(
                4,
                &child.actor(),
                EventBody::AttemptStarted(AttemptStarted {
                    task: task.clone(),
                    attempt: att.clone(),
                    branch: "gitbots/att-x".into(),
                    base: "main".into(),
                    base_commit: "b".into(),
                    session: Some(child.id.clone()),
                }),
            ),
            ev(
                5,
                &child.actor(),
                EventBody::AttemptSubmitted(AttemptSubmitted {
                    attempt: att.clone(),
                    head: "h".into(),
                    summary: None,
                    diff: None,
                    mandate: None,
                }),
            ),
            ev(
                6,
                &child.actor(),
                EventBody::Report(Report {
                    title: "fyi".into(),
                    body: None,
                    level: ReportLevel::Info,
                    task: None,
                    attempt: None,
                }),
            ),
            ev(
                7,
                &child.actor(),
                EventBody::Report(Report {
                    title: "stuck".into(),
                    body: None,
                    level: ReportLevel::Blocker,
                    task: None,
                    attempt: None,
                }),
            ),
        ];
        (events, att)
    }

    #[test]
    fn workflow_defaults() {
        let mut wf = json!({"name": "ci", "on": ["manual"], "jobs": {"t": {"steps": [{"run": "x"}], "needs": ["a"]}}});
        fill_workflow_defaults(&mut wf);
        assert_eq!(
            wf,
            json!({"name": "ci", "on": ["manual"], "env": {}, "jobs": {"t": {
                "runs-on": "local", "needs": ["a"], "env": {}, "timeout-secs": null,
                "steps": [{"run": "x", "name": null, "env": {}, "workdir": null, "continue-on-error": false}]
            }}})
        );
    }

    #[test]
    fn board_and_inbox_shapes() {
        let (events, att) = fixture();
        let board = Board::from_events(&events);
        let v = board_json(&board).unwrap();
        assert_eq!(v["tasks"][0]["status"], "in_progress");
        assert_eq!(v["attempts"][0]["checks_passed"], Value::Null);
        assert_eq!(v["awaiting_review"][0], att.as_str());
        assert_eq!(v["sessions"].as_array().unwrap().len(), 2);
        let inbox = inbox_json(&board).unwrap();
        assert_eq!(inbox["reports"][0]["report"]["title"], "stuck");
        assert_eq!(inbox["awaiting_review"][0]["state"], "submitted");
        assert!(inbox["awaiting_review"][0].get("checks_passed").is_none());
    }

    #[test]
    fn events_query_and_detail() {
        let (events, att) = fixture();
        let board = Board::from_events(&events);
        let by_task = query_events(
            &events,
            &board,
            &EventFilter {
                task: Some(board.tasks.keys().next().unwrap().to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(by_task.len(), 3);
        let kind =
            EventFilter { kind: Some("attempt".into()), limit: Some(1), ..Default::default() };
        let last = query_events(&events, &board, &kind).unwrap();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].kind(), "attempt.submitted");
        assert!(
            query_events(
                &events,
                &board,
                &EventFilter { attempt: Some("zzzzzz".into()), ..Default::default() }
            )
            .is_err()
        );

        let a = board.find_attempt(&att.short()).unwrap();
        let commit = Commit {
            sha: "h".into(),
            tree: "t".into(),
            parents: vec!["b".into()],
            message: "Do it\n\nGitbots-Attempt: x".into(),
        };
        let v = attempt_detail_json(&board, a, &events, &[commit], &[]).unwrap();
        assert_eq!(v["task"]["status"], "in_progress");
        assert_eq!(v["workroom"], Value::Null);
        assert_eq!(v["commits"][0]["subject"], "Do it");
        assert_eq!(v["commits"][0]["trailers"][0], json!(["Gitbots-Attempt", "x"]));
        assert_eq!(v["events"].as_array().unwrap().len(), 2);
        let chain = v["session_chain"].as_array().unwrap();
        assert_eq!(chain.len(), 2);
        assert!(chain[1].get("parent").is_none());
    }
}

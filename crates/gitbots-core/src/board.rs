//! The Workrooms board: current state of tasks and attempts, folded from
//! ledger events. Pure, so CLI, MCP and a hosted UI agree by construction.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use time::OffsetDateTime;

use crate::action::RunStatus;
use crate::event::{DiffStat, Event, EventBody, HandoffTarget, Report, ReviewDecision};
use crate::id::{AttemptId, EventId, RunId, SessionId, TaskId};
use crate::identity::{Actor, Session};

#[derive(Clone, Debug, Default, Serialize)]
pub struct Board {
    pub tasks: BTreeMap<TaskId, TaskView>,
    pub attempts: BTreeMap<AttemptId, AttemptView>,
    pub sessions: BTreeMap<SessionId, SessionView>,
    pub reports: Vec<ReportView>,
    /// Events that referenced a task/attempt the board hasn't seen.
    pub orphans: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionView {
    pub session: Session,
    pub ended: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskView {
    pub id: TaskId,
    pub title: String,
    pub body: Option<String>,
    pub recipe: Option<String>,
    pub labels: Vec<String>,
    pub created_by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub attempts: Vec<AttemptId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    InProgress,
    Accepted,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Active,
    Submitted,
    ChangesRequested,
    Accepted,
    Rejected,
    Merged,
    Abandoned,
}

impl AttemptState {
    pub fn is_open(self) -> bool {
        matches!(self, Self::Active | Self::Submitted | Self::ChangesRequested)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Submitted => "submitted",
            Self::ChangesRequested => "changes_requested",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Merged => "merged",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RunSummary {
    pub run: RunId,
    pub workflow: String,
    pub status: RunStatus,
    pub commit: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReviewView {
    pub decision: ReviewDecision,
    pub reason: Option<String>,
    pub by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

#[derive(Clone, Debug, Serialize)]
pub struct AttemptView {
    pub id: AttemptId,
    pub task: TaskId,
    pub branch: String,
    pub base: String,
    pub base_commit: String,
    pub session: Option<SessionId>,
    pub started_by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    pub state: AttemptState,
    pub head: Option<String>,
    pub summary: Option<String>,
    pub diff: Option<DiffStat>,
    pub submitted_by: Option<Actor>,
    /// When the latest submission happened (`updated_at` also moves on runs,
    /// handoffs and reviews).
    #[serde(with = "time::serde::rfc3339::option")]
    pub submitted_at: Option<OffsetDateTime>,
    /// Who holds the attempt after the latest handoff.
    pub holder: Option<HandoffTarget>,
    pub runs: Vec<RunSummary>,
    pub review: Option<ReviewView>,
    pub merged_commit: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl AttemptView {
    /// Latest run per workflow passed. `None` when no checks ran.
    pub fn checks_passed(&self) -> Option<bool> {
        let mut latest: BTreeMap<&str, RunStatus> = BTreeMap::new();
        for r in &self.runs {
            latest.insert(&r.workflow, r.status);
        }
        (!latest.is_empty()).then(|| latest.values().all(|s| s.is_success()))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ReportView {
    pub id: EventId,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub by: Actor,
    pub report: Report,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LookupError {
    #[error("no {what} matches `{query}`")]
    NotFound { what: &'static str, query: String },
    #[error("`{query}` is ambiguous: {}", candidates.join(", "))]
    Ambiguous { query: String, candidates: Vec<String> },
}

impl Board {
    /// Fold events in id (time) order.
    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a Event>) -> Self {
        let mut sorted: Vec<&Event> = events.into_iter().collect();
        sorted.sort_by(|a, b| a.id.cmp(&b.id));
        let mut board = Self::default();
        for e in sorted {
            board.apply(e);
        }
        board
    }

    pub fn apply(&mut self, e: &Event) {
        match &e.body {
            EventBody::SessionStarted(s) => {
                self.sessions.insert(
                    s.session.id.clone(),
                    SessionView { session: s.session.clone(), ended: false },
                );
            }
            EventBody::SessionEnded(s) => {
                if let Some(v) = self.sessions.get_mut(&s.session) {
                    v.ended = true;
                }
            }
            EventBody::TaskCreated(t) => {
                self.tasks.insert(
                    t.task.clone(),
                    TaskView {
                        id: t.task.clone(),
                        title: t.title.clone(),
                        body: t.body.clone(),
                        recipe: t.recipe.clone(),
                        labels: t.labels.clone(),
                        created_by: e.actor.clone(),
                        created_at: e.ts,
                        attempts: Vec::new(),
                    },
                );
            }
            EventBody::AttemptStarted(a) => {
                let Some(task) = self.tasks.get_mut(&a.task) else {
                    self.orphans += 1;
                    return;
                };
                task.attempts.push(a.attempt.clone());
                self.attempts.insert(
                    a.attempt.clone(),
                    AttemptView {
                        id: a.attempt.clone(),
                        task: a.task.clone(),
                        branch: a.branch.clone(),
                        base: a.base.clone(),
                        base_commit: a.base_commit.clone(),
                        session: a.session.clone(),
                        started_by: e.actor.clone(),
                        started_at: e.ts,
                        state: AttemptState::Active,
                        head: None,
                        summary: None,
                        diff: None,
                        submitted_by: None,
                        submitted_at: None,
                        holder: None,
                        runs: Vec::new(),
                        review: None,
                        merged_commit: None,
                        updated_at: e.ts,
                    },
                );
            }
            EventBody::Report(r) => self.reports.push(ReportView {
                id: e.id.clone(),
                at: e.ts,
                by: e.actor.clone(),
                report: r.clone(),
            }),
            body => {
                let Some(id) = body.attempt() else { return };
                let Some(a) = self.attempts.get_mut(id) else {
                    self.orphans += 1;
                    return;
                };
                a.updated_at = e.ts;
                match body {
                    EventBody::AttemptSubmitted(s) => {
                        a.state = AttemptState::Submitted;
                        a.head = Some(s.head.clone());
                        a.summary = s.summary.clone().or(a.summary.take());
                        a.diff = s.diff;
                        a.submitted_by = Some(e.actor.clone());
                        a.submitted_at = Some(e.ts);
                        a.review = None;
                    }
                    EventBody::AttemptHandoff(h) => a.holder = Some(h.to.clone()),
                    EventBody::AttemptAbandoned(_) => a.state = AttemptState::Abandoned,
                    EventBody::AttemptMerged(m) => {
                        a.state = AttemptState::Merged;
                        a.merged_commit = Some(m.commit.clone());
                    }
                    EventBody::ReviewDecided(r) => {
                        a.state = match r.decision {
                            ReviewDecision::Accept => AttemptState::Accepted,
                            ReviewDecision::Reject => AttemptState::Rejected,
                            ReviewDecision::ChangesRequested => AttemptState::ChangesRequested,
                        };
                        a.review = Some(ReviewView {
                            decision: r.decision,
                            reason: r.reason.clone(),
                            by: e.actor.clone(),
                            at: e.ts,
                        });
                    }
                    EventBody::ActionCompleted(run) => a.runs.push(RunSummary {
                        run: run.run.clone(),
                        workflow: run.workflow.clone(),
                        status: run.status,
                        commit: run.commit.clone(),
                    }),
                    EventBody::CommitRecorded(c) => a.head = Some(c.sha.clone()),
                    _ => {}
                }
            }
        }
    }

    pub fn task_status(&self, task: &TaskView) -> TaskStatus {
        let states: Vec<AttemptState> =
            task.attempts.iter().filter_map(|id| self.attempts.get(id)).map(|a| a.state).collect();
        if states.contains(&AttemptState::Merged) {
            TaskStatus::Done
        } else if states.contains(&AttemptState::Accepted) {
            TaskStatus::Accepted
        } else if states.iter().any(|s| s.is_open()) {
            TaskStatus::InProgress
        } else {
            TaskStatus::Open
        }
    }

    /// Submitted attempts waiting for a review decision.
    pub fn awaiting_review(&self) -> impl Iterator<Item = &AttemptView> {
        self.attempts.values().filter(|a| a.state == AttemptState::Submitted)
    }

    pub fn find_task(&self, query: &str) -> Result<&TaskView, LookupError> {
        find(&self.tasks, "task", query, |id| id.matches(query), |id| id.to_string())
    }

    pub fn find_attempt(&self, query: &str) -> Result<&AttemptView, LookupError> {
        // Branch names are also accepted.
        if let Some(a) = self.attempts.values().find(|a| a.branch == query) {
            return Ok(a);
        }
        find(&self.attempts, "attempt", query, |id| id.matches(query), |id| id.to_string())
    }

    pub fn find_session(&self, query: &str) -> Result<&SessionView, LookupError> {
        find(&self.sessions, "session", query, |id| id.matches(query), |id| id.to_string())
    }

    /// The whole session tree `session` belongs to: its root ancestor and
    /// every descendant (so siblings too). A parent could otherwise spawn a
    /// subagent to rubber-stamp its own work, so no member of the family may
    /// approve work produced inside it.
    pub fn session_family(&self, session: &SessionId) -> BTreeSet<SessionId> {
        let mut family = BTreeSet::from([session.clone()]);
        let mut cursor = self.sessions.get(session).and_then(|s| s.session.parent.clone());
        while let Some(parent) = cursor {
            if !family.insert(parent.clone()) {
                break;
            }
            cursor = self.sessions.get(&parent).and_then(|s| s.session.parent.clone());
        }
        loop {
            let before = family.len();
            for s in self.sessions.values() {
                if s.session.parent.as_ref().is_some_and(|p| family.contains(p)) {
                    family.insert(s.session.id.clone());
                }
            }
            if family.len() == before {
                return family;
            }
        }
    }
}

fn find<'a, K: Ord, V>(
    map: &'a BTreeMap<K, V>,
    what: &'static str,
    query: &str,
    matches: impl Fn(&K) -> bool,
    show: impl Fn(&K) -> String,
) -> Result<&'a V, LookupError> {
    let hits: Vec<(&K, &V)> = map.iter().filter(|(k, _)| matches(k)).collect();
    match hits.as_slice() {
        [(_, v)] => Ok(v),
        [] => Err(LookupError::NotFound { what, query: query.to_owned() }),
        many => Err(LookupError::Ambiguous {
            query: query.to_owned(),
            candidates: many.iter().map(|(k, _)| show(k)).collect(),
        }),
    }
}

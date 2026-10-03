//! The ledger event envelope and every event kind gitbots understands.
//!
//! On disk an event is
//! `{"v":1,"id":"evt_..","ts":"..","actor":{..},"kind":"task.created","data":{..}}`.
//!
//! Compatibility rules (the ledger outlives any one binary):
//! - readers ignore unknown fields;
//! - kinds this build doesn't know, or can't parse, become [`EventBody::Unknown`];
//! - existing kinds only ever gain optional fields; a breaking change gets a
//!   new kind name;
//! - `v` versions the envelope, not the payloads.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use time::OffsetDateTime;

use crate::action::{ActionRun, LogRef};
use crate::id::{AttemptId, EventId, ProjectId, SessionId, TaskId};
use crate::identity::{Actor, Session, Via};

/// Envelope schema version written by this build.
pub const EVENT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub v: u32,
    pub id: EventId,
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub actor: Actor,
    /// How `actor` was resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    /// Writer, e.g. `gitbots/0.1.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    /// Idempotency key (`commit:<oid>`, `mcp:<ses>:<rpc>`); writers skip an
    /// event whose key is already in the ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idem: Option<String>,
    /// The event this one acts on (e.g. the submission a review decides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<EventId>,
    #[serde(flatten)]
    pub body: EventBody,
}

impl Event {
    pub fn new(id: EventId, ts: OffsetDateTime, actor: Actor, body: EventBody) -> Self {
        Self {
            v: EVENT_VERSION,
            id,
            ts,
            actor,
            via: None,
            producer: None,
            idem: None,
            on: None,
            body,
        }
    }

    pub fn with_via(mut self, via: Via) -> Self {
        self.via = Some(via);
        self
    }

    pub fn with_idem(mut self, key: impl Into<String>) -> Self {
        self.idem = Some(key.into());
        self
    }

    pub fn kind(&self) -> &str {
        self.body.kind()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStat {
    pub files: u32,
    pub insertions: u32,
    pub deletions: u32,
}

impl std::ops::AddAssign for DiffStat {
    fn add_assign(&mut self, rhs: Self) {
        self.files += rhs.files;
        self.insertions += rhs.insertions;
        self.deletions += rhs.deletions;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInitialized {
    pub project: ProjectId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStarted {
    pub session: Session,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnded {
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCreated {
    pub task: TaskId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Recipe (`name@version`) the task was rendered from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptStarted {
    pub task: TaskId,
    pub attempt: AttemptId,
    pub branch: String,
    /// Branch the attempt targets.
    pub base: String,
    pub base_commit: String,
    /// Session bound to the attempt's workroom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptSubmitted {
    pub attempt: AttemptId,
    pub head: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffStat>,
    /// Blob id of the manifest the submission was checked against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mandate: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HandoffTarget {
    Session {
        session: SessionId,
    },
    /// Whoever picks up next with this role.
    Role {
        role: String,
    },
    Human {
        handle: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptHandoff {
    pub attempt: AttemptId,
    pub to: HandoffTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptAbandoned {
    pub attempt: AttemptId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptMerged {
    pub attempt: AttemptId,
    pub into: String,
    /// Commit on `into` that contains the attempt.
    pub commit: String,
    /// Attempt commits, so attribution survives squash merges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_commits: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Accept,
    Reject,
    ChangesRequested,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewDecided {
    pub attempt: AttemptId,
    pub decision: ReviewDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Blob id of the manifest that authorized the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mandate: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRecorded {
    pub sha: String,
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<AttemptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffStat>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportLevel {
    #[default]
    Info,
    Warning,
    /// The agent is stuck until a human acts.
    Blocker,
}

/// An agent telling its human something.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default)]
    pub level: ReportLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<AttemptId>,
}

/// One traced agent tool call (shell command, edit, MCP call, ...).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCalled {
    pub tool: String,
    /// Short, human-readable input summary. Full payloads go to `log`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<LogRef>,
}

macro_rules! event_kinds {
    ($($variant:ident($payload:ty) => $konst:ident = $kind:literal),* $(,)?) => {
        /// Kind strings, usable as actions triggers.
        pub mod kind {
            $(pub const $konst: &str = $kind;)*
            /// Every kind this build understands.
            pub const ALL: &[&str] = &[$($kind),*];
        }

        #[derive(Clone, Debug, PartialEq)]
        pub enum EventBody {
            $($variant($payload),)*
            /// A kind this build doesn't understand, kept verbatim.
            Unknown { kind: String, data: Value },
        }

        impl EventBody {
            pub fn kind(&self) -> &str {
                match self {
                    $(Self::$variant(_) => $kind,)*
                    Self::Unknown { kind, .. } => kind,
                }
            }
        }

        impl Serialize for EventBody {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                #[derive(Serialize)]
                struct Raw<'a, T: Serialize> {
                    kind: &'a str,
                    data: &'a T,
                }
                match self {
                    $(Self::$variant(data) => Raw { kind: $kind, data }.serialize(s),)*
                    Self::Unknown { kind, data } => Raw { kind, data }.serialize(s),
                }
            }
        }

        impl<'de> Deserialize<'de> for EventBody {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                #[derive(Deserialize)]
                struct Raw {
                    kind: String,
                    #[serde(default)]
                    data: Value,
                }
                let Raw { kind, data } = Raw::deserialize(d)?;
                let typed = match kind.as_str() {
                    $($kind => serde_json::from_value(data.clone()).ok().map(Self::$variant),)*
                    _ => None,
                };
                Ok(typed.unwrap_or(Self::Unknown { kind, data }))
            }
        }
    };
}

event_kinds! {
    ProjectInitialized(ProjectInitialized) => PROJECT_INITIALIZED = "project.initialized",
    SessionStarted(SessionStarted) => SESSION_STARTED = "session.started",
    SessionEnded(SessionEnded) => SESSION_ENDED = "session.ended",
    TaskCreated(TaskCreated) => TASK_CREATED = "task.created",
    AttemptStarted(AttemptStarted) => ATTEMPT_STARTED = "attempt.started",
    AttemptSubmitted(AttemptSubmitted) => ATTEMPT_SUBMITTED = "attempt.submitted",
    AttemptHandoff(AttemptHandoff) => ATTEMPT_HANDOFF = "attempt.handoff",
    AttemptAbandoned(AttemptAbandoned) => ATTEMPT_ABANDONED = "attempt.abandoned",
    AttemptMerged(AttemptMerged) => ATTEMPT_MERGED = "attempt.merged",
    ReviewDecided(ReviewDecided) => REVIEW_DECIDED = "review.decided",
    CommitRecorded(CommitRecorded) => COMMIT_RECORDED = "commit.recorded",
    ActionCompleted(ActionRun) => ACTION_COMPLETED = "action.completed",
    Report(Report) => REPORT = "report",
    ToolCalled(ToolCalled) => TOOL_CALLED = "tool.called",
}

impl EventBody {
    /// Task this event is about, if it names one directly.
    pub fn task(&self) -> Option<&TaskId> {
        match self {
            Self::TaskCreated(e) => Some(&e.task),
            Self::AttemptStarted(e) => Some(&e.task),
            Self::Report(e) => e.task.as_ref(),
            _ => None,
        }
    }

    /// Attempt this event is about, if it names one directly.
    pub fn attempt(&self) -> Option<&AttemptId> {
        match self {
            Self::AttemptStarted(e) => Some(&e.attempt),
            Self::AttemptSubmitted(e) => Some(&e.attempt),
            Self::AttemptHandoff(e) => Some(&e.attempt),
            Self::AttemptAbandoned(e) => Some(&e.attempt),
            Self::AttemptMerged(e) => Some(&e.attempt),
            Self::ReviewDecided(e) => Some(&e.attempt),
            Self::CommitRecorded(e) => e.attempt.as_ref(),
            Self::ActionCompleted(e) => e.attempt.as_ref(),
            Self::Report(e) => e.attempt.as_ref(),
            _ => None,
        }
    }

    /// One-line human summary, used for ledger commit messages and listings.
    pub fn summary(&self) -> String {
        match self {
            Self::ProjectInitialized(e) => format!("initialized {}", e.name),
            Self::SessionStarted(e) => format!("{} started", e.session.agent.key()),
            Self::SessionEnded(e) => format!("{} ended", e.session.short()),
            Self::TaskCreated(e) => e.title.clone(),
            Self::AttemptStarted(e) => format!("{} on {}", e.attempt.short(), e.branch),
            Self::AttemptSubmitted(e) => match &e.summary {
                Some(s) => format!("{}: {s}", e.attempt.short()),
                None => format!("{} at {}", e.attempt.short(), short_sha(&e.head)),
            },
            Self::AttemptHandoff(e) => format!("{} -> {}", e.attempt.short(), handoff_label(&e.to)),
            Self::AttemptAbandoned(e) => e.attempt.short(),
            Self::AttemptMerged(e) => format!("{} into {}", e.attempt.short(), e.into),
            Self::ReviewDecided(e) => {
                format!("{:?} {}", e.decision, e.attempt.short()).to_lowercase()
            }
            Self::CommitRecorded(e) => format!("{} {}", short_sha(&e.sha), e.subject),
            Self::ActionCompleted(e) => format!("{} {}", e.workflow, e.status.as_str()),
            Self::Report(e) => e.title.clone(),
            Self::ToolCalled(e) => e.tool.clone(),
            Self::Unknown { kind, .. } => kind.clone(),
        }
    }
}

pub fn handoff_label(target: &HandoffTarget) -> String {
    match target {
        HandoffTarget::Session { session } => session.short(),
        HandoffTarget::Role { role } => format!("role:{role}"),
        HandoffTarget::Human { handle } => format!("@{handle}"),
    }
}

pub fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::Ulid;

    fn ev(body: EventBody) -> Event {
        Event::new(
            EventId::from_ulid(Ulid::from_parts(1_700_000_000_000, 7)),
            OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            Actor::human("gstohl", None),
            body,
        )
    }

    #[test]
    fn envelope_shape_and_roundtrip() {
        let task = TaskId::from_ulid(Ulid::from_parts(1_700_000_000_000, 1));
        let e = ev(EventBody::TaskCreated(TaskCreated {
            task,
            title: "Fix login".into(),
            body: None,
            recipe: None,
            labels: vec![],
        }));
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["v"], 1);
        assert_eq!(json["kind"], "task.created");
        assert_eq!(json["data"]["title"], "Fix login");
        assert_eq!(json["actor"]["type"], "human");
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn unknown_kinds_survive() {
        let raw = serde_json::json!({
            "v": 2, "id": "evt_01K6M3C2V7QZ8Y9X0W1T2S3R4P", "ts": "2026-10-03T10:31:00Z",
            "actor": {"type": "system", "component": "future"},
            "kind": "attempt.teleported", "data": {"to": "mars"}
        });
        let e: Event = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(e.kind(), "attempt.teleported");
        assert!(matches!(e.body, EventBody::Unknown { .. }));
        assert_eq!(serde_json::to_value(&e).unwrap(), raw);
    }

    #[test]
    fn malformed_known_kind_is_kept_as_unknown() {
        let raw = serde_json::json!({
            "v": 1, "id": "evt_01K6M3C2V7QZ8Y9X0W1T2S3R4P", "ts": "2026-10-03T10:31:00Z",
            "actor": {"type": "human", "handle": "x"},
            "kind": "task.created", "data": {"title": 3}
        });
        let e: Event = serde_json::from_value(raw).unwrap();
        assert!(matches!(e.body, EventBody::Unknown { .. }));
    }
}

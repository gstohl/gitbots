//! Layout of the ledger branches (`gitbots/activity`, `gitbots/logs`).
//!
//! Every path is derived from an id, never from a wall clock, and directories
//! are sharded (UTC hour, then session) so no tree grows large and two writers
//! never touch the same file. Commit boundaries carry no meaning: writers may
//! batch any number of files into one ledger commit.
//!
//! ```text
//! gitbots/activity
//!   LEDGER.json
//!   sessions/<YYYY>/<MM>/<ses_id>.json
//!   events/<YYYY>/<MM>/<DD>/<HH>/<ses_id|_>/<evt_id>.json
//!   idem/<xx>/<sha256(key)>           (idempotency markers; content = key)
//!   conflicts/...                     (union-merge quarantine)
//! gitbots/logs
//!   LEDGER.json
//!   runs/<YYYY>/<MM>/<DD>/<run_id>/<job>.log
//!   sessions/<YYYY>/<MM>/<DD>/<ses_id>/<ulid>-<name>.log
//!   ...*.ptr.json                     (pointer to an out-of-git blob)
//! ```

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::event::Event;
use crate::id::{ProjectId, RunId, SessionId, Ulid};

pub const LEDGER_FORMAT: u32 = 1;
pub const LEDGER_META_PATH: &str = "LEDGER.json";
/// Logs above this size are truncated (or stored out of git via a pointer).
pub const MAX_LOG_BYTES: usize = 25 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerKind {
    Activity,
    Logs,
}

/// `LEDGER.json` at the root of each ledger branch. Two ledgers with a
/// different `project` or `format` must never be merged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerMeta {
    pub format: u32,
    pub kind: LedgerKind,
    pub project: ProjectId,
    /// Bumped only by an approved history rewrite (e.g. purging a leaked
    /// secret). Union merge refuses to mix epochs.
    #[serde(default)]
    pub epoch: u32,
}

impl LedgerMeta {
    pub fn new(kind: LedgerKind, project: ProjectId) -> Self {
        Self { format: LEDGER_FORMAT, kind, project, epoch: 0 }
    }

    /// Whether two ledgers may be union-merged.
    pub fn compatible(&self, other: &Self) -> bool {
        self.format == other.format
            && self.kind == other.kind
            && self.project == other.project
            && self.epoch == other.epoch
    }
}

/// Stored instead of a log too large (or too sensitive) to keep in git.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogPointer {
    pub sha256: String,
    pub size: u64,
    pub uri: String,
}

fn utc(ulid: Ulid) -> OffsetDateTime {
    let ms = i128::from(ulid.timestamp_ms());
    OffsetDateTime::from_unix_timestamp_nanos(ms * 1_000_000).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn ymd(ulid: Ulid) -> String {
    let t = utc(ulid);
    format!("{:04}/{:02}/{:02}", t.year(), u8::from(t.month()), t.day())
}

pub fn event_path(event: &Event) -> String {
    let t = utc(event.id.ulid());
    let session = event.actor.session().map_or("_", SessionId::as_str);
    format!("events/{}/{:02}/{session}/{}.json", ymd(event.id.ulid()), t.hour(), event.id)
}

pub fn session_path(session: &SessionId) -> String {
    let t = utc(session.ulid());
    format!("sessions/{:04}/{:02}/{session}.json", t.year(), u8::from(t.month()))
}

pub fn run_log_path(run: &RunId, job: &str) -> String {
    format!("runs/{}/{run}/{}.log", ymd(run.ulid()), sanitize_segment(job))
}

pub fn session_log_path(session: &SessionId, log: Ulid, name: &str) -> String {
    format!("sessions/{}/{session}/{log}-{}.log", ymd(log), sanitize_segment(name))
}

/// Marker path for an event idempotency key. Identical writers produce
/// identical markers, so they never conflict in a union merge.
pub fn idem_path(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let hex: String = Sha256::digest(key.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    format!("idem/{}/{hex}", &hex[..2])
}

pub fn is_event_path(path: &str) -> bool {
    path.starts_with("events/") && path.ends_with(".json")
}

pub fn is_session_path(path: &str) -> bool {
    path.starts_with("sessions/") && path.ends_with(".json")
}

/// Make an arbitrary name safe as a single path segment.
pub fn sanitize_segment(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
        .take(64)
        .collect();
    match s.trim_start_matches('.') {
        "" => "_".to_owned(),
        rest => rest.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventBody, Report};
    use crate::id::EventId;
    use crate::identity::{Actor, AgentDescriptor};

    // 2026-10-03T10:31:00.000Z
    const TS: u64 = 1_791_023_460_000;

    #[test]
    fn paths_are_sharded_by_id_time_and_session() {
        let session = SessionId::from_ulid(Ulid::from_parts(TS, 1));
        let actor = Actor::Agent {
            session: session.clone(),
            agent: AgentDescriptor::new("a", "m", "c"),
            parent: None,
        };
        let id = EventId::from_ulid(Ulid::from_parts(TS, 2));
        let report = Report {
            title: "t".into(),
            body: None,
            level: Default::default(),
            task: None,
            attempt: None,
        };
        // The envelope timestamp is deliberately wrong: paths come from the id.
        let e =
            Event::new(id.clone(), OffsetDateTime::UNIX_EPOCH, actor, EventBody::Report(report));
        assert_eq!(event_path(&e), format!("events/2026/10/03/10/{session}/{id}.json"));
        assert_eq!(session_path(&session), format!("sessions/2026/10/{session}.json"));
        assert!(is_event_path(&event_path(&e)));

        let run = RunId::from_ulid(Ulid::from_parts(TS, 3));
        assert_eq!(
            run_log_path(&run, "unit tests"),
            format!("runs/2026/10/03/{run}/unit_tests.log")
        );
    }

    #[test]
    fn idem_paths_are_stable() {
        let p = idem_path("commit:abc");
        assert_eq!(p, idem_path("commit:abc"));
        assert_ne!(p, idem_path("commit:abd"));
        assert!(p.starts_with("idem/") && p.len() == "idem/xx/".len() + 64);
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_segment("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize_segment(""), "_");
        assert_eq!(sanitize_segment("..."), "_");
    }
}

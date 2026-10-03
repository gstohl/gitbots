//! Indexing `gitbots/activity` into the hosted event store (`docs/CLOUD.md`).
//!
//! The walk only descends into `events/` subtrees whose hash differs from
//! the last indexed root tree (the layout is sharded by hour and session, so
//! an append touches a handful of trees). Every new or changed file is parsed
//! as an [`gitbots_core::Event`] and upserted by id, so indexing is idempotent:
//! re-running it, running it concurrently or indexing a fork that shares
//! history with `<prj>` stores each event once.

use std::collections::{HashMap, HashSet};

use futures_util::future::join_all;
use gitbots_core::Event;
use gitbots_core::ledger::is_event_path;
use time::format_description::well_known::Rfc3339;

use crate::source::{EntryKind, SourceError, TreeSource};

/// The ledger branch the indexer reads, without `refs/heads/`.
pub const ACTIVITY_BRANCH: &str = "gitbots/activity";
/// The logs ledger branch, kept in `<prj>-logs`.
pub const LOGS_BRANCH: &str = "gitbots/logs";
const EVENTS_DIR: &str = "events";
/// Blobs read concurrently.
const READ_CONCURRENCY: usize = 8;
/// Ids per [`EventStore::known`] call (D1 binds at most 100 parameters).
const KNOWN_CHUNK: usize = 50;

/// An event file found in the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventFile {
    pub path: String,
    pub blob: String,
}

impl EventFile {
    /// The event id the file name claims (`.../<evt_id>.json`).
    pub fn claimed_id(&self) -> Option<&str> {
        let name = self.path.rsplit('/').next()?;
        name.strip_suffix(".json").filter(|id| id.starts_with("evt_"))
    }
}

/// Event files under `events/` of `new_root` that are not at the same path
/// with the same blob in `old_root`, sorted by path.
pub async fn changed_event_files<S: TreeSource + ?Sized>(
    src: &S,
    new_root: &str,
    old_root: Option<&str>,
) -> Result<Vec<EventFile>, SourceError> {
    if old_root == Some(new_root) {
        return Ok(vec![]);
    }
    let events_dir = |entries: Vec<crate::source::TreeEntry>| {
        entries.into_iter().find(|e| e.name == EVENTS_DIR && e.kind == EntryKind::Tree)
    };
    let Some(new_events) = events_dir(src.read_tree(new_root).await?) else { return Ok(vec![]) };
    let old_events = match old_root {
        Some(old) => events_dir(src.read_tree(old).await?).map(|e| e.sha),
        None => None,
    };
    let mut out = Vec::new();
    let mut stack = vec![(EVENTS_DIR.to_owned(), new_events.sha, old_events)];
    while let Some((dir, new, old)) = stack.pop() {
        if old.as_deref() == Some(new.as_str()) {
            continue;
        }
        let before: HashMap<String, crate::source::TreeEntry> = match &old {
            Some(old) => {
                src.read_tree(old).await?.into_iter().map(|e| (e.name.clone(), e)).collect()
            }
            None => HashMap::new(),
        };
        for entry in src.read_tree(&new).await? {
            let prev = before.get(&entry.name);
            if prev.is_some_and(|p| p.sha == entry.sha && p.kind == entry.kind) {
                continue;
            }
            let path = format!("{dir}/{}", entry.name);
            match entry.kind {
                EntryKind::Tree => {
                    let prev_tree =
                        prev.filter(|p| p.kind == EntryKind::Tree).map(|p| p.sha.clone());
                    stack.push((path, entry.sha, prev_tree));
                }
                EntryKind::Blob if is_event_path(&path) => {
                    out.push(EventFile { path, blob: entry.sha });
                }
                _ => {}
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// One parsed event, ready for the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedEvent {
    pub id: String,
    pub kind: String,
    /// RFC 3339.
    pub ts: String,
    /// The acting session (`actor.session`), if an agent acted.
    pub session: Option<String>,
    /// The task the event names directly.
    pub task: Option<String>,
    /// The attempt the event names directly.
    pub attempt: Option<String>,
    /// The actor claims to be a human. Forks (pushed by agents) may not
    /// contribute such events.
    pub human: bool,
    pub path: String,
    pub blob: String,
    /// The file as written, so fields this build doesn't know survive.
    pub json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("{0}: not UTF-8")]
    Encoding(String),
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
}

pub fn parse_event_file(file: &EventFile, bytes: &[u8]) -> Result<IndexedEvent, ParseError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ParseError::Encoding(file.path.clone()))?;
    let invalid = |message: String| ParseError::Invalid { path: file.path.clone(), message };
    let event: Event = serde_json::from_str(text).map_err(|e| invalid(e.to_string()))?;
    let ts = event.ts.format(&Rfc3339).map_err(|e| invalid(e.to_string()))?;
    Ok(IndexedEvent {
        id: event.id.to_string(),
        kind: event.kind().to_owned(),
        ts,
        session: event.actor.session().map(ToString::to_string),
        task: event.body.task().map(ToString::to_string),
        attempt: event.body.attempt().map(ToString::to_string),
        human: event.actor.is_human(),
        path: file.path.clone(),
        blob: file.blob.clone(),
        json: text.trim_end().to_owned(),
    })
}

/// Where indexing of one repo stands: the activity commit and its root tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Checkpoint {
    pub commit: Option<String>,
    pub tree: Option<String>,
}

/// The hosted event store (D1 in the Worker).
#[allow(async_fn_in_trait)]
pub trait EventStore {
    /// Blob ids of the events in `ids` that are already stored, by event id.
    async fn known(&self, ids: &[&str]) -> Result<HashMap<String, String>, String>;

    /// Inserts or replaces events by id; returns how many it stored (a
    /// store may refuse some, e.g. human events pushed to a fork).
    async fn upsert(&self, events: &[IndexedEvent]) -> Result<u32, String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexOutcome {
    /// Root tree of the indexed commit; the next checkpoint when `complete`.
    pub tree: String,
    /// Event files stored by this run (new, or changed since last stored).
    pub new_events: u32,
    /// Event files that did not parse, counted and skipped.
    pub unreadable: Vec<String>,
    /// False when `budget` ran out: the caller must not advance its
    /// checkpoint, and the next run continues where this one stopped.
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IndexError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("event store: {0}")]
    Store(String),
}

/// Indexes the activity tree of `commit`, reading at most `budget` blobs.
pub async fn index_commit<S, E>(
    src: &S,
    store: &E,
    commit: &str,
    last: &Checkpoint,
    budget: usize,
) -> Result<IndexOutcome, IndexError>
where
    S: TreeSource + ?Sized,
    E: EventStore + ?Sized,
{
    let tree = src.read_commit(commit).await?.tree;
    let files = changed_event_files(src, &tree, last.tree.as_deref()).await?;

    // Skip files the store already holds with the same blob: left by a run
    // that ran out of budget, or shared with another repo of the project.
    let mut todo = Vec::new();
    for chunk in files.chunks(KNOWN_CHUNK) {
        let ids: Vec<&str> = chunk.iter().filter_map(EventFile::claimed_id).collect();
        let known = store.known(&ids).await.map_err(IndexError::Store)?;
        todo.extend(
            chunk
                .iter()
                .filter(|f| f.claimed_id().is_none_or(|id| known.get(id) != Some(&f.blob)))
                .cloned(),
        );
    }

    let complete = todo.len() <= budget;
    todo.truncate(budget);
    let mut out = IndexOutcome { tree, new_events: 0, unreadable: vec![], complete };
    let mut seen = HashSet::new();
    for chunk in todo.chunks(READ_CONCURRENCY) {
        let blobs = join_all(chunk.iter().map(|f| src.read_blob(&f.blob))).await;
        let mut parsed = Vec::new();
        for (file, bytes) in chunk.iter().zip(blobs) {
            match parse_event_file(file, &bytes?) {
                // Two files claiming one id: keep the first (paths are sorted).
                Ok(e) if !seen.insert(e.id.clone()) => {}
                Ok(e) => parsed.push(e),
                Err(_) => out.unreadable.push(file.path.clone()),
            }
        }
        if !parsed.is_empty() {
            out.new_events += store.upsert(&parsed).await.map_err(IndexError::Store)?;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gitbots_core::event::{EventBody, Report, TaskCreated};
    use gitbots_core::ledger::event_path;
    use gitbots_core::{Actor, EventId, TaskId, Ulid};
    use time::OffsetDateTime;

    use super::*;
    use crate::source::block_on;
    use crate::source::mem::MemRepo;

    #[derive(Default)]
    struct MemStore(RefCell<HashMap<String, IndexedEvent>>);

    impl EventStore for MemStore {
        async fn known(&self, ids: &[&str]) -> Result<HashMap<String, String>, String> {
            let map = self.0.borrow();
            Ok(ids
                .iter()
                .filter_map(|id| map.get(*id).map(|e| ((*id).to_owned(), e.blob.clone())))
                .collect())
        }

        async fn upsert(&self, events: &[IndexedEvent]) -> Result<u32, String> {
            let mut map = self.0.borrow_mut();
            for e in events {
                map.insert(e.id.clone(), e.clone());
            }
            Ok(u32::try_from(events.len()).unwrap())
        }
    }

    // 2026-10-03T10:31:00.000Z
    const TS: u64 = 1_791_023_460_000;

    fn event(n: u64) -> Event {
        let id = EventId::from_ulid(Ulid::from_parts(TS + n * 3_600_000, u128::from(n)));
        let body = if n.is_multiple_of(2) {
            EventBody::TaskCreated(TaskCreated {
                task: TaskId::from_ulid(Ulid::from_parts(TS, u128::from(n))),
                title: format!("task {n}"),
                body: None,
                recipe: None,
                labels: vec![],
            })
        } else {
            EventBody::Report(Report {
                title: format!("report {n}"),
                body: None,
                level: Default::default(),
                task: None,
                attempt: None,
            })
        };
        Event::new(id, OffsetDateTime::UNIX_EPOCH, Actor::human("gstohl", None), body)
    }

    fn files(events: &[Event], extra: &[(&str, &[u8])]) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> =
            events.iter().map(|e| (event_path(e), serde_json::to_vec_pretty(e).unwrap())).collect();
        out.push(("LEDGER.json".into(), b"{}".to_vec()));
        out.push(("idem/ab/abcdef".into(), b"key".to_vec()));
        out.extend(extra.iter().map(|(p, b)| ((*p).to_owned(), b.to_vec())));
        out
    }

    fn commit(repo: &MemRepo, files: &[(String, Vec<u8>)], parent: Option<&str>) -> String {
        let refs: Vec<(&str, &[u8])> =
            files.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        let tree = repo.tree(&refs);
        repo.commit(&tree, &parent.into_iter().collect::<Vec<_>>(), "ledger")
    }

    #[test]
    fn indexes_new_events_and_prunes_unchanged_subtrees() {
        let repo = MemRepo::default();
        let store = MemStore::default();
        let first: Vec<Event> = (0..6).map(event).collect();
        let bad: &[u8] = b"{not json";
        let bad_file = [("events/2026/10/03/10/_/evt_bad.json", bad)];
        let c1 = commit(&repo, &files(&first, &bad_file), None);

        let out = block_on(index_commit(&repo, &store, &c1, &Checkpoint::default(), 100)).unwrap();
        assert!(out.complete);
        assert_eq!(out.new_events, 6);
        assert_eq!(out.unreadable, ["events/2026/10/03/10/_/evt_bad.json"]);
        let stored = store.0.borrow().get(first[0].id.as_str()).cloned().unwrap();
        assert_eq!(stored.kind, "task.created");
        assert_eq!(stored.ts, "1970-01-01T00:00:00Z");
        assert_eq!(serde_json::from_str::<Event>(&stored.json).unwrap(), first[0]);
        assert!(stored.task.is_some() && stored.session.is_none() && stored.human);

        // Re-running on the same commit reads nothing.
        let cp = Checkpoint { commit: Some(c1.clone()), tree: Some(out.tree.clone()) };
        repo.blob_reads.set(0);
        let again = block_on(index_commit(&repo, &store, &c1, &cp, 100)).unwrap();
        assert_eq!((again.new_events, repo.blob_reads.get()), (0, 0));

        // One more event in a new hour shard: only that blob is read, and
        // the unchanged hour shards are not descended into.
        let mut second = first.clone();
        second.push(event(9));
        let c2 = commit(&repo, &files(&second, &bad_file), Some(&c1));
        repo.blob_reads.set(0);
        repo.tree_reads.set(0);
        let out2 = block_on(index_commit(&repo, &store, &c2, &cp, 100)).unwrap();
        assert_eq!(out2.new_events, 1);
        assert_eq!(repo.blob_reads.get(), 1);
        // root x2, events x2, 2026 x2, 10 x2, 03 x2, new hour + its session dir.
        assert!(repo.tree_reads.get() <= 12, "read {} trees", repo.tree_reads.get());
        assert_eq!(store.0.borrow().len(), 7);
    }

    #[test]
    fn budget_leaves_the_rest_for_the_next_run() {
        let repo = MemRepo::default();
        let store = MemStore::default();
        let all: Vec<Event> = (0..10).map(event).collect();
        let c = commit(&repo, &files(&all, &[]), None);
        let out = block_on(index_commit(&repo, &store, &c, &Checkpoint::default(), 4)).unwrap();
        assert!(!out.complete);
        assert_eq!(out.new_events, 4);
        // Caller kept the old checkpoint; already stored files are skipped.
        repo.blob_reads.set(0);
        let rest = block_on(index_commit(&repo, &store, &c, &Checkpoint::default(), 100)).unwrap();
        assert!(rest.complete);
        assert_eq!((rest.new_events, repo.blob_reads.get()), (6, 6));
        assert_eq!(store.0.borrow().len(), 10);
    }

    #[test]
    fn no_events_dir() {
        let repo = MemRepo::default();
        let c = commit(&repo, &[("LEDGER.json".into(), b"{}".to_vec())], None);
        let out =
            block_on(index_commit(&repo, &MemStore::default(), &c, &Checkpoint::default(), 9))
                .unwrap();
        assert!(out.complete && out.new_events == 0);
    }

    #[test]
    fn claimed_ids() {
        let f = |p: &str| EventFile { path: p.into(), blob: "x".into() };
        assert_eq!(f("events/a/evt_01K.json").claimed_id(), Some("evt_01K"));
        assert_eq!(f("events/a/other.json").claimed_id(), None);
    }
}

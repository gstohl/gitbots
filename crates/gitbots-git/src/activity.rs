//! The `gitbots/activity` ledger: one JSON file per event, plus session records
//! and idempotency markers.

use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::Deref;
use std::str::FromStr;

use anyhow::Result;
use gitbots_core::event::Event;
use gitbots_core::id::EventId;
use gitbots_core::identity::Session;
use gitbots_core::ledger::{
    LedgerKind, event_path, idem_path, is_event_path, is_session_path, session_path,
};
use gitbots_core::redact::redact;
use gix::ObjectId;
use serde::Serialize;
use serde_json::Value;

use crate::Repo;
use crate::ledger::{Ledger, Plan, blob_exists};
use crate::objects;

/// The activity ledger. Derefs to [`Ledger`] for `ensure`, `tip`, `read`, ...
#[derive(Clone, Debug)]
pub struct Activity<'r>(pub Ledger<'r>);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppendOutcome {
    /// `None` if every event was a duplicate.
    pub commit: Option<String>,
    pub written: Vec<EventId>,
    /// Skipped: the idempotency key or the event id was already recorded.
    pub duplicates: Vec<EventId>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventsRead {
    /// Sorted by id.
    pub events: Vec<Event>,
    /// Paths under `events/` that did not parse.
    pub unreadable: Vec<String>,
}

/// One event, serialized and written as a blob, ready to commit.
struct Staged {
    index: usize,
    path: String,
    blob: ObjectId,
    /// `(idem_path, marker blob, key)`.
    marker: Option<(String, ObjectId, String)>,
}

impl<'r> Activity<'r> {
    pub fn new(repo: &'r Repo, branch_short: &str) -> Self {
        Activity(Ledger::new(repo, branch_short, LedgerKind::Activity))
    }

    /// Records `events` in one commit, skipping any whose idempotency key
    /// (or id) is already in the ledger or earlier in the batch. String
    /// values are redacted before writing.
    pub fn append(&self, events: &[Event]) -> Result<AppendOutcome> {
        let repo = self.0.repo().local();
        let staged = events
            .iter()
            .enumerate()
            .map(|(i, e)| stage(&repo, i, e))
            .collect::<Result<Vec<_>>>()?;
        self.0.update(&repo, |tip| {
            let tip = self.0.require(tip)?;
            let tree = objects::tree_of(&repo, tip)?;
            let mut outcome = AppendOutcome::default();
            let mut files = Vec::new();
            let mut paths = HashSet::new();
            let mut keys = HashSet::new();
            let mut kept = Vec::new();
            for s in &staged {
                let id = events[s.index].id.clone();
                let key_dup = match &s.marker {
                    Some((marker, _, key)) => {
                        keys.contains(key) || blob_exists(&repo, tree, marker)?
                    }
                    None => false,
                };
                if key_dup || paths.contains(&s.path) || blob_exists(&repo, tree, &s.path)? {
                    outcome.duplicates.push(id);
                    continue;
                }
                paths.insert(s.path.clone());
                files.push((s.path.clone(), s.blob));
                if let Some((marker, blob, key)) = &s.marker {
                    keys.insert(key.clone());
                    files.push((marker.clone(), *blob));
                }
                outcome.written.push(id);
                kept.push(&events[s.index]);
            }
            if files.is_empty() {
                return Ok(Plan::Keep(outcome));
            }
            let message = match kept.as_slice() {
                [one] => event_message(one),
                many => format!("{} events", many.len()),
            };
            let commit = self.0.commit_files(&repo, tip, &files, &message)?;
            outcome.commit = Some(commit.to_string());
            Ok(Plan::Move { to: commit, log: format!("gitbots: {message}"), value: outcome })
        })
    }

    /// Records a session and its `session.started` event in one commit.
    /// Parts already recorded are skipped; returns the resulting tip.
    pub fn put_session(&self, session: &Session, started: &Event) -> Result<String> {
        let repo = self.0.repo().local();
        let session_file = (session_path(&session.id), repo.write_blob(json(session)?)?.detach());
        let event = stage(&repo, 0, started)?;
        self.0.update(&repo, |tip| {
            let tip = self.0.require(tip)?;
            let tree = objects::tree_of(&repo, tip)?;
            let mut files = Vec::new();
            if !blob_exists(&repo, tree, &session_file.0)? {
                files.push(session_file.clone());
            }
            let event_dup = blob_exists(&repo, tree, &event.path)?
                || match &event.marker {
                    Some((marker, _, _)) => blob_exists(&repo, tree, marker)?,
                    None => false,
                };
            if !event_dup {
                files.push((event.path.clone(), event.blob));
                files.extend(event.marker.iter().map(|(path, blob, _)| (path.clone(), *blob)));
            }
            if files.is_empty() {
                return Ok(Plan::Keep(tip.to_string()));
            }
            let message = event_message(started);
            let commit = self.0.commit_files(&repo, tip, &files, &message)?;
            Ok(Plan::Move {
                to: commit,
                log: format!("gitbots: {message}"),
                value: commit.to_string(),
            })
        })
    }

    /// Every event, sorted by id.
    pub fn events(&self) -> Result<EventsRead> {
        let mut read = EventsRead::default();
        for (path, bytes) in self.0.read_all("events")? {
            if !is_event_path(&path) {
                continue;
            }
            match serde_json::from_slice::<Event>(&bytes) {
                Ok(event) => read.events.push(event),
                Err(_) => read.unreadable.push(path),
            }
        }
        read.events.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(read)
    }

    /// Every session record that parses, sorted by id.
    pub fn sessions(&self) -> Result<Vec<Session>> {
        let mut sessions: Vec<Session> = self
            .0
            .read_all("sessions")?
            .into_iter()
            .filter(|(path, _)| is_session_path(path))
            .filter_map(|(_, bytes)| serde_json::from_slice(&bytes).ok())
            .collect();
        sessions.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(sessions)
    }

    /// The largest event id, found by descending the newest
    /// `events/YYYY/MM/DD/HH` shard and taking the largest file across its
    /// session directories. Reads a handful of trees, not every event.
    pub fn latest_event_id(&self) -> Result<Option<EventId>> {
        let repo = self.0.repo().local();
        let Some(root) = self.0.tip_tree(&repo)? else { return Ok(None) };
        match objects::lookup(&repo, root, "events")? {
            Some((mode, events)) if mode.is_tree() => latest_in(&repo, events, 4),
            _ => Ok(None),
        }
    }
}

impl<'r> Deref for Activity<'r> {
    type Target = Ledger<'r>;

    fn deref(&self) -> &Ledger<'r> {
        &self.0
    }
}

/// `depth` shard levels (YYYY, MM, DD, HH) above the session directories.
/// Newest shard first; falls back to older ones only if a shard holds no
/// event file.
fn latest_in(repo: &gix::Repository, tree: ObjectId, depth: usize) -> Result<Option<EventId>> {
    if depth == 0 {
        let mut best: Option<EventId> = None;
        for (_, is_tree, session_dir) in objects::children_desc(repo, tree)? {
            if !is_tree {
                continue;
            }
            let newest = objects::children_desc(repo, session_dir)?
                .into_iter()
                .filter(|(_, is_tree, _)| !is_tree)
                .find_map(|(name, _, _)| EventId::from_str(name.strip_suffix(".json")?).ok());
            best = best.max(newest);
        }
        return Ok(best);
    }
    for (_, is_tree, child) in objects::children_desc(repo, tree)? {
        if is_tree && let Some(found) = latest_in(repo, child, depth - 1)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn stage(repo: &gix::Repository, index: usize, event: &Event) -> Result<Staged> {
    let marker = match &event.idem {
        Some(key) => Some((idem_path(key), repo.write_blob(key.as_bytes())?.detach(), key.clone())),
        None => None,
    };
    Ok(Staged {
        index,
        path: event_path(event),
        blob: repo.write_blob(json(event)?)?.detach(),
        marker,
    })
}

/// Pretty JSON with every string value redacted (`gitbots_core::redact`).
/// Redacting per string keeps the JSON valid; when nothing matches, the
/// struct is written as is, keeping its field order.
fn json(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut tree = serde_json::to_value(value)?;
    let mut bytes = if redact_strings(&mut tree) {
        serde_json::to_vec_pretty(&tree)?
    } else {
        serde_json::to_vec_pretty(value)?
    };
    bytes.push(b'\n');
    Ok(bytes)
}

/// Whether anything was masked.
fn redact_strings(value: &mut Value) -> bool {
    match value {
        Value::String(s) => match redact(s).0 {
            Cow::Owned(masked) => {
                *s = masked;
                true
            }
            Cow::Borrowed(_) => false,
        },
        Value::Array(items) => items.iter_mut().fold(false, |hit, v| redact_strings(v) | hit),
        Value::Object(map) => map.values_mut().fold(false, |hit, v| redact_strings(v) | hit),
        _ => false,
    }
}

/// `<kind>: <summary>` (first line only).
fn event_message(event: &Event) -> String {
    let summary = event.body.summary();
    format!("{}: {}", event.kind(), summary.lines().next().unwrap_or(""))
}

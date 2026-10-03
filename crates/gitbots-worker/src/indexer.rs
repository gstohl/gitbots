//! Indexes `gitbots/activity` of `<prj>` and its forks into D1
//! (`gitbots_cloud::index` does the walk; this is the IO around it).

use std::collections::HashMap;

use gitbots_cloud::api::RepoIngest;
use gitbots_cloud::index::{ACTIVITY_BRANCH, Checkpoint, EventStore, IndexedEvent, index_commit};

use crate::artifacts::Artifacts;
use crate::db::{Db, ProjectRow, RepoRow};
use crate::http::{ApiError, ApiResult};

struct D1Store<'a> {
    db: &'a Db,
    project: &'a str,
    repo: &'a str,
    primary: bool,
}

impl EventStore for D1Store<'_> {
    async fn known(&self, ids: &[&str]) -> Result<HashMap<String, String>, String> {
        self.db.known_events(self.project, ids).await.map_err(|e| e.to_string())
    }

    async fn upsert(&self, events: &[IndexedEvent]) -> Result<u32, String> {
        // Forks are pushed by agents with a fork token: human decisions
        // only ever reach the index through `<prj>` (the steward).
        let kept: Vec<IndexedEvent> = if self.primary {
            events.to_vec()
        } else {
            events.iter().filter(|e| !e.human).cloned().collect()
        };
        for e in events.iter().filter(|e| !self.primary && e.human) {
            worker::console_warn!("{}: dropped human event {} pushed to a fork", self.repo, e.id);
        }
        self.db
            .upsert_events(self.project, self.repo, self.primary, &kept)
            .await
            .map_err(|e| e.to_string())?;
        // Decisions the steward applied from the outbox: done, even if the
        // ack never arrived. (Fork events never count; they hold no human
        // events.)
        if self.primary {
            let applied: Vec<(&str, &str)> =
                kept.iter().filter_map(|e| Some((e.outbox_item()?, e.id.as_str()))).collect();
            self.db.mark_applied(self.project, &applied).await.map_err(|e| e.to_string())?;
        }
        Ok(u32::try_from(kept.len()).unwrap_or(u32::MAX))
    }
}

pub struct Indexed {
    pub report: RepoIngest,
    /// False when the blob budget ran out before the walk finished.
    pub complete: bool,
}

/// Indexes one repo of `project` (its `main` repo or a fork). Idempotent
/// and safe to run concurrently: events are upserted by id and the
/// checkpoint only moves by compare-and-swap.
pub async fn index_repo(
    artifacts: &Artifacts,
    db: &Db,
    project: &ProjectRow,
    repo: &RepoRow,
    budget: usize,
) -> ApiResult<Indexed> {
    let handle = artifacts.get(&repo.name).await?;
    let tip = handle.tip(ACTIVITY_BRANCH).await?;
    let mut report = RepoIngest { repo: repo.name.clone(), tip: tip.clone(), new_events: 0 };
    let Some(tip) = tip else { return Ok(Indexed { report, complete: true }) };
    if repo.indexed_commit.as_deref() == Some(tip.as_str()) {
        return Ok(Indexed { report, complete: true });
    }
    let store =
        D1Store { db, project: &project.id, repo: &repo.name, primary: repo.role == "main" };
    let last = Checkpoint { commit: repo.indexed_commit.clone(), tree: repo.indexed_tree.clone() };
    let out = index_commit(&handle, &store, &tip, &last, budget)
        .await
        .map_err(|e| ApiError::new(502, format!("indexing {}: {e}", repo.name)))?;
    if !out.unreadable.is_empty() {
        worker::console_warn!(
            "{}: {} unreadable event files, e.g. {}",
            repo.name,
            out.unreadable.len(),
            out.unreadable[0]
        );
    }
    if out.complete {
        db.advance_checkpoint(&repo.name, repo.indexed_commit.as_deref(), &tip, &out.tree).await?;
    }
    report.new_events = out.new_events;
    Ok(Indexed { report, complete: out.complete })
}

/// `<prj>` and every fork of the project.
pub async fn indexable_repos(db: &Db, project: &ProjectRow) -> ApiResult<Vec<RepoRow>> {
    let mut repos = db.repos(&project.id, "main").await?;
    repos.extend(db.repos(&project.id, "fork").await?);
    Ok(repos)
}

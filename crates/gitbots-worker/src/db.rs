//! D1 access (schema in `migrations/`).

use std::collections::HashMap;

use gitbots_cloud::index::IndexedEvent;
use serde::Deserialize;
use wasm_bindgen::JsValue;
use worker::{D1Database, D1PreparedStatement, Env, Result};

use crate::util::now_rfc3339;

pub struct Db(D1Database);

#[derive(Clone, Debug, Deserialize)]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    pub repo: String,
    pub logs_repo: String,
    pub owner_key_hash: String,
    pub trusted_branch: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RepoRow {
    pub name: String,
    pub project_id: String,
    pub role: String,
    pub remote: String,
    pub attempt: Option<String>,
    pub session: Option<String>,
    pub created_at: String,
    pub indexed_commit: Option<String>,
    pub indexed_tree: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OutboxRow {
    pub id: String,
    pub created_at: String,
    pub kind: String,
    pub body: String,
    pub actor: String,
    pub status: String,
}

/// An outbox item the steward acked with an error.
#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct OutboxError {
    pub id: String,
    pub kind: String,
    /// `done` (decision recorded, e.g. only the merge failed) or `failed`.
    pub status: String,
    pub event: Option<String>,
    #[serde(rename(serialize = "error"))]
    pub last_error: String,
    pub acked_at: Option<String>,
}

#[derive(Deserialize)]
struct Count {
    n: f64,
}

#[derive(Deserialize)]
struct Known {
    id: String,
    blob: String,
}

#[derive(Deserialize)]
struct JsonRow {
    json: String,
}

#[derive(Deserialize)]
struct MaxId {
    id: Option<String>,
}

fn s(v: &str) -> JsValue {
    JsValue::from_str(v)
}

fn o(v: Option<&str>) -> JsValue {
    v.map_or(JsValue::NULL, JsValue::from_str)
}

fn changes(r: &worker::D1Result) -> usize {
    r.meta().ok().flatten().and_then(|m| m.changes).unwrap_or(0)
}

impl Db {
    pub fn from_env(env: &Env) -> Result<Self> {
        env.d1("DB").map(Self)
    }

    fn stmt(&self, sql: &str, args: &[JsValue]) -> Result<D1PreparedStatement> {
        self.0.prepare(sql).bind(args)
    }

    // ---- projects -------------------------------------------------------

    pub async fn project_by_key_hash(&self, hash: &str) -> Result<Option<ProjectRow>> {
        self.stmt("SELECT * FROM projects WHERE owner_key_hash = ?1", &[s(hash)])?.first(None).await
    }

    pub async fn project(&self, id: &str) -> Result<Option<ProjectRow>> {
        self.stmt("SELECT * FROM projects WHERE id = ?1", &[s(id)])?.first(None).await
    }

    /// Inserts the project and its `main` and `logs` repos atomically.
    pub async fn insert_project(
        &self,
        p: &ProjectRow,
        main_remote: &str,
        logs_remote: &str,
    ) -> Result<()> {
        let repo = "INSERT INTO repos (name, project_id, role, remote, created_at) \
                    VALUES (?1, ?2, ?3, ?4, ?5) \
                    ON CONFLICT(name) DO UPDATE SET project_id = excluded.project_id, \
                    role = excluded.role, remote = excluded.remote";
        self.0
            .batch(vec![
                self.stmt(
                    "INSERT INTO projects (id, name, repo, logs_repo, owner_key_hash, \
                     trusted_branch, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    &[
                        s(&p.id),
                        s(&p.name),
                        s(&p.repo),
                        s(&p.logs_repo),
                        s(&p.owner_key_hash),
                        s(&p.trusted_branch),
                        s(&p.created_at),
                    ],
                )?,
                self.stmt(
                    repo,
                    &[s(&p.repo), s(&p.id), s("main"), s(main_remote), s(&p.created_at)],
                )?,
                self.stmt(
                    repo,
                    &[s(&p.logs_repo), s(&p.id), s("logs"), s(logs_remote), s(&p.created_at)],
                )?,
            ])
            .await?;
        Ok(())
    }

    // ---- repos ----------------------------------------------------------

    pub async fn repo(&self, name: &str) -> Result<Option<RepoRow>> {
        self.stmt("SELECT * FROM repos WHERE name = ?1", &[s(name)])?.first(None).await
    }

    pub async fn repos(&self, project: &str, role: &str) -> Result<Vec<RepoRow>> {
        self.stmt(
            "SELECT * FROM repos WHERE project_id = ?1 AND role = ?2 ORDER BY name",
            &[s(project), s(role)],
        )?
        .all()
        .await?
        .results()
    }

    pub async fn fork_for_attempt(&self, project: &str, attempt: &str) -> Result<Option<RepoRow>> {
        self.stmt(
            "SELECT * FROM repos WHERE project_id = ?1 AND role = 'fork' AND attempt = ?2",
            &[s(project), s(attempt)],
        )?
        .first(None)
        .await
    }

    pub async fn insert_fork(&self, r: &RepoRow) -> Result<()> {
        self.stmt(
            "INSERT INTO repos (name, project_id, role, remote, attempt, session, created_at) \
             VALUES (?1, ?2, 'fork', ?3, ?4, ?5, ?6) ON CONFLICT(name) DO NOTHING",
            &[
                s(&r.name),
                s(&r.project_id),
                s(&r.remote),
                o(r.attempt.as_deref()),
                o(r.session.as_deref()),
                s(&r.created_at),
            ],
        )?
        .run()
        .await?;
        Ok(())
    }

    /// Moves the indexing checkpoint from `from` (compare-and-swap, so a
    /// slower concurrent run never moves it back). Returns whether it moved.
    pub async fn advance_checkpoint(
        &self,
        repo: &str,
        from: Option<&str>,
        commit: &str,
        tree: &str,
    ) -> Result<bool> {
        let r = self
            .stmt(
                "UPDATE repos SET indexed_commit = ?2, indexed_tree = ?3, indexed_at = ?4 \
                 WHERE name = ?1 AND indexed_commit IS ?5",
                &[s(repo), s(commit), s(tree), s(&now_rfc3339()), o(from)],
            )?
            .run()
            .await?;
        Ok(changes(&r) > 0)
    }

    // ---- tokens ---------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_token(
        &self,
        id: &str,
        project: &str,
        repo: &str,
        scope: &str,
        session: Option<&str>,
        expires_at: &str,
    ) -> Result<()> {
        self.stmt(
            "INSERT INTO tokens (id, project_id, repo, scope, session, created_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(id) DO NOTHING",
            &[s(id), s(project), s(repo), s(scope), o(session), s(&now_rfc3339()), s(expires_at)],
        )?
        .run()
        .await?;
        Ok(())
    }

    // ---- events ---------------------------------------------------------

    /// Every stored event of the project, oldest first, as written.
    pub async fn events_json(&self, project: &str) -> Result<Vec<String>> {
        let rows: Vec<JsonRow> = self
            .stmt("SELECT json FROM events WHERE project_id = ?1 ORDER BY id", &[s(project)])?
            .all()
            .await?
            .results()?;
        Ok(rows.into_iter().map(|r| r.json).collect())
    }

    pub async fn known_events(
        &self,
        project: &str,
        ids: &[&str],
    ) -> Result<HashMap<String, String>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let marks: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 2)).collect();
        let sql = format!(
            "SELECT id, blob FROM events WHERE project_id = ?1 AND id IN ({})",
            marks.join(", ")
        );
        let mut args = vec![s(project)];
        args.extend(ids.iter().map(|id| s(id)));
        let rows: Vec<Known> = self.stmt(&sql, &args)?.all().await?.results()?;
        Ok(rows.into_iter().map(|r| (r.id, r.blob)).collect())
    }

    /// Upserts by `(project, id)`. Events from `<prj>` win; a fork can add
    /// events but never replace one already stored.
    pub async fn upsert_events(
        &self,
        project: &str,
        repo: &str,
        primary: bool,
        events: &[IndexedEvent],
    ) -> Result<()> {
        let on_conflict = if primary {
            "ON CONFLICT(project_id, id) DO UPDATE SET kind = excluded.kind, ts = excluded.ts, \
             session = excluded.session, task = excluded.task, attempt = excluded.attempt, \
             repo = excluded.repo, path = excluded.path, blob = excluded.blob, \
             json = excluded.json, indexed_at = excluded.indexed_at"
        } else {
            "ON CONFLICT(project_id, id) DO NOTHING"
        };
        let sql = format!(
            "INSERT INTO events (project_id, id, kind, ts, session, task, attempt, repo, path, \
             blob, json, indexed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
             {on_conflict}"
        );
        let now = now_rfc3339();
        let stmts = events
            .iter()
            .map(|e| {
                self.stmt(
                    &sql,
                    &[
                        s(project),
                        s(&e.id),
                        s(&e.kind),
                        s(&e.ts),
                        o(e.session.as_deref()),
                        o(e.task.as_deref()),
                        o(e.attempt.as_deref()),
                        s(repo),
                        s(&e.path),
                        s(&e.blob),
                        s(&e.json),
                        s(&now),
                    ],
                )
            })
            .collect::<Result<Vec<_>>>()?;
        if !stmts.is_empty() {
            self.0.batch(stmts).await?;
        }
        Ok(())
    }

    /// The newest indexed event id.
    pub async fn latest_event(&self, project: &str) -> Result<Option<String>> {
        let row: Option<MaxId> = self
            .stmt("SELECT max(id) AS id FROM events WHERE project_id = ?1", &[s(project)])?
            .first(None)
            .await?;
        Ok(row.and_then(|r| r.id))
    }

    // ---- outbox ---------------------------------------------------------

    pub async fn insert_outbox(
        &self,
        id: &str,
        project: &str,
        kind: &str,
        body: &str,
        actor: &str,
    ) -> Result<()> {
        self.stmt(
            "INSERT INTO outbox (id, project_id, created_at, kind, body, actor) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            &[s(id), s(project), s(&now_rfc3339()), s(kind), s(body), s(actor)],
        )?
        .run()
        .await?;
        Ok(())
    }

    pub async fn pending_outbox(&self, project: &str) -> Result<Vec<OutboxRow>> {
        self.stmt(
            "SELECT id, created_at, kind, body, actor, status FROM outbox \
             WHERE project_id = ?1 AND status = 'pending' ORDER BY id",
            &[s(project)],
        )?
        .all()
        .await?
        .results()
    }

    pub async fn count_pending(&self, project: &str) -> Result<u64> {
        let row: Option<Count> = self
            .stmt(
                "SELECT count(*) AS n FROM outbox WHERE project_id = ?1 AND status = 'pending'",
                &[s(project)],
            )?
            .first(None)
            .await?;
        // A row count is a small non-negative integer.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok(row.map_or(0, |r| r.n as u64))
    }

    pub async fn outbox_item(&self, project: &str, id: &str) -> Result<Option<OutboxRow>> {
        self.stmt(
            "SELECT id, created_at, kind, body, actor, status FROM outbox \
             WHERE project_id = ?1 AND id = ?2",
            &[s(project), s(id)],
        )?
        .first(None)
        .await
    }

    /// Records the steward's ack: `event` (the decision is in the ledger)
    /// and/or `error` (it could not be applied, or applied with a failed
    /// merge). With an event the item is `done` even if there is an error,
    /// which stays visible as `last_error`. Later acks are no-ops.
    pub async fn ack_outbox(
        &self,
        project: &str,
        id: &str,
        event: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        self.stmt(
            "UPDATE outbox SET event = coalesce(?3, event), last_error = ?4, acked_at = ?5, \
             status = CASE WHEN coalesce(?3, event) IS NOT NULL THEN 'done' \
                           WHEN ?4 IS NOT NULL THEN 'failed' ELSE 'done' END \
             WHERE project_id = ?1 AND id = ?2 AND acked_at IS NULL",
            &[s(project), s(id), o(event), o(error), s(&now_rfc3339())],
        )?
        .run()
        .await?;
        Ok(())
    }

    /// Marks items applied whose event the indexer found in `<prj>`
    /// (`idem: "outbox:<id>"`), in case the steward's ack was lost.
    pub async fn mark_applied(&self, project: &str, applied: &[(&str, &str)]) -> Result<()> {
        let stmts = applied
            .iter()
            .map(|(item, event)| {
                self.stmt(
                    "UPDATE outbox SET status = 'done', event = coalesce(event, ?3) \
                     WHERE project_id = ?1 AND id = ?2 AND status = 'pending'",
                    &[s(project), s(item), s(event)],
                )
            })
            .collect::<Result<Vec<_>>>()?;
        if !stmts.is_empty() {
            self.0.batch(stmts).await?;
        }
        Ok(())
    }

    /// The latest acked items that carry an error, newest first.
    pub async fn outbox_errors(&self, project: &str, limit: u32) -> Result<Vec<OutboxError>> {
        self.stmt(
            "SELECT id, kind, status, event, last_error, acked_at FROM outbox \
             WHERE project_id = ?1 AND last_error IS NOT NULL ORDER BY id DESC LIMIT ?2",
            &[s(project), JsValue::from_f64(f64::from(limit))],
        )?
        .all()
        .await?
        .results()
    }
}

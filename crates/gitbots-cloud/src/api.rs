//! Request and response bodies of the control-plane API (`/v1`).

use serde::{Deserialize, Serialize};

/// Which repo of a project a token or remote refers to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RepoRef {
    Known(KnownRepo),
    /// A fork repo name, as returned by `POST /v1/forks`.
    Fork(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownRepo {
    Main,
    Logs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    Read,
    Write,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remotes {
    pub main: String,
    pub logs: String,
}

/// `POST /v1/projects` (admin key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProject {
    pub project_id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCreated {
    pub project_id: String,
    /// Shown once; the Worker stores only its SHA-256.
    pub owner_key: String,
    pub namespace: String,
    pub remotes: Remotes,
}

/// `GET /v1/project` (owner key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub project_id: String,
    pub name: String,
    pub namespace: String,
    pub remotes: Remotes,
    pub created_at: String,
}

/// `POST /v1/tokens` (owner key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRequest {
    pub repo: RepoRef,
    pub scope: TokenScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenIssued {
    /// `art_v1_…`; send as `Authorization: Bearer <token>` to the git remote.
    pub token: String,
    /// RFC 3339.
    pub expires_at: String,
    pub remote: String,
}

/// `POST /v1/forks` (owner key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkRequest {
    pub attempt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkCreated {
    pub repo: String,
    pub remote: String,
    pub token: String,
    pub expires_at: String,
}

/// `POST /v1/ingest` (owner key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestReport {
    pub repos: Vec<RepoIngest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoIngest {
    pub repo: String,
    pub tip: Option<String>,
    pub new_events: u32,
}

/// A human decision queued by the hosted dashboard, applied by `gitbots sync`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutboxItem {
    pub id: String,
    pub created_at: String,
    #[serde(flatten)]
    pub action: OutboxAction,
    /// The human who decided (`gitbots_core::Actor`, always `type: human`).
    pub actor: gitbots_core::Actor,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body")]
pub enum OutboxAction {
    #[serde(rename = "task.create")]
    TaskCreate(NewTask),
    #[serde(rename = "review")]
    Review(ReviewRequest),
}

/// Body of `POST /api/tasks` and of a `task.create` outbox item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewTask {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

/// Body of `POST /api/attempts/{id}/review` and of a `review` outbox item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequest {
    /// Present in outbox items; taken from the URL in `/api`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<String>,
    pub decision: gitbots_core::event::ReviewDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub merge: bool,
}

/// `POST /v1/outbox/{id}/ack` (owner key).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxAck {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Any non-2xx response body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_ref_shapes() {
        assert_eq!(serde_json::to_string(&RepoRef::Known(KnownRepo::Main)).unwrap(), "\"main\"");
        let fork: RepoRef = serde_json::from_str("\"prj_x-att-abc123\"").unwrap();
        assert_eq!(fork, RepoRef::Fork("prj_x-att-abc123".into()));
        let main: RepoRef = serde_json::from_str("\"logs\"").unwrap();
        assert_eq!(main, RepoRef::Known(KnownRepo::Logs));
    }

    #[test]
    fn outbox_item_shape() {
        let json = serde_json::json!({
            "id": "obx_1", "created_at": "2026-10-03T10:00:00Z",
            "kind": "review", "body": {"attempt": "att_x", "decision": "accept", "merge": true},
            "actor": {"type": "human", "handle": "gstohl"}
        });
        let item: OutboxItem = serde_json::from_value(json.clone()).unwrap();
        assert!(matches!(&item.action, OutboxAction::Review(r) if r.merge));
        assert_eq!(serde_json::to_value(&item).unwrap(), json);
    }
}

//! Who did it: agents, sessions (one chat/run), and humans.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::id::SessionId;

/// Which AI: the unit gitbots benchmarks on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AgentDescriptor {
    /// Model vendor, e.g. `anthropic`, `openai`.
    pub provider: String,
    /// Model id, e.g. `claude-opus-5-5`, `gpt-5-codex`.
    pub model: String,
    /// Harness that drives the model, e.g. `claude-code`, `codex`, `chatgpt`.
    pub client: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
}

impl AgentDescriptor {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        client: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            client: client.into(),
            client_version: None,
        }
    }

    /// Display and grouping key: `provider/model@client`. Not parseable
    /// (model ids may contain `/` or `@`); the session id is authoritative.
    pub fn key(&self) -> String {
        format!("{}/{}@{}", self.provider, self.model, self.client)
    }
}

/// One chat or run of an agent. Subagents point at their parent session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub agent: AgentDescriptor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    /// Free-form role, e.g. `implementer`, `reviewer`, `planner`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Handle of the human this agent reports to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    /// The client's own id for this chat (conversation id, session id, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
}

impl Session {
    pub fn actor(&self) -> Actor {
        Actor::Agent {
            session: self.id.clone(),
            agent: self.agent.clone(),
            parent: self.parent.clone(),
        }
    }
}

/// How the actor of an event was resolved, so readers know how far to
/// trust the attribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// Explicit `--session` flag.
    Flag,
    /// `GITBOTS_SESSION` environment variable.
    Env,
    /// Session bound to the current workroom.
    Worktree,
    /// MCP connection (client info from the protocol).
    Mcp,
    /// Fallback to the human in git config.
    GitConfig,
    /// Interactive terminal confirmed by a human.
    Tty,
    /// gitbots itself (hooks, actions engine).
    System,
    /// The local web UI (`gitbots ui`), authenticated by its launch token.
    Ui,
    /// A value from a newer gitbots; kept readable rather than failing the event.
    #[serde(other)]
    Other,
}

/// The author of an event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Actor {
    Human {
        handle: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        email: Option<String>,
    },
    Agent {
        session: SessionId,
        agent: AgentDescriptor,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<SessionId>,
    },
    /// gitbots itself, e.g. the actions engine.
    System { component: String },
    /// An actor type from a newer gitbots. Treated as untrusted: never human,
    /// never a session.
    #[serde(other)]
    Unknown,
}

impl Actor {
    pub fn human(handle: impl Into<String>, email: Option<String>) -> Self {
        Self::Human { handle: handle.into(), email }
    }

    pub fn system(component: impl Into<String>) -> Self {
        Self::System { component: component.into() }
    }

    pub fn is_human(&self) -> bool {
        matches!(self, Self::Human { .. })
    }

    pub fn session(&self) -> Option<&SessionId> {
        match self {
            Self::Agent { session, .. } => Some(session),
            _ => None,
        }
    }

    pub fn agent_key(&self) -> Option<String> {
        match self {
            Self::Agent { agent, .. } => Some(agent.key()),
            _ => None,
        }
    }

    /// Short display label.
    pub fn label(&self) -> String {
        match self {
            Self::Human { handle, .. } => format!("@{handle}"),
            Self::Agent { session, agent, .. } => format!("{} ({})", agent.key(), session.short()),
            Self::System { component } => format!("system:{component}"),
            Self::Unknown => "unknown".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_format() {
        let a = AgentDescriptor::new("anthropic", "claude-opus-5-5", "claude-code");
        assert_eq!(a.key(), "anthropic/claude-opus-5-5@claude-code");
    }

    #[test]
    fn unknown_via_is_tolerated() {
        let v: Via = serde_json::from_str("\"carrier_pigeon\"").unwrap();
        assert_eq!(v, Via::Other);
        assert_eq!(serde_json::to_string(&Via::Ui).unwrap(), "\"ui\"");
    }

    #[test]
    fn unknown_actor_type_is_tolerated() {
        let a: Actor = serde_json::from_str(r#"{"type":"robot","serial":7}"#).unwrap();
        assert_eq!(a, Actor::Unknown);
        assert!(!a.is_human() && a.session().is_none());
    }

    #[test]
    fn actor_json_shape() {
        let actor = Actor::human("gstohl", None);
        let json = serde_json::to_value(&actor).unwrap();
        assert_eq!(json, serde_json::json!({"type": "human", "handle": "gstohl"}));
    }
}

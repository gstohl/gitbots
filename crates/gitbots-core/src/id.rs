//! Prefixed, time-sortable identifiers (`ses_01K...`, `att_01K...`).
//!
//! Ids wrap a ULID so they sort by creation time and need no coordination.
//! `gitbots-core` never generates ids itself (that needs a clock and randomness,
//! which a wasm host provides differently); callers pass in a [`Ulid`].

use std::fmt;
use std::str::FromStr;

pub use ulid::Ulid;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("expected an id starting with `{expected}_`, got `{got}`")]
    Prefix { expected: &'static str, got: String },
    #[error("`{0}` does not end in a valid ULID")]
    Ulid(String),
}

macro_rules! prefixed_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            pub fn from_ulid(ulid: Ulid) -> Self {
                Self(format!("{}_{}", $prefix, ulid))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn ulid(&self) -> Ulid {
                // Validated on construction.
                Ulid::from_string(&self.0[$prefix.len() + 1..]).expect("validated ulid")
            }

            /// Lowercase random suffix, for branch names and CLI display.
            pub fn short(&self) -> String {
                self.0[self.0.len() - 6..].to_ascii_lowercase()
            }

            /// True if `query` is this id, its ULID, or a (case-insensitive)
            /// prefix/suffix of at least 4 characters of the ULID.
            pub fn matches(&self, query: &str) -> bool {
                let ulid = &self.0[$prefix.len() + 1..];
                let q = query.strip_prefix(concat!($prefix, "_")).unwrap_or(query);
                if q.len() < 4 {
                    return false;
                }
                let q = q.to_ascii_uppercase();
                ulid == q || ulid.starts_with(&q) || ulid.ends_with(&q)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, IdError> {
                let rest = s
                    .strip_prefix(concat!($prefix, "_"))
                    .ok_or_else(|| IdError::Prefix { expected: $prefix, got: s.to_owned() })?;
                Ulid::from_string(rest).map_err(|_| IdError::Ulid(s.to_owned()))?;
                Ok(Self(s.to_owned()))
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(s: String) -> Result<Self, IdError> {
                s.parse()
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

prefixed_id!(
    /// A project (one `.gitbots/manifest.json`).
    ProjectId,
    "prj"
);
prefixed_id!(
    /// One chat or run of an agent.
    SessionId,
    "ses"
);
prefixed_id!(
    /// One ledger event; doubles as its idempotency key.
    EventId,
    "evt"
);
prefixed_id!(
    /// A unit of work that attempts compete or collaborate on.
    TaskId,
    "tsk"
);
prefixed_id!(
    /// One isolated try at a task: a branch + worktree.
    AttemptId,
    "att"
);
prefixed_id!(
    /// One execution of an actions workflow.
    RunId,
    "run"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_short() {
        let id = TaskId::from_ulid(Ulid::from_parts(1_700_000_000_000, 42));
        let parsed: TaskId = id.as_str().parse().unwrap();
        assert_eq!(parsed, id);
        assert_eq!(id.short().len(), 6);
        assert!(id.matches(&id.short()));
        assert!(id.matches(id.as_str()));
        assert!(!id.matches("abc"));
    }

    #[test]
    fn rejects_wrong_prefix() {
        let id = TaskId::from_ulid(Ulid::from_parts(1, 1));
        let err = id.as_str().replace("tsk_", "att_").parse::<TaskId>().unwrap_err();
        assert!(matches!(err, IdError::Prefix { .. }));
        assert!("tsk_nope".parse::<TaskId>().is_err());
    }

    #[test]
    fn serde_validates() {
        let ok: Result<SessionId, _> = serde_json::from_str("\"ses_01K6M3C2V7QZ8Y9X0W1T2S3R4P\"");
        assert!(ok.is_ok());
        let bad: Result<SessionId, _> = serde_json::from_str("\"evt_01K6M3C2V7QZ8Y9X0W1T2S3R4P\"");
        assert!(bad.is_err());
    }
}

//! `.gitbots/manifest.json`: project meta, tenancy, and the agentic mandate.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::id::ProjectId;
use crate::identity::Actor;

pub const MANIFEST_VERSION: u32 = 1;
pub const GITBOTS_DIR: &str = ".gitbots";
pub const MANIFEST_PATH: &str = ".gitbots/manifest.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub project: ProjectMeta,
    pub tenancy: Tenancy,
    pub mandate: Mandate,
    #[serde(default)]
    pub ledger: LedgerConfig,
    #[serde(default)]
    pub workrooms: WorkroomConfig,
}

impl Manifest {
    pub fn new(project: ProjectMeta, owner: Owner, mandate: Mandate) -> Self {
        Self {
            version: MANIFEST_VERSION,
            project,
            tenancy: Tenancy { owner, team: None, workspace: None },
            mandate,
            ledger: LedgerConfig::default(),
            workrooms: WorkroomConfig::default(),
        }
    }

    /// Structural checks that serde can't express.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.version != MANIFEST_VERSION {
            return Err(ManifestError::Version(self.version));
        }
        if !self.mandate.principals.iter().any(|p| p.role == Role::Owner) {
            return Err(ManifestError::NoOwner);
        }
        self.mandate.agents.compile()?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("unsupported manifest version {0} (this build understands {MANIFEST_VERSION})")]
    Version(u32),
    #[error("mandate needs at least one principal with role `owner`")]
    NoOwner,
    #[error("invalid path glob `{glob}`: {message}")]
    Glob { glob: String, message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub id: ProjectId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Owner, then team, then workspace, then repo. Enforced by the hosted layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tenancy {
    pub owner: Owner,
    #[serde(default)]
    pub team: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub kind: OwnerKind,
    pub handle: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerKind {
    User,
    Org,
}

/// What agents are here to do, what they may touch, and which decisions
/// stay with humans.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mandate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    pub autonomy: Autonomy,
    pub principals: Vec<Principal>,
    #[serde(default)]
    pub agents: AgentRules,
    pub approvals: Approvals,
}

impl Mandate {
    pub fn new(autonomy: Autonomy, owner: Principal) -> Self {
        Self {
            goal: None,
            autonomy,
            principals: vec![owner],
            agents: AgentRules::default(),
            approvals: Approvals::for_autonomy(autonomy),
        }
    }

    /// The principal an actor maps to, matched by email, then by handle.
    pub fn principal_for(&self, actor: &Actor) -> Option<&Principal> {
        let Actor::Human { handle, email } = actor else { return None };
        let by_email = email.as_deref().and_then(|email| {
            self.principals
                .iter()
                .find(|p| p.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(email)))
        });
        by_email.or_else(|| self.principals.iter().find(|p| &p.handle == handle))
    }

    pub fn authorize(&self, actor: &Actor, decision: Decision) -> Authorization {
        let required = match self.approvals.get(decision) {
            Approver::Any => return Authorization::Allowed,
            Approver::Reviewer => Role::Reviewer,
            Approver::Maintainer => Role::Maintainer,
            Approver::Owner => Role::Owner,
        };
        if !actor.is_human() {
            return Authorization::NeedsHuman { role: required };
        }
        match self.principal_for(actor) {
            Some(p) if p.role >= required => Authorization::Allowed,
            Some(p) => Authorization::Denied {
                reason: format!("{decision} needs role {required}; @{} is {}", p.handle, p.role),
            },
            None => Authorization::Denied {
                reason: format!("{} is not a principal of this project", actor.label()),
            },
        }
    }

    pub fn is_protected_branch(&self, branch: &str) -> bool {
        let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
        self.agents
            .protected_branches
            .iter()
            .any(|pattern| Glob::new(pattern).is_ok_and(|g| g.compile_matcher().is_match(branch)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Autonomy {
    /// Humans accept every attempt.
    Supervised,
    /// Agents may accept attempts; humans own protected branches.
    Assisted,
    /// Agents may accept and merge; humans own the mandate.
    Autonomous,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub role: Role,
}

/// Human roles, weakest first (ordering is meaningful).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Reviewer,
    Maintainer,
    Owner,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Reviewer => "reviewer",
            Self::Maintainer => "maintainer",
            Self::Owner => "owner",
        })
    }
}

/// Path and branch rules applied to agent-produced changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRules {
    pub allowed_paths: Vec<String>,
    pub denied_paths: Vec<String>,
    pub protected_branches: Vec<String>,
}

impl Default for AgentRules {
    fn default() -> Self {
        Self {
            allowed_paths: vec!["**".into()],
            // Files that grant permissions to future agents or runners.
            denied_paths: [
                ".gitbots/**",
                ".github/**",
                "**/.claude/**",
                "**/.codex/**",
                "**/.cursor/**",
                "**/.mcp.json",
                "**/CLAUDE.md",
                "**/AGENTS.md",
            ]
            .map(String::from)
            .to_vec(),
            protected_branches: vec!["main".into(), "master".into()],
        }
    }
}

impl AgentRules {
    fn compile(&self) -> Result<(GlobSet, GlobSet), ManifestError> {
        fn set(globs: &[String]) -> Result<GlobSet, ManifestError> {
            let mut b = GlobSetBuilder::new();
            for g in globs {
                // Case-insensitive: `.Gitbots/Manifest.json` is the same file on macOS.
                let glob = globset::GlobBuilder::new(g)
                    .literal_separator(true)
                    .case_insensitive(true)
                    .build()
                    .map_err(|e| ManifestError::Glob { glob: g.clone(), message: e.to_string() })?;
                b.add(glob);
            }
            b.build()
                .map_err(|e| ManifestError::Glob { glob: globs.join(","), message: e.to_string() })
        }
        Ok((set(&self.allowed_paths)?, set(&self.denied_paths)?))
    }

    /// Paths (repo-relative, `/`-separated) an agent may not change.
    pub fn violations<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<PathViolation>, ManifestError> {
        let (allowed, denied) = self.compile()?;
        Ok(paths
            .into_iter()
            .filter_map(|p| {
                if denied.is_match(p) {
                    Some(PathViolation { path: p.to_owned(), reason: "denied by mandate" })
                } else if !allowed.is_match(p) {
                    Some(PathViolation { path: p.to_owned(), reason: "not in allowed_paths" })
                } else {
                    None
                }
            })
            .collect())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathViolation {
    pub path: String,
    pub reason: &'static str,
}

/// Decisions the mandate governs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    AcceptAttempt,
    MergeProtected,
    ChangeMandate,
    RunHostedAction,
}

impl std::fmt::Display for Decision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::AcceptAttempt => "accept_attempt",
            Self::MergeProtected => "merge_protected",
            Self::ChangeMandate => "change_mandate",
            Self::RunHostedAction => "run_hosted_action",
        })
    }
}

/// Who may make a decision: anyone (agent or human), or a human with at
/// least the given role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approver {
    Any,
    Reviewer,
    Maintainer,
    Owner,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approvals {
    pub accept_attempt: Approver,
    pub merge_protected: Approver,
    pub change_mandate: Approver,
    pub run_hosted_action: Approver,
}

impl Approvals {
    pub fn for_autonomy(autonomy: Autonomy) -> Self {
        use Approver::*;
        match autonomy {
            Autonomy::Supervised => Self {
                accept_attempt: Reviewer,
                merge_protected: Maintainer,
                change_mandate: Owner,
                run_hosted_action: Maintainer,
            },
            Autonomy::Assisted => Self {
                accept_attempt: Any,
                merge_protected: Maintainer,
                change_mandate: Owner,
                run_hosted_action: Maintainer,
            },
            Autonomy::Autonomous => Self {
                accept_attempt: Any,
                merge_protected: Any,
                change_mandate: Owner,
                run_hosted_action: Any,
            },
        }
    }

    pub fn get(&self, decision: Decision) -> Approver {
        match decision {
            Decision::AcceptAttempt => self.accept_attempt,
            Decision::MergeProtected => self.merge_protected,
            Decision::ChangeMandate => self.change_mandate,
            Decision::RunHostedAction => self.run_hosted_action,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Authorization {
    Allowed,
    /// An agent asked; a human with this role must decide.
    NeedsHuman {
        role: Role,
    },
    Denied {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerConfig {
    pub activity_branch: String,
    pub logs_branch: String,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self { activity_branch: "gitbots/activity".into(), logs_branch: "gitbots/logs".into() }
    }
}

/// Where workrooms live on disk is machine-local (git config
/// `gitbots.workrooms`), not part of the committed manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkroomConfig {
    pub branch_prefix: String,
}

impl Default for WorkroomConfig {
    fn default() -> Self {
        Self { branch_prefix: "gitbots/attempt".into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{SessionId, Ulid};
    use crate::identity::AgentDescriptor;

    fn mandate(autonomy: Autonomy) -> Mandate {
        let mut m = Mandate::new(
            autonomy,
            Principal { handle: "gstohl".into(), email: Some("d@x.com".into()), role: Role::Owner },
        );
        m.principals.push(Principal {
            handle: "santosh".into(),
            email: None,
            role: Role::Reviewer,
        });
        m
    }

    fn agent() -> Actor {
        Actor::Agent {
            session: SessionId::from_ulid(Ulid::from_parts(1, 1)),
            agent: AgentDescriptor::new("openai", "gpt-5-codex", "codex"),
            parent: None,
        }
    }

    #[test]
    fn supervised_needs_humans() {
        let m = mandate(Autonomy::Supervised);
        assert_eq!(
            m.authorize(&agent(), Decision::AcceptAttempt),
            Authorization::NeedsHuman { role: Role::Reviewer }
        );
        assert_eq!(
            m.authorize(&Actor::human("santosh", None), Decision::AcceptAttempt),
            Authorization::Allowed
        );
        assert!(matches!(
            m.authorize(&Actor::human("santosh", None), Decision::MergeProtected),
            Authorization::Denied { .. }
        ));
        assert!(matches!(
            m.authorize(&Actor::human("stranger", None), Decision::AcceptAttempt),
            Authorization::Denied { .. }
        ));
        // Email wins over handle.
        assert_eq!(
            m.authorize(&Actor::human("whatever", Some("D@X.com".into())), Decision::ChangeMandate),
            Authorization::Allowed
        );
    }

    #[test]
    fn assisted_lets_agents_accept_but_not_merge_protected() {
        let m = mandate(Autonomy::Assisted);
        assert_eq!(m.authorize(&agent(), Decision::AcceptAttempt), Authorization::Allowed);
        assert_eq!(
            m.authorize(&agent(), Decision::MergeProtected),
            Authorization::NeedsHuman { role: Role::Maintainer }
        );
    }

    #[test]
    fn path_rules() {
        let rules = AgentRules::default();
        let v = rules
            .violations([
                "src/main.rs",
                ".Gitbots/Manifest.json",
                ".github/workflows/ci.yml",
                "a/b/c",
                "docs/AGENTS.md",
                "AGENTS.md",
                "pkg/.claude/settings.json",
            ])
            .unwrap();
        let paths: Vec<_> = v.iter().map(|v| v.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                ".Gitbots/Manifest.json",
                ".github/workflows/ci.yml",
                "docs/AGENTS.md",
                "AGENTS.md",
                "pkg/.claude/settings.json"
            ]
        );

        let only_src = AgentRules { allowed_paths: vec!["src/**".into()], ..AgentRules::default() };
        let v = only_src.violations(["src/x.rs", "README.md"]).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].reason, "not in allowed_paths");
    }

    #[test]
    fn protected_branches() {
        let m = mandate(Autonomy::Assisted);
        assert!(m.is_protected_branch("main"));
        assert!(m.is_protected_branch("refs/heads/main"));
        assert!(!m.is_protected_branch("gitbots/attempt/x"));
    }

    #[test]
    fn manifest_json_roundtrip() {
        let m = Manifest::new(
            ProjectMeta {
                id: ProjectId::from_ulid(Ulid::from_parts(1, 2)),
                name: "demo".into(),
                description: None,
            },
            Owner { kind: OwnerKind::User, handle: "gstohl".into() },
            mandate(Autonomy::Assisted),
        );
        m.validate().unwrap();
        let json = serde_json::to_string_pretty(&m).unwrap();
        assert!(json.contains("\"accept_attempt\": \"any\""));
        let back: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }
}

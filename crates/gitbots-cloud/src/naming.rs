//! Artifacts repo names, git remote URLs and owner keys.
//!
//! The Worker owns the project → repo mapping (`docs/CLOUD.md`):
//!
//! | repo            | name                                   |
//! |-----------------|----------------------------------------|
//! | `<prj>`         | the project id lowercased: `prj_01k…`  |
//! | `<prj>-logs`    | `prj_01k…-logs`                        |
//! | `<prj>-<att>`   | `prj_01k…-<attempt short id>`          |
//!
//! Artifacts repo names match `^[a-zA-Z0-9][a-zA-Z0-9._-]*$`, so a
//! lowercased `prj_<ULID>` is already valid.

use gitbots_core::{AttemptId, ProjectId};
use sha2::{Digest, Sha256};

pub const LOGS_SUFFIX: &str = "-logs";
/// Prefix of owner keys, so they are recognizable in logs and secret scanners.
pub const OWNER_KEY_PREFIX: &str = "gitbots_ok_";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("`{0}` is not a project id (expected `prj_<ULID>`)")]
    Project(String),
    #[error("`{0}` is not a valid Artifacts repo name")]
    Repo(String),
}

/// Artifacts repo name rule: `^[a-zA-Z0-9][a-zA-Z0-9._-]*$`.
pub fn is_valid_repo_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `<prj>`: the code repo of a project.
pub fn project_repo(project_id: &str) -> Result<String, NameError> {
    let id: ProjectId =
        project_id.parse().map_err(|_| NameError::Project(project_id.to_owned()))?;
    let name = id.as_str().to_ascii_lowercase();
    if is_valid_repo_name(&name) { Ok(name) } else { Err(NameError::Repo(name)) }
}

/// `<prj>-logs`.
pub fn logs_repo(main: &str) -> String {
    format!("{main}{LOGS_SUFFIX}")
}

/// `<prj>-<attempt short>`: the fork for one hosted attempt.
pub fn fork_repo(main: &str, attempt: &AttemptId) -> String {
    format!("{main}-{}", attempt.short())
}

/// `https://<ACCOUNT_ID>.artifacts.cloudflare.net/git/<namespace>/<repo>.git`.
pub fn remote_url(account_id: &str, namespace: &str, repo: &str) -> String {
    format!("https://{account_id}.artifacts.cloudflare.net/git/{namespace}/{repo}.git")
}

/// A new owner key from 32 random bytes (the caller supplies randomness).
pub fn owner_key(random: &[u8; 32]) -> String {
    format!("{OWNER_KEY_PREFIX}{}", hex(random))
}

/// What D1 stores instead of an owner key: lowercase hex SHA-256.
pub fn key_hash(key: &str) -> String {
    hex(&Sha256::digest(key.as_bytes()))
}

/// Compares secrets (or their hashes) without an early exit.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The token of an `Authorization: Bearer <token>` header value.
pub fn bearer(header: &str) -> Option<&str> {
    let (scheme, token) = header.trim().split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use gitbots_core::Ulid;

    use super::*;

    #[test]
    fn repo_names() {
        let main = project_repo("prj_01K6M3C2V7QZ8Y9X0W1T2S3R4P").unwrap();
        assert_eq!(main, "prj_01k6m3c2v7qz8y9x0w1t2s3r4p");
        assert_eq!(logs_repo(&main), "prj_01k6m3c2v7qz8y9x0w1t2s3r4p-logs");
        let att = AttemptId::from_ulid(Ulid::from_parts(1, 0xABCDEF));
        let fork = fork_repo(&main, &att);
        assert!(fork.starts_with(&format!("{main}-")) && fork.len() == main.len() + 7);
        for name in [&main, &logs_repo(&main), &fork] {
            assert!(is_valid_repo_name(name), "{name}");
        }
        assert!(project_repo("tsk_01K6M3C2V7QZ8Y9X0W1T2S3R4P").is_err());
        assert!(project_repo("prj_nope").is_err());
        assert!(!is_valid_repo_name("-x") && !is_valid_repo_name("") && !is_valid_repo_name("a/b"));
    }

    #[test]
    fn keys() {
        let key = owner_key(&[7; 32]);
        assert!(key.starts_with(OWNER_KEY_PREFIX) && key.len() == OWNER_KEY_PREFIX.len() + 64);
        assert_eq!(
            key_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(constant_time_eq(b"abc", b"abc") && !constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert_eq!(bearer("Bearer  tok "), Some("tok"));
        assert_eq!(bearer("bearer tok"), Some("tok"));
        assert_eq!(bearer("Basic tok"), None);
        assert_eq!(bearer("Bearer "), None);
        assert_eq!(
            remote_url("acc", "gitbots-dev", "prj_x"),
            "https://acc.artifacts.cloudflare.net/git/gitbots-dev/prj_x.git"
        );
    }
}

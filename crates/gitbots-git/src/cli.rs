//! Running the `git` CLI.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};

/// Variables git exports to hooks to locate the repository. gitbots always names
/// the directory explicitly, so an inherited value (gitbots running inside a
/// hook of another worktree) must not redirect the command.
const LOCATION_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_PREFIX",
    "GIT_OBJECT_DIRECTORY",
];

pub(crate) fn command<I, S>(dir: &Path, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir).stdin(Stdio::null()).env("LC_ALL", "C");
    for var in LOCATION_VARS {
        cmd.env_remove(var);
    }
    #[cfg(test)]
    crate::tests::isolate(&mut cmd);
    cmd
}

/// Runs a command and returns its output whatever the exit status.
pub(crate) fn output(mut cmd: Command) -> Result<Output> {
    cmd.output().with_context(|| format!("failed to run {}; is git installed?", describe(&cmd)))
}

/// Runs a command and returns its raw stdout; a non-zero exit is an error
/// that carries stderr.
pub(crate) fn run(cmd: Command) -> Result<Vec<u8>> {
    let what = describe(&cmd);
    let out = output(cmd)?;
    if !out.status.success() {
        bail!("{what} failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

pub(crate) fn run_in<I, S>(dir: &Path, args: I) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run(command(dir, args))
}

/// Stdout as text with trailing whitespace removed. Leading whitespace is
/// kept so column formats (`status --porcelain`) survive.
pub(crate) fn text(stdout: Vec<u8>) -> String {
    let mut s = String::from_utf8_lossy(&stdout).into_owned();
    s.truncate(s.trim_end().len());
    s
}

/// NUL-separated fields of `-z` output, empty fields dropped.
pub(crate) fn nul_fields(out: &[u8]) -> impl Iterator<Item = String> + '_ {
    out.split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
}

/// The command line for error messages, with the values of secret `-c`
/// config (`http.extraHeader`, which carries bearer tokens) redacted.
fn describe(cmd: &Command) -> String {
    let mut args = Vec::new();
    let mut config_value = false;
    for arg in cmd.get_args().map(|a| a.to_string_lossy()) {
        let shown = match arg.split_once('=') {
            Some((key, _)) if config_value && is_secret_config(key) => format!("{key}=<redacted>"),
            _ => arg.clone().into_owned(),
        };
        config_value = arg == "-c";
        args.push(shown);
    }
    format!("`git {}`", args.join(" "))
}

/// Config keys whose values must never be shown.
fn is_secret_config(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == "http.extraheader" || (key.starts_with("http.") && key.ends_with(".extraheader"))
}

/// Fails unless the installed git is at least `major.minor`.
pub(crate) fn require_version(major: u32, minor: u32, why: &str) -> Result<()> {
    static VERSION: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    let found = *VERSION.get_or_init(|| {
        let out = run_in(&std::env::temp_dir(), ["version"]).ok()?;
        parse_version(&String::from_utf8_lossy(&out))
    });
    match found {
        Some(v) if v >= (major, minor) => Ok(()),
        Some((a, b)) => bail!("{why} needs git >= {major}.{minor}, found {a}.{b}"),
        None => bail!("{why} needs git >= {major}.{minor}, and `git version` failed"),
    }
}

/// `git version 2.50.1 (Apple Git-155)` -> `(2, 50)`.
fn parse_version(s: &str) -> Option<(u32, u32)> {
    let v = s.split_whitespace().nth(2)?;
    let mut parts = v.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("git version 2.50.1 (Apple Git-155)"), Some((2, 50)));
        assert_eq!(parse_version("git version 2.38.0.windows.1"), Some((2, 38)));
        assert_eq!(parse_version("nope"), None);
        require_version(2, 38, "the test suite").unwrap();
    }

    #[test]
    fn describe_redacts_auth_headers() {
        let mut cmd = Command::new("git");
        cmd.args(["-c", "http.extraHeader=Authorization: Bearer art_v1_secret"]);
        cmd.args(["-c", "http.https://x.example/.extraheader=Authorization: Bearer art_v1_two"]);
        cmd.args(["-c", "user.name=gitbots", "push", "a=b"]);
        let shown = describe(&cmd);
        assert!(!shown.contains("art_v1"), "{shown}");
        assert!(shown.contains("http.extraHeader=<redacted>"), "{shown}");
        assert!(shown.contains("user.name=gitbots") && shown.contains("push a=b"), "{shown}");
    }
}

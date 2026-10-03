//! Git hooks that attribute commits to agents.
//!
//! The hooks fail open: if gitbots is missing or errors, git carries on as if
//! they did not exist. They never overwrite a hook gitbots did not install.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::Repo;

/// Hooks gitbots installs. Each runs `gitbots hook <name> "$@"`.
pub const HOOKS: &[&str] = &["prepare-commit-msg", "post-commit"];

/// Marks a hook as gitbots's, so re-installing may replace it.
pub const HOOK_MARKER: &str = "# installed by gitbots";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookReport {
    /// The hooks directory used (`core.hooksPath` or `<common_dir>/hooks`).
    pub dir: PathBuf,
    pub installed: Vec<String>,
    /// `(hook, reason)` for hooks left alone.
    pub skipped: Vec<(String, String)>,
}

impl Repo {
    /// `core.hooksPath` if set (relative to the worktree, as git runs
    /// hooks), else `<common_dir>/hooks`.
    pub fn hooks_dir(&self) -> Result<PathBuf> {
        let dir = PathBuf::from(self.git(&["rev-parse", "--git-path", "hooks"])?);
        Ok(if dir.is_absolute() {
            dir
        } else {
            self.workdir().unwrap_or_else(|| self.git_dir()).join(dir)
        })
    }

    /// Installs `prepare-commit-msg` and `post-commit`. Idempotent; a
    /// foreign hook of the same name is reported in `skipped`, untouched.
    pub fn install_hooks(&self, gitbots_bin: &Path) -> Result<HookReport> {
        let dir = self.hooks_dir()?;
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let gitbots = std::path::absolute(gitbots_bin)?;
        let gitbots = gitbots
            .to_str()
            .with_context(|| format!("gitbots path {} is not UTF-8", gitbots.display()))?;
        let mut report = HookReport { dir: dir.clone(), ..HookReport::default() };
        for hook in HOOKS {
            let path = dir.join(hook);
            let script = script(hook, gitbots);
            match std::fs::read(&path) {
                Ok(existing) if existing == script.as_bytes() => {}
                Ok(existing) if !String::from_utf8_lossy(&existing).contains(HOOK_MARKER) => {
                    let reason =
                        format!("{} exists and was not installed by gitbots", path.display());
                    report.skipped.push((hook.to_string(), reason));
                    continue;
                }
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("reading {}", path.display()));
                }
                _ => std::fs::write(&path, &script)
                    .with_context(|| format!("writing {}", path.display()))?,
            }
            make_executable(&path)?;
            report.installed.push(hook.to_string());
        }
        Ok(report)
    }
}

/// A POSIX sh hook that finds gitbots (`$GITBOTS_BIN`, then the path baked in at
/// install time, then `gitbots` on `PATH`) and never fails the git command.
fn script(hook: &str, gitbots: &str) -> String {
    format!(
        r#"#!/bin/sh
{HOOK_MARKER}
# Attributes commits to agents. Fails open: if gitbots is missing or fails,
# git carries on as if this hook did not exist.
find_gitbots() {{
  for c in "${{GITBOTS_BIN:-}}" {gitbots}; do
    case $c in
      '') ;;
      */*) if [ -x "$c" ] && [ ! -d "$c" ]; then printf '%s\n' "$c"; return 0; fi ;;
      *) if command -v "$c" >/dev/null 2>&1; then command -v "$c"; return 0; fi ;;
    esac
  done
  command -v gitbots 2>/dev/null
}}
GITBOTS=$(find_gitbots) || exit 0
[ -n "$GITBOTS" ] || exit 0
"$GITBOTS" hook {hook} "$@" || true
exit 0
"#,
        gitbots = sh_quote(gitbots),
    )
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("chmod {}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> Result<()> {
    Ok(())
}

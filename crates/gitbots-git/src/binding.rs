//! Bindings: tiny per-worktree files (`<git_dir>/gitbots/<key>`) that bind a
//! workroom to a session or attempt, so an agent's shell is attributed even
//! when it does not keep environment variables between calls.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

pub fn write_binding(git_dir: &Path, key: &str, value: &str) -> Result<()> {
    write_atomic(&binding_path(git_dir, key)?, format!("{}\n", value.trim()).as_bytes())
}

/// The trimmed value; `None` if unbound or empty.
pub fn read_binding(git_dir: &Path, key: &str) -> Result<Option<String>> {
    let path = binding_path(git_dir, key)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text.trim().to_owned()).filter(|v| !v.is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn remove_binding(git_dir: &Path, key: &str) -> Result<()> {
    let path = binding_path(git_dir, key)?;
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}

fn binding_path(git_dir: &Path, key: &str) -> Result<PathBuf> {
    // `tips` is taken: in the main worktree `<git_dir>/gitbots` also holds the
    // ledger's remembered tips (`<common_dir>/gitbots/tips`).
    if key.is_empty() || key == "tips" || key.starts_with('.') || key.contains(['/', '\\', '\0']) {
        bail!("invalid binding key {key:?}");
    }
    Ok(git_dir.join("gitbots").join(key))
}

/// Writes via a unique temp file and a rename, so readers never see a torn
/// file and concurrent writers never interleave.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().context("path has no file name")?.to_string_lossy();
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming to {}", path.display()))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

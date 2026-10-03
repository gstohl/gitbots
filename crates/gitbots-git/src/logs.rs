//! The `gitbots/logs` ledger: raw logs (action output, transcripts, traces).

use std::ops::Deref;

use anyhow::Result;
use gitbots_core::action::LogRef;
use gitbots_core::ledger::{LedgerKind, MAX_LOG_BYTES};
use gitbots_core::redact::redact;

use crate::Repo;
use crate::ledger::Ledger;

/// The logs ledger. Derefs to [`Ledger`] for `ensure`, `tip`, `read`, ...
#[derive(Clone, Debug)]
pub struct Logs<'r>(pub Ledger<'r>);

impl<'r> Logs<'r> {
    pub fn new(repo: &'r Repo, branch_short: &str) -> Self {
        Logs(Ledger::new(repo, branch_short, LedgerKind::Logs))
    }

    /// Stores one log. Text (valid UTF-8) is redacted; anything over
    /// [`MAX_LOG_BYTES`] is cut with a marker line.
    pub fn put(&self, path: &str, bytes: &[u8]) -> Result<LogRef> {
        let mut refs = self.put_many(vec![(path.to_owned(), bytes.to_vec())])?;
        Ok(refs.remove(0))
    }

    /// Stores several logs in one commit.
    pub fn put_many(&self, items: Vec<(String, Vec<u8>)>) -> Result<Vec<LogRef>> {
        if items.is_empty() {
            return Ok(vec![]);
        }
        let refs: Vec<LogRef> = items
            .iter()
            .map(|(path, _)| LogRef { branch: self.0.branch().to_owned(), path: path.clone() })
            .collect();
        let message = match refs.as_slice() {
            [one] => format!("log: {}", one.path),
            many => format!("{} logs", many.len()),
        };
        let files =
            items.into_iter().map(|(path, bytes)| (path, prepare(bytes, MAX_LOG_BYTES))).collect();
        self.0.write(files, &message)?;
        Ok(refs)
    }
}

impl<'r> Deref for Logs<'r> {
    type Target = Ledger<'r>;

    fn deref(&self) -> &Ledger<'r> {
        &self.0
    }
}

/// Redacts text and caps the size at `max` bytes.
pub(crate) fn prepare(bytes: Vec<u8>, max: usize) -> Vec<u8> {
    let bytes = match String::from_utf8(bytes) {
        Ok(text) => redact(&text).0.into_owned().into_bytes(),
        Err(binary) => binary.into_bytes(),
    };
    cap(bytes, max)
}

/// Keeps the head of an oversized log and ends it with a marker line, never
/// splitting a UTF-8 character.
fn cap(mut bytes: Vec<u8>, max: usize) -> Vec<u8> {
    if bytes.len() <= max {
        return bytes;
    }
    let marker = format!("\n[gitbots: log truncated to {max} of {} bytes]\n", bytes.len());
    let mut keep = max.saturating_sub(marker.len());
    while keep > 0 && bytes[keep] & 0xC0 == 0x80 {
        keep -= 1;
    }
    bytes.truncate(keep);
    bytes.extend_from_slice(marker.as_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_with_marker_on_char_boundary() {
        let text = "é".repeat(100).into_bytes(); // 200 bytes
        let out = prepare(text, 80);
        assert!(out.len() <= 80, "{}", out.len());
        let out = String::from_utf8(out).expect("still UTF-8");
        assert!(out.ends_with("[gitbots: log truncated to 80 of 200 bytes]\n"), "{out}");
        assert!(out.starts_with("éé"));

        assert_eq!(prepare(b"short".to_vec(), 80), b"short");
        let binary = vec![0xFF; 100];
        let out = prepare(binary, 60);
        assert!(out.len() <= 60 && out.starts_with(&[0xFF]));
    }
}

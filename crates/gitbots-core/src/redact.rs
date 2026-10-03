//! Best-effort secret redaction for anything written to a ledger.
//!
//! Ledger branches are append-only and union-merged, so a leaked secret can't
//! be removed by rewriting one clone. Redact before writing.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

const MASK: &str = "[REDACTED]";

static PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        // Whole private key blocks.
        (r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----", MASK),
        // Vendor token shapes.
        (r"\bsk-(?:ant-|proj-)?[A-Za-z0-9_\-]{20,}", MASK),
        (r"\bgh[pousr]_[A-Za-z0-9]{36,}", MASK),
        (r"\bgithub_pat_[A-Za-z0-9_]{22,}", MASK),
        (r"\bAKIA[0-9A-Z]{16}\b", MASK),
        (r"\bAIza[0-9A-Za-z_\-]{35}", MASK),
        (r"\bxox[abprs]-[A-Za-z0-9-]{10,}", MASK),
        // `Authorization: Bearer <token>`: keep the scheme.
        (r"(?i)\b(bearer)\s+[A-Za-z0-9._~+/=\-]{16,}", "$1 [REDACTED]"),
        // `FOO_TOKEN=...`, `password: ...`: keep the key.
        (
            r#"(?i)\b([A-Z0-9_\-]*(?:secret|token|passwd|password|api_?key)[A-Z0-9_\-]*)(\s*[=:]\s*)["']?[^\s"']{8,}["']?"#,
            "$1$2[REDACTED]",
        ),
    ]
    .into_iter()
    .map(|(re, rep)| (Regex::new(re).expect("valid redaction pattern"), rep))
    .collect()
});

/// Returns the redacted text and how many secrets were masked.
pub fn redact(text: &str) -> (Cow<'_, str>, usize) {
    let mut out = Cow::Borrowed(text);
    let mut hits = 0;
    for (re, rep) in PATTERNS.iter() {
        let n = re.find_iter(&out).count();
        if n > 0 {
            hits += n;
            out = Cow::Owned(re.replace_all(&out, *rep).into_owned());
        }
    }
    (out, hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_common_secrets() {
        let input = "export ANTHROPIC_API_KEY=sk-ant-api03-abcdefghijklmnopqrstuvwxyz\n\
                     curl -H 'Authorization: Bearer abcdefghijklmnop1234'\n\
                     token ghp_0123456789abcdefghijklmnopqrstuvwxyzAB\n\
                     password: hunter2hunter2\n\
                     cargo test passed";
        let (out, hits) = redact(input);
        assert!(hits >= 4, "{out}");
        assert!(!out.contains("sk-ant-api03"));
        assert!(!out.contains("abcdefghijklmnop1234"));
        assert!(!out.contains("ghp_0123"));
        assert!(!out.contains("hunter2"));
        assert!(out.contains("Bearer [REDACTED]"));
        assert!(out.contains("cargo test passed"));
    }

    #[test]
    fn leaves_clean_text_borrowed() {
        let (out, hits) = redact("nothing to see");
        assert_eq!(hits, 0);
        assert!(matches!(out, Cow::Borrowed(_)));
    }
}

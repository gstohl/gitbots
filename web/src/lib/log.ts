// Helpers for the job log viewer.

// CSI sequences (colors, cursor moves), OSC sequences (titles, hyperlinks) and
// lone two-byte escapes. Logs are redacted plain text; we only strip styling.
// eslint-disable-next-line no-control-regex
const ANSI_RE = /\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]/g;

export function stripAnsi(s: string): string {
  return s.replace(ANSI_RE, "");
}

/** Splits a log into display lines: ANSI stripped, `\r\n` normalized, progress-bar `\r` rewrites collapsed. */
export function logLines(raw: string): string[] {
  const text = stripAnsi(raw).replace(/\r\n/g, "\n");
  const lines = text.split("\n");
  if (lines.length && lines[lines.length - 1] === "") lines.pop();
  return lines.map((l) => {
    const cr = l.lastIndexOf("\r");
    return cr >= 0 ? l.slice(cr + 1) : l;
  });
}

// Whole words only, so `thiserror` or `error_handling.rs` don't count. Also
// gitbots's own step footer for a failed step: `== exit 1 (4 ms)`.
const ERROR_RE = /\b(?:error|Error|ERROR)\b|\bFAILED\b|\bpanicked at\b|^== exit (?!0\b)\d+|^== timed out/;

/** Index of the first line that looks like a failure (`error`, `FAILED`), or -1. */
export function firstErrorLine(lines: string[]): number {
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i]!;
    // Skip summary lines like "0 errors".
    if (ERROR_RE.test(l) && !/\b0 errors?\b/i.test(l)) return i;
  }
  return -1;
}

/** Indices of lines containing `q` (case-insensitive). */
export function searchLines(lines: string[], q: string): number[] {
  if (!q) return [];
  const needle = q.toLowerCase();
  const out: number[] = [];
  for (let i = 0; i < lines.length; i++) if (lines[i]!.toLowerCase().includes(needle)) out.push(i);
  return out;
}

export function isErrorLine(l: string): boolean {
  return ERROR_RE.test(l) && !/\b0 errors?\b/i.test(l);
}

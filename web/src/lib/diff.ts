// A small parser for `git diff` unified output (also accepts plain `diff -u`).
// Line counts in the hunk headers drive parsing, so content lines that look
// like headers (`--- a/x` inside a hunk) are handled correctly.

export type DiffLineType = "add" | "del" | "ctx" | "meta";
export type DiffLine = { type: DiffLineType; text: string; oldNo: number | null; newNo: number | null };
export type DiffHunk = {
  header: string;
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  /** Text after the closing `@@`, usually the enclosing function. */
  section: string;
  lines: DiffLine[];
};
export type FileStatus = "added" | "deleted" | "modified" | "renamed" | "copied";
export type DiffFile = {
  oldPath: string | null;
  newPath: string | null;
  status: FileStatus;
  binary: boolean;
  oldMode?: string;
  newMode?: string;
  similarity?: number;
  hunks: DiffHunk[];
  additions: number;
  deletions: number;
};

const HUNK_RE = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@ ?(.*)$/;

/** Path shown for a file: the new path, else the old one. */
export function filePath(f: DiffFile): string {
  return f.newPath ?? f.oldPath ?? "(unknown)";
}

/** Total number of rendered lines in a file (for collapse decisions). */
export function fileLineCount(f: DiffFile): number {
  let n = 0;
  for (const h of f.hunks) n += h.lines.length + 1;
  return n;
}

/** Undo git's C-style quoting of unusual paths (`"a/tab\there"`). */
export function unquotePath(p: string): string {
  if (!(p.length >= 2 && p.startsWith('"') && p.endsWith('"'))) return p;
  const body = p.slice(1, -1);
  const bytes: number[] = [];
  const te = new TextEncoder();
  for (let i = 0; i < body.length; i++) {
    const c = body[i]!;
    if (c !== "\\") {
      bytes.push(...te.encode(c));
      continue;
    }
    const n = body[++i];
    if (n === undefined) break;
    if (/[0-7]/.test(n)) {
      const oct = body.slice(i, i + 3);
      bytes.push(parseInt(oct, 8));
      i += 2;
    } else {
      const map: Record<string, string> = { n: "\n", t: "\t", r: "\r", '"': '"', "\\": "\\", a: "\x07", b: "\b", f: "\f", v: "\v" };
      bytes.push(...te.encode(map[n] ?? n));
    }
  }
  return new TextDecoder().decode(new Uint8Array(bytes));
}

/** `a/foo` -> `foo`, `/dev/null` -> null. Also drops a trailing tab+timestamp (`diff -u`). */
function cleanPath(raw: string): string | null {
  let p = raw.replace(/\t.*$/, "");
  p = unquotePath(p);
  if (p === "/dev/null") return null;
  if (/^[abciwo12]\//.test(p)) p = p.slice(2);
  return p;
}

/** Split `a/X b/Y` from a `diff --git` line. Exact when both paths are equal. */
function splitGitPaths(rest: string): [string | null, string | null] {
  if (rest.startsWith('"')) {
    const end = rest.indexOf('"', 1);
    const a = rest.slice(0, end + 1);
    const b = rest.slice(end + 2);
    return [cleanPath(a), cleanPath(b)];
  }
  // Same path on both sides (the common case): "a/P b/P", length 2|P|+5.
  if ((rest.length - 5) % 2 === 0) {
    const half = (rest.length - 5) / 2;
    const a = rest.slice(2, 2 + half);
    const b = rest.slice(half + 5);
    if (a === b && rest.startsWith("a/") && rest.slice(half + 2, half + 5) === " b/") return [a, b];
  }
  const m = /^(.+?) (b\/.+|"b\/.+")$/.exec(rest);
  if (m) return [cleanPath(m[1]!), cleanPath(m[2]!)];
  return [null, null];
}

function newFile(): DiffFile {
  return { oldPath: null, newPath: null, status: "modified", binary: false, hunks: [], additions: 0, deletions: 0 };
}

export function parseUnifiedDiff(text: string): DiffFile[] {
  const files: DiffFile[] = [];
  if (!text) return files;
  const lines = text.split("\n");
  if (lines.length && lines[lines.length - 1] === "") lines.pop();

  let file: DiffFile | null = null;
  let hunk: DiffHunk | null = null;
  let oldRem = 0;
  let newRem = 0;
  let oldNo = 0;
  let newNo = 0;
  // Whether the current file's header saw `diff --git` (then `---` belongs to it).
  let inGitHeader = false;

  const startFile = () => {
    file = newFile();
    files.push(file);
    hunk = null;
    oldRem = newRem = 0;
    return file;
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!;

    // Inside a hunk: the header's line counts say what is content.
    if (hunk && (oldRem > 0 || newRem > 0)) {
      const h: DiffHunk = hunk;
      const f = file as DiffFile | null;
      const c = line[0];
      if (c === "+" && newRem > 0) {
        h.lines.push({ type: "add", text: line.slice(1), oldNo: null, newNo: newNo++ });
        newRem--;
        if (f) f.additions++;
        continue;
      }
      if (c === "-" && oldRem > 0) {
        h.lines.push({ type: "del", text: line.slice(1), oldNo: oldNo++, newNo: null });
        oldRem--;
        if (f) f.deletions++;
        continue;
      }
      if ((c === " " || line === "") && oldRem > 0 && newRem > 0) {
        h.lines.push({ type: "ctx", text: line.slice(1), oldNo: oldNo++, newNo: newNo++ });
        oldRem--;
        newRem--;
        continue;
      }
      if (c === "\\") {
        h.lines.push({ type: "meta", text: line.slice(2), oldNo: null, newNo: null });
        continue;
      }
      // Malformed or truncated hunk: fall through and treat as a header line.
      oldRem = newRem = 0;
    }

    if (line.startsWith("\\") && hunk) {
      (hunk as DiffHunk).lines.push({ type: "meta", text: line.slice(2), oldNo: null, newNo: null });
      continue;
    }

    if (line.startsWith("diff --git ")) {
      const f = startFile();
      inGitHeader = true;
      const [a, b] = splitGitPaths(line.slice("diff --git ".length));
      f.oldPath = a;
      f.newPath = b;
      continue;
    }

    const m = HUNK_RE.exec(line);
    if (m) {
      const f = file ?? startFile();
      inGitHeader = false;
      const h: DiffHunk = {
        header: line,
        oldStart: Number(m[1]),
        oldLines: m[2] === undefined ? 1 : Number(m[2]),
        newStart: Number(m[3]),
        newLines: m[4] === undefined ? 1 : Number(m[4]),
        section: m[5] ?? "",
        lines: [],
      };
      f.hunks.push(h);
      hunk = h;
      oldRem = h.oldLines;
      newRem = h.newLines;
      oldNo = h.oldStart;
      newNo = h.newStart;
      continue;
    }

    if (line.startsWith("--- ")) {
      // A `---` outside a git header starts a new file (plain `diff -u`).
      const f = inGitHeader && file ? (file as DiffFile) : startFile();
      inGitHeader = true;
      f.oldPath = cleanPath(line.slice(4));
      if (f.oldPath === null && f.status === "modified") f.status = "added";
      continue;
    }
    if (line.startsWith("+++ ") && file) {
      const f = file as DiffFile;
      f.newPath = cleanPath(line.slice(4));
      if (f.newPath === null) f.status = "deleted";
      continue;
    }

    if (!file || !inGitHeader) continue; // preamble or trailing noise
    const f = file as DiffFile;
    if (line.startsWith("new file mode ")) {
      f.status = "added";
      f.newMode = line.slice(14);
      f.oldPath = null;
    } else if (line.startsWith("deleted file mode ")) {
      f.status = "deleted";
      f.oldMode = line.slice(18);
      f.newPath = null;
    } else if (line.startsWith("old mode ")) {
      f.oldMode = line.slice(9);
    } else if (line.startsWith("new mode ")) {
      f.newMode = line.slice(9);
    } else if (line.startsWith("similarity index ")) {
      f.similarity = parseInt(line.slice(17), 10);
    } else if (line.startsWith("rename from ")) {
      f.status = "renamed";
      f.oldPath = unquotePath(line.slice(12));
    } else if (line.startsWith("rename to ")) {
      f.status = "renamed";
      f.newPath = unquotePath(line.slice(10));
    } else if (line.startsWith("copy from ")) {
      f.status = "copied";
      f.oldPath = unquotePath(line.slice(10));
    } else if (line.startsWith("copy to ")) {
      f.status = "copied";
      f.newPath = unquotePath(line.slice(8));
    } else if (line.startsWith("Binary files ") || line === "GIT binary patch") {
      f.binary = true;
    }
  }
  return files;
}

export function diffTotals(files: DiffFile[]): { files: number; additions: number; deletions: number } {
  let additions = 0;
  let deletions = 0;
  for (const f of files) {
    additions += f.additions;
    deletions += f.deletions;
  }
  return { files: files.length, additions, deletions };
}

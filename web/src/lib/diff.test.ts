import { describe, expect, it } from "vitest";
import { bigUpgradeDiff, DIFF_RATE_LIMIT } from "../mocks/diffs";
import { diffTotals, fileLineCount, filePath, parseUnifiedDiff, unquotePath } from "./diff";

const MODIFIED = `diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,5 @@ mod tests {
 use std::fmt;
-use std::io;
+use std::io::{self, Write};
+use std::sync::Arc;
 
 fn main() {
@@ -20,3 +21,2 @@ fn helper() {
 let a = 1;
-let b = 2;
 let c = 3;
`;

describe("parseUnifiedDiff", () => {
  it("parses a modified file with two hunks and line numbers", () => {
    const [f, ...rest] = parseUnifiedDiff(MODIFIED);
    expect(rest).toHaveLength(0);
    expect(f!.status).toBe("modified");
    expect(f!.oldPath).toBe("src/lib.rs");
    expect(f!.newPath).toBe("src/lib.rs");
    expect(f!.hunks).toHaveLength(2);
    expect(f!.additions).toBe(2);
    expect(f!.deletions).toBe(2);
    const h = f!.hunks[0]!;
    expect(h.section).toBe("mod tests {");
    expect(h.lines.map((l) => l.type)).toEqual(["ctx", "del", "add", "add", "ctx", "ctx"]);
    expect(h.lines[0]).toMatchObject({ oldNo: 1, newNo: 1, text: "use std::fmt;" });
    expect(h.lines[1]).toMatchObject({ type: "del", oldNo: 2, newNo: null });
    expect(h.lines[2]).toMatchObject({ type: "add", oldNo: null, newNo: 2 });
    expect(h.lines[3]).toMatchObject({ type: "add", newNo: 3 });
    // A context line whose leading space was stripped still counts.
    expect(h.lines[4]).toMatchObject({ type: "ctx", oldNo: 3, newNo: 4, text: "" });
    const h2 = f!.hunks[1]!;
    expect(h2.oldStart).toBe(20);
    expect(h2.newStart).toBe(21);
    expect(h2.lines[2]).toMatchObject({ type: "ctx", oldNo: 22, newNo: 22 });
  });

  it("handles added, deleted, renamed and binary files", () => {
    const text = `diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..e69de29
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
diff --git a/old.txt b/old.txt
deleted file mode 100755
index e69de29..0000000
--- a/old.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/src/util/time.rs b/src/clock.rs
similarity index 94%
rename from src/util/time.rs
rename to src/clock.rs
index 3333333..4444444 100644
--- a/src/util/time.rs
+++ b/src/clock.rs
@@ -1 +1 @@
-//! old
+//! new
diff --git a/pure-rename.md b/moved.md
similarity index 100%
rename from pure-rename.md
rename to moved.md
diff --git a/img.png b/img.png
new file mode 100644
index 0000000..abcdef0
Binary files /dev/null and b/img.png differ
diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
`;
    const files = parseUnifiedDiff(text);
    expect(files.map((f) => f.status)).toEqual(["added", "deleted", "renamed", "renamed", "added", "modified"]);
    expect(files[0]!.oldPath).toBeNull();
    expect(files[0]!.additions).toBe(2);
    expect(files[1]!.newPath).toBeNull();
    expect(files[1]!.oldMode).toBe("100755");
    expect(files[1]!.hunks[0]!.oldLines).toBe(1); // "@@ -1 +0,0 @@": count defaults to 1
    expect(files[1]!.deletions).toBe(1);
    expect(files[2]).toMatchObject({ oldPath: "src/util/time.rs", newPath: "src/clock.rs", similarity: 94 });
    expect(files[3]).toMatchObject({ oldPath: "pure-rename.md", newPath: "moved.md", hunks: [] });
    expect(files[4]).toMatchObject({ binary: true, newPath: "img.png" });
    expect(files[5]).toMatchObject({ oldMode: "100644", newMode: "100755", hunks: [] });
    expect(filePath(files[1]!)).toBe("old.txt");
  });

  it("treats header-looking content inside a hunk as content", () => {
    const text = `diff --git a/notes.md b/notes.md
index 1..2 100644
--- a/notes.md
+++ b/notes.md
@@ -1,3 +1,3 @@
 # Notes
---- a/fake
+++++ b/fake
 end
`;
    const [f] = parseUnifiedDiff(text);
    expect(f!.hunks[0]!.lines.map((l) => [l.type, l.text])).toEqual([
      ["ctx", "# Notes"],
      ["del", "--- a/fake"],
      ["add", "++++ b/fake"],
      ["ctx", "end"],
    ]);
  });

  it("keeps the no-newline marker as a meta line", () => {
    const text = `diff --git a/a b/a
--- a/a
+++ b/a
@@ -1 +1 @@
-x
\\ No newline at end of file
+y
\\ No newline at end of file
`;
    const [f] = parseUnifiedDiff(text);
    expect(f!.hunks[0]!.lines.map((l) => l.type)).toEqual(["del", "meta", "add", "meta"]);
    expect(f!.hunks[0]!.lines[1]!.text).toBe("No newline at end of file");
    expect(f!.additions).toBe(1);
  });

  it("parses plain `diff -u` output with timestamps and several files", () => {
    const text = `--- a/one.txt\t2026-10-03 10:00:00
+++ b/one.txt\t2026-10-03 10:01:00
@@ -1 +1 @@
-a
+b
--- a/two.txt
+++ b/two.txt
@@ -1,2 +1,2 @@
 same
-old
+new
`;
    const files = parseUnifiedDiff(text);
    expect(files.map(filePath)).toEqual(["one.txt", "two.txt"]);
    expect(diffTotals(files)).toEqual({ files: 2, additions: 2, deletions: 2 });
  });

  it("unquotes C-style quoted paths", () => {
    expect(unquotePath('"a/tab\\there"')).toBe("a/tab\there");
    expect(unquotePath('"caf\\303\\251.txt"')).toBe("café.txt");
    const [f] = parseUnifiedDiff(`diff --git "a/with space.txt" "b/with space.txt"
new file mode 100644
--- /dev/null
+++ "b/with space.txt"
@@ -0,0 +1 @@
+x
`);
    expect(f!.newPath).toBe("with space.txt");
  });

  it("splits `diff --git` paths that contain spaces", () => {
    const [f] = parseUnifiedDiff(`diff --git a/my file.txt b/my file.txt
old mode 100644
new mode 100755
`);
    expect(f!.oldPath).toBe("my file.txt");
    expect(f!.newPath).toBe("my file.txt");
  });

  it("returns nothing for empty or non-diff input", () => {
    expect(parseUnifiedDiff("")).toEqual([]);
    expect(parseUnifiedDiff("hello\nworld\n")).toEqual([]);
  });

  it("parses the mock fixtures consistently", () => {
    const files = parseUnifiedDiff(DIFF_RATE_LIMIT);
    expect(files.length).toBe(9);
    expect(files.find((f) => f.binary)?.newPath).toBe("docs/img/rate-limit-burst.png");
    expect(files.find((f) => f.status === "renamed")?.oldPath).toBe("src/util/time.rs");
    // Every hunk's line counts match its header.
    for (const f of files)
      for (const h of f.hunks) {
        expect(h.lines.filter((l) => l.type !== "add" && l.type !== "meta").length).toBe(h.oldLines);
        expect(h.lines.filter((l) => l.type !== "del" && l.type !== "meta").length).toBe(h.newLines);
      }
  });

  it("parses a ~5,000-line diff quickly", () => {
    const text = bigUpgradeDiff();
    expect(text.split("\n").length).toBeGreaterThan(5000);
    const t0 = performance.now();
    const files = parseUnifiedDiff(text);
    const ms = performance.now() - t0;
    expect(files.length).toBe(61);
    expect(files.reduce((n, f) => n + fileLineCount(f), 0)).toBeGreaterThan(5000);
    expect(ms).toBeLessThan(250);
  });
});

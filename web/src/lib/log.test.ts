import { describe, expect, it } from "vitest";
import { failingTestLog } from "../mocks/logs";
import { firstErrorLine, logLines, searchLines, stripAnsi } from "./log";

describe("log helpers", () => {
  it("strips ANSI colors and OSC sequences", () => {
    expect(stripAnsi("\x1b[1m\x1b[32m   Compiling\x1b[0m foo")).toBe("   Compiling foo");
    expect(stripAnsi("\x1b]8;;https://x\x07link\x1b]8;;\x07")).toBe("link");
  });

  it("splits lines, normalizes CRLF and collapses carriage-return rewrites", () => {
    expect(logLines("a\r\nb\rprogress 50%\rprogress 100%\nc\n")).toEqual(["a", "progress 100%", "c"]);
  });

  it("finds the first failure, ignoring `thiserror` and `0 errors`", () => {
    const lines = ["Compiling thiserror v2", "src/error_handling.rs ok", "0 errors", "test x ... FAILED", "error: boom"];
    expect(firstErrorLine(lines)).toBe(3);
    expect(firstErrorLine(["all good"])).toBe(-1);
    const real = logLines(failingTestLog());
    const i = firstErrorLine(real);
    expect(real[i]).toContain("retries_on_503 ... FAILED");
  });

  it("understands gitbots's step footers", () => {
    const lines = ["== step: no TODOs", "$ ! grep -rn TODO src/", "src/auth.py:2: # TODO", "== exit 1 (4 ms)"];
    expect(firstErrorLine(lines)).toBe(3);
    expect(firstErrorLine(["== step: x", "== exit 0 (4 ms)"])).toBe(-1);
  });

  it("searches case-insensitively", () => {
    expect(searchLines(["Alpha", "beta", "ALPHABET"], "alpha")).toEqual([0, 2]);
    expect(searchLines(["x"], "")).toEqual([]);
  });
});

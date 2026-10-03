// Captures real responses from a running `gitbots ui` into src/mocks/real/, so
// the contract tests (src/mocks/real/real.test.ts) check the frontend's types
// against what the server actually sends.
//
//   npm run snapshot:real -- 'http://127.0.0.1:7799/#token=…'
//
// Needs Node 22.18+ (runs TypeScript directly). Read-only: only GETs.

import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const link = process.argv[2];
if (!link || !link.includes("#token=")) {
  console.error("usage: npm run snapshot:real -- 'http://127.0.0.1:7799/#token=…'");
  process.exit(2);
}
const url = new URL(link);
const token = new URLSearchParams(url.hash.slice(1)).get("token") ?? "";
const base = url.origin;
const out = join(dirname(fileURLToPath(import.meta.url)), "..", "src", "mocks", "real");

async function get(path: string): Promise<Response> {
  const res = await fetch(base + path, { headers: { Authorization: `Bearer ${token}` } });
  if (!res.ok) throw new Error(`${path}: HTTP ${res.status} ${await res.text()}`);
  return res;
}
const json = async (path: string): Promise<unknown> => (await get(path)).json();
const text = async (path: string): Promise<string> => (await get(path)).text();

function write(name: string, body: unknown) {
  const s = typeof body === "string" ? body : `${JSON.stringify(body, null, 2)}\n`;
  writeFileSync(join(out, name), s);
  console.log(`wrote ${name}`);
}

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });

for (const name of ["project", "tip", "board", "inbox", "events", "stats", "workflows", "recipes"]) {
  write(`${name}.json`, await json(`/api/${name}`));
}

type Board = { attempts: { id: string; state: string }[] };
const board = (await json("/api/board")) as Board;
// One attempt detail per state (the most informative variety).
const seen = new Set<string>();
const details: { id: string; runs: { jobs: { log?: { path: string } }[] }[] }[] = [];
for (const a of board.attempts) {
  if (seen.has(a.state)) continue;
  seen.add(a.state);
  const d = (await json(`/api/attempts/${encodeURIComponent(a.id)}`)) as (typeof details)[number];
  details.push(d);
  write(`attempt-${a.state}.json`, d);
}
const withDiff = board.attempts.find((a) => a.state === "submitted") ?? board.attempts[0];
if (withDiff) write("attempt.diff", await text(`/api/attempts/${encodeURIComponent(withDiff.id)}/diff`));
const log = details.flatMap((d) => d.runs.flatMap((r) => r.jobs)).find((j) => j.log)?.log;
if (log) write("job.log", await text(`/api/logs?path=${encodeURIComponent(log.path)}`));
console.log(`done: ${out}`);

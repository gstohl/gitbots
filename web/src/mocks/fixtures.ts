// Realistic mock data for `npm run dev:mock` and the component tests.
// Everything here is typed against src/api/types.ts, so a contract change
// that breaks the fixtures fails `npm run typecheck`.
//
// The story: two founders (@gstohl, @santosh) supervise Claude Code (opus,
// plus haiku subagents) and Codex on a billing API. Times are relative to
// `now` so the UI always looks fresh.

import type {
  ActionRun,
  Actor,
  AgentDescriptor,
  AttemptState,
  BoardAttempt,
  BoardTask,
  CommitInfo,
  DiffStat,
  Event,
  JobResult,
  ProjectInfo,
  RecipesResponse,
  ReportView,
  RunStatus,
  Session,
  SessionView,
  Stats,
  EventDataMap,
  KnownKind,
  TaskStatus,
  Via,
  WorkflowsResponse,
} from "../api/types";
import { diffTotals, parseUnifiedDiff } from "../lib/diff";
import { bigUpgradeDiff, DIFF_CI_CACHE, DIFF_RATE_LIMIT, DIFF_WEBHOOK, smallDiff } from "./diffs";
import { idAt, sha, stableId } from "./ids";
import { clippyLog, failingTestLog, fmtLog, passingTestLog, timeoutLog } from "./logs";

export type AttemptExtras = {
  commits: CommitInfo[];
  runs: ActionRun[];
  violations: { path: string; reason: string }[];
  workroom: string | null;
};

export type MockWorld = {
  project: ProjectInfo;
  sessions: SessionView[];
  tasks: BoardTask[];
  attempts: BoardAttempt[];
  events: Event[];
  stats: Stats;
  reports: ReportView[];
  extras: Record<string, AttemptExtras>;
  diffs: Record<string, string>;
  logs: Record<string, string>;
  workflows: WorkflowsResponse;
  recipes: RecipesResponse;
  orphans: number;
};

// ---- identities ----------------------------------------------------------------

export const OPUS: AgentDescriptor = { provider: "anthropic", model: "claude-opus-5-5", client: "claude-code", client_version: "2.4.1" };
export const HAIKU: AgentDescriptor = { provider: "anthropic", model: "claude-haiku-4-5", client: "claude-code", client_version: "2.4.1" };
export const CODEX: AgentDescriptor = { provider: "openai", model: "gpt-5-codex", client: "codex", client_version: "0.71.0" };
export const CHATGPT: AgentDescriptor = { provider: "openai", model: "gpt-5", client: "chatgpt" };

export const IDS = {
  project: stableId("prj", "billing"),
  sOpus: stableId("ses", "opus-main"),
  sCodex: stableId("ses", "codex-main"),
  sHaikuTests: stableId("ses", "haiku-tests"),
  sHaikuCi: stableId("ses", "haiku-ci"),
  sOpusOld: stableId("ses", "opus-pdf"),
  sCodexOld: stableId("ses", "codex-axum"),
  sChatgpt: stableId("ses", "chatgpt-planner"),
  tRate: stableId("tsk", "rate-limit"),
  tFlaky: stableId("tsk", "flaky-webhook"),
  tPdf: stableId("tsk", "pdf-stream"),
  tAxum: stableId("tsk", "axum-upgrade"),
  tDocs: stableId("tsk", "webhook-docs"),
  tLatency: stableId("tsk", "latency"),
  tV0: stableId("tsk", "remove-v0"),
  tIdem: stableId("tsk", "idempotency"),
  tCi: stableId("tsk", "ci-cache"),
  tCreds: stableId("tsk", "rotate-creds"),
  aRate: stableId("att", "rate-limit-opus"),
  aRateOld: stableId("att", "rate-limit-codex"),
  aFlaky: stableId("att", "flaky-codex"),
  aPdf: stableId("att", "pdf-opus"),
  aAxum: stableId("att", "axum-codex"),
  aLatency: stableId("att", "latency-opus"),
  aV0: stableId("att", "v0-opus"),
  aIdem: stableId("att", "idempotency-haiku"),
  aCi: stableId("att", "ci-haiku"),
};

const gstohl: Actor = { type: "human", handle: "gstohl", email: "dominik@gstohl.com" };
const santosh: Actor = { type: "human", handle: "santosh", email: "santosh@gitbots.dev" };
const actions: Actor = { type: "system", component: "actions" };
const hooks: Actor = { type: "system", component: "hooks" };

export function buildWorld(now: number = Date.now()): MockWorld {
  const at = (minAgo: number) => new Date(now - minAgo * 60_000).toISOString();
  const ms = (minAgo: number) => now - minAgo * 60_000;

  // ---- sessions -----------------------------------------------------------------
  const sess = (s: Session, ended: boolean): SessionView => ({ session: s, ended });
  const sOpus: Session = {
    id: IDS.sOpus,
    agent: OPUS,
    role: "implementer",
    operator: "gstohl",
    external_id: "7c1e4b2a-93f0-4d51-a8e6-0b9f2c6d41aa",
    label: "API hardening",
    started_at: at(310),
  };
  const sCodex: Session = {
    id: IDS.sCodex,
    agent: CODEX,
    role: "implementer",
    operator: "santosh",
    external_id: "codex-run-5512",
    label: "tests & flakes",
    started_at: at(245),
  };
  const sHaikuTests: Session = {
    id: IDS.sHaikuTests,
    agent: HAIKU,
    parent: IDS.sOpus,
    role: "test-writer",
    operator: "gstohl",
    label: "idempotency keys",
    started_at: at(205),
  };
  const sHaikuCi: Session = {
    id: IDS.sHaikuCi,
    agent: HAIKU,
    parent: IDS.sOpus,
    role: "ci-helper",
    operator: "gstohl",
    label: "CI cache",
    started_at: at(185),
  };
  const sOpusOld: Session = {
    id: IDS.sOpusOld,
    agent: OPUS,
    role: "implementer",
    operator: "gstohl",
    label: "PDF streaming",
    started_at: at(1640),
  };
  const sCodexOld: Session = {
    id: IDS.sCodexOld,
    agent: CODEX,
    role: "implementer",
    operator: "santosh",
    label: "axum 0.9",
    started_at: at(2950),
  };
  const sChatgpt: Session = {
    id: IDS.sChatgpt,
    agent: CHATGPT,
    role: "planner",
    operator: "santosh",
    external_id: "chatcmpl-conv-88d1",
    started_at: at(95),
  };
  const sessions: SessionView[] = [
    sess(sCodexOld, true),
    sess(sOpusOld, true),
    sess(sOpus, false),
    sess(sCodex, false),
    sess(sHaikuTests, true),
    sess(sHaikuCi, false),
    sess(sChatgpt, true),
  ];
  const agentOf = (s: Session): Actor =>
    s.parent ? { type: "agent", session: s.id, agent: s.agent, parent: s.parent } : { type: "agent", session: s.id, agent: s.agent };

  // ---- runs -----------------------------------------------------------------------
  const today = new Date(now).toISOString().slice(0, 10).replace(/-/g, "/");
  const logs: Record<string, string> = {};
  const job = (run: string, name: string, status: RunStatus, duration_ms: number, extra: Partial<JobResult> = {}, log?: string): JobResult => {
    const path = `runs/${today}/${run}/${name}.log`;
    const j: JobResult = { name, status, duration_ms, ...extra };
    if (log !== undefined) {
      logs[path] = log;
      j.log = { branch: "gitbots/logs", path };
    }
    return j;
  };
  const mkRun = (seed: string, attempt: string, commit: string, status: RunStatus, jobs: (run: string) => JobResult[]): ActionRun => {
    const run = stableId("run", seed);
    const js = jobs(run);
    return {
      run,
      workflow: "ci",
      trigger: "attempt.submitted",
      attempt,
      commit,
      runner: "local",
      status,
      duration_ms: js.reduce((a, j) => a + j.duration_ms, 0),
      jobs: js,
    };
  };

  // ---- heads and diffs ------------------------------------------------------------
  const head = (seed: string) => sha(`head-${seed}`);
  const diffs: Record<string, string> = {
    [IDS.aRate]: DIFF_RATE_LIMIT,
    [IDS.aFlaky]: DIFF_WEBHOOK,
    [IDS.aAxum]: bigUpgradeDiff(),
    [IDS.aCi]: DIFF_CI_CACHE,
    [IDS.aPdf]: smallDiff("invoices/pdf"),
    [IDS.aRateOld]: smallDiff("middleware/global_limit"),
    [IDS.aIdem]: smallDiff("refunds/idempotency"),
    [IDS.aLatency]: smallDiff("charges/query"),
    [IDS.aV0]: smallDiff("routes/v0"),
  };
  const stat = (id: string): DiffStat => {
    const t = diffTotals(parseUnifiedDiff(diffs[id] ?? ""));
    return { files: t.files, insertions: t.additions, deletions: t.deletions };
  };

  const runRate = mkRun("rate-ci", IDS.aRate, head("rate"), "success", (r) => [
    job(r, "fmt", "success", 2_140, { exit_code: 0 }, fmtLog()),
    job(r, "clippy", "success", 41_020, { exit_code: 0 }, clippyLog()),
    job(r, "test", "success", 63_870, { exit_code: 0 }, passingTestLog()),
  ]);
  const runFlaky = mkRun("flaky-ci", IDS.aFlaky, head("flaky"), "failure", (r) => [
    job(r, "fmt", "success", 1_980, { exit_code: 0 }, fmtLog()),
    job(r, "test", "failure", 75_410, { exit_code: 101, failed_step: "test" }, failingTestLog()),
    job(r, "clippy", "skipped", 0),
  ]);
  const runRateOld = mkRun("rate-old-ci", IDS.aRateOld, head("rate-old"), "failure", (r) => [
    job(r, "fmt", "success", 2_020, { exit_code: 0 }, fmtLog()),
    job(r, "test", "failure", 69_100, { exit_code: 101, failed_step: "test" }, failingTestLog()),
  ]);
  const runPdf = mkRun("pdf-ci", IDS.aPdf, head("pdf"), "success", (r) => [
    job(r, "fmt", "success", 2_310, { exit_code: 0 }, fmtLog()),
    job(r, "test", "success", 59_400, { exit_code: 0 }, passingTestLog()),
  ]);
  const runAxum = mkRun("axum-ci", IDS.aAxum, head("axum"), "success", (r) => [
    job(r, "fmt", "success", 2_200, { exit_code: 0 }, fmtLog()),
    job(r, "clippy", "success", 48_900, { exit_code: 0 }, clippyLog()),
    job(r, "test", "success", 71_300, { exit_code: 0 }, passingTestLog()),
  ]);
  const runCi = mkRun("ci-ci", IDS.aCi, head("ci"), "timed_out", (r) => [
    job(r, "test", "timed_out", 900_000, { failed_step: "test" }, timeoutLog()),
  ]);

  const summary = (r: ActionRun) => ({ run: r.run, workflow: r.workflow, status: r.status, commit: r.commit ?? null });

  // ---- attempts -------------------------------------------------------------------
  const branch = (slug: string, id: string) => `gitbots/attempt/${slug}-${id.slice(-6).toLowerCase()}`;
  const baseCommit = sha("main-base");

  type A = Omit<BoardAttempt, "checks_passed" | "diff"> & { diff?: DiffStat | null };
  const mk = (a: A): BoardAttempt => {
    const runs = a.runs;
    const checks_passed = runs.length === 0 ? null : runs.every((r) => r.status === "success");
    return { ...a, diff: a.diff === undefined ? stat(a.id) : a.diff, checks_passed };
  };

  const attempts: BoardAttempt[] = [
    mk({
      id: IDS.aAxum,
      task: IDS.tAxum,
      branch: branch("upgrade-axum-to-0-9", IDS.aAxum),
      base: "main",
      base_commit: sha("main-axum"),
      session: IDS.sCodexOld,
      started_by: agentOf(sCodexOld),
      started_at: at(2930),
      state: "merged",
      head: head("axum"),
      summary: "Bump axum 0.8 → 0.9: State<AppState> by value, typed path params, regenerate Cargo.lock.",
      submitted_by: agentOf(sCodexOld),
      holder: null,
      runs: [summary(runAxum)],
      review: { decision: "accept", reason: "Mechanical, checks green. Merging.", by: santosh, at: at(2010) },
      merged_commit: sha("merge-axum"),
      updated_at: at(2008),
    }),
    mk({
      id: IDS.aPdf,
      task: IDS.tPdf,
      branch: branch("stream-invoice-pdfs", IDS.aPdf),
      base: "main",
      base_commit: sha("main-pdf"),
      session: IDS.sOpusOld,
      started_by: agentOf(sOpusOld),
      started_at: at(1620),
      state: "accepted",
      head: head("pdf"),
      summary: "Render invoices page by page into a streaming body; peak RSS for a 2,000-line invoice drops from 410 MB to 38 MB.",
      submitted_by: agentOf(sOpusOld),
      holder: null,
      runs: [summary(runPdf)],
      review: { decision: "accept", reason: "Nice memory win. Will merge after the release freeze.", by: santosh, at: at(905) },
      merged_commit: null,
      updated_at: at(905),
    }),
    mk({
      id: IDS.aV0,
      task: IDS.tV0,
      branch: branch("remove-deprecated-v0", IDS.aV0),
      base: "main",
      base_commit: sha("main-v0"),
      session: IDS.sOpusOld,
      started_by: agentOf(sOpusOld),
      started_at: at(1310),
      state: "abandoned",
      head: null,
      summary: null,
      diff: null,
      submitted_by: null,
      holder: null,
      runs: [],
      review: null,
      merged_commit: null,
      updated_at: at(1280),
    }),
    mk({
      id: IDS.aRateOld,
      task: IDS.tRate,
      branch: branch("add-token-bucket-rate-limiting", IDS.aRateOld),
      base: "main",
      base_commit: sha("main-rate-old"),
      session: IDS.sCodexOld,
      started_by: agentOf(sCodexOld),
      started_at: at(1200),
      state: "rejected",
      head: head("rate-old"),
      summary: "Global limiter behind a Mutex<HashMap>; 429 on overflow.",
      submitted_by: agentOf(sCodexOld),
      holder: null,
      runs: [summary(runRateOld)],
      review: {
        decision: "reject",
        reason: "A global mutex serializes every request. Needs per-key buckets without a global lock.",
        by: gstohl,
        at: at(1150),
      },
      merged_commit: null,
      updated_at: at(1150),
    }),
    mk({
      id: IDS.aRate,
      task: IDS.tRate,
      branch: branch("add-token-bucket-rate-limiting", IDS.aRate),
      base: "main",
      base_commit: baseCommit,
      session: IDS.sOpus,
      started_by: agentOf(sOpus),
      started_at: at(290),
      state: "submitted",
      head: head("rate"),
      summary:
        "Per-key token buckets in a sharded DashMap (no global lock); 429 with Retry-After; config via RATE_LIMIT_*; janitor evicts idle buckets. 4 new tests.",
      submitted_by: agentOf(sOpus),
      holder: null,
      runs: [summary(runRate)],
      review: null,
      merged_commit: null,
      updated_at: at(36),
    }),
    mk({
      id: IDS.aCi,
      task: IDS.tCi,
      branch: branch("cache-cargo-registry-in-ci", IDS.aCi),
      base: "main",
      base_commit: baseCommit,
      session: IDS.sHaikuCi,
      started_by: agentOf(sHaikuCi),
      started_at: at(180),
      state: "changes_requested",
      head: head("ci"),
      summary: "Cache ~/.cargo in GitHub Actions and raise the local CI timeout to 30 min.",
      submitted_by: agentOf(sHaikuCi),
      holder: null,
      runs: [summary(runCi)],
      review: {
        decision: "changes_requested",
        reason: "Don't edit .github/** or .gitbots/** from a workroom. Propose the workflow change in a report instead.",
        by: gstohl,
        at: at(120),
      },
      merged_commit: null,
      updated_at: at(120),
    }),
    mk({
      id: IDS.aLatency,
      task: IDS.tLatency,
      branch: branch("investigate-p99-latency", IDS.aLatency),
      base: "main",
      base_commit: baseCommit,
      session: IDS.sOpus,
      started_by: agentOf(sOpus),
      started_at: at(140),
      state: "active",
      head: head("latency"),
      summary: null,
      diff: null,
      submitted_by: null,
      holder: { type: "human", handle: "santosh" },
      runs: [],
      review: null,
      merged_commit: null,
      updated_at: at(48),
    }),
    mk({
      id: IDS.aFlaky,
      task: IDS.tFlaky,
      branch: branch("fix-flaky-webhook-retry-test", IDS.aFlaky),
      base: "main",
      base_commit: baseCommit,
      session: IDS.sCodex,
      started_by: agentOf(sCodex),
      started_at: at(230),
      state: "submitted",
      head: head("flaky"),
      summary: "Replace sleep-based waits with a paused tokio clock and dedupe retry scheduling on duplicate 5xx.",
      submitted_by: agentOf(sCodex),
      holder: null,
      runs: [summary(runFlaky)],
      review: null,
      merged_commit: null,
      updated_at: at(13),
    }),
    mk({
      id: IDS.aIdem,
      task: IDS.tIdem,
      branch: branch("idempotency-keys-for-refunds", IDS.aIdem),
      base: "release/2026.10",
      base_commit: sha("release-base"),
      session: IDS.sHaikuTests,
      started_by: agentOf(sHaikuTests),
      started_at: at(200),
      state: "submitted",
      head: head("idem"),
      summary: "Idempotency-Key header on POST /v1/refunds, stored in Redis for 24h; replays return the original response.",
      submitted_by: agentOf(sHaikuTests),
      holder: null,
      runs: [],
      review: null,
      merged_commit: null,
      updated_at: at(4),
    }),
  ];

  // ---- tasks ----------------------------------------------------------------------
  const task = (
    id: string,
    title: string,
    status: TaskStatus,
    created_by: Actor,
    minAgo: number,
    labels: string[],
    body: string | null,
    recipe: string | null = null,
  ): BoardTask => ({
    id,
    title,
    body,
    recipe,
    labels,
    created_by,
    created_at: at(minAgo),
    attempts: attempts.filter((a) => a.task === id).map((a) => a.id),
    status,
  });
  const tasks: BoardTask[] = [
    task(IDS.tAxum, "Upgrade axum to 0.9", "done", santosh, 2960, ["deps"], "Follow the 0.9 migration guide. No behavior changes."),
    task(IDS.tPdf, "Stream invoice PDFs instead of buffering in memory", "accepted", gstohl, 1650, ["perf", "invoices"], "Large invoices OOM the pod. Stream page by page."),
    task(IDS.tV0, "Remove deprecated v0 endpoints", "open", gstohl, 1320, ["api", "cleanup"], "Delete /v0/* routes and their tests."),
    task(
      IDS.tRate,
      "Add token-bucket rate limiting to the public API",
      "in_progress",
      gstohl,
      1220,
      ["api", "reliability"],
      "Per API key. Burst 60, 10 req/s. Answer 429 with Retry-After. Must not add a global lock on the hot path.",
    ),
    task(IDS.tFlaky, "Fix flaky test in webhook retry suite", "in_progress", santosh, 250, ["tests", "flaky"], "`retries_on_503` fails ~1 in 8 runs on CI."),
    task(IDS.tIdem, "Add idempotency keys to POST /v1/refunds", "in_progress", gstohl, 215, ["api", "payments"], null, "idempotency-key@1"),
    task(IDS.tCi, "Cache the cargo registry in CI", "in_progress", santosh, 195, ["ci"], null),
    task(IDS.tLatency, "Investigate p99 latency regression on /v1/charges", "in_progress", santosh, 150, ["perf", "incident"], "p99 went from 180 ms to 640 ms after Tuesday's deploy."),
    task(IDS.tDocs, "Document the webhook signature scheme", "open", gstohl, 62, ["docs"], "Public docs page: header format, HMAC, replay window, examples in curl and Python."),
    task(IDS.tCreds, "Rotate staging database credentials", "open", santosh, 20, ["ops", "security"], null),
  ];

  // ---- reports ----------------------------------------------------------------------
  const reports: ReportView[] = [
    {
      id: idAt("evt", ms(47), "r-blocker"),
      at: at(47),
      by: agentOf(sOpus),
      report: {
        title: "Blocked: need read access to the prod latency dashboards",
        body:
          "The p99 regression doesn't reproduce on staging (p99 190 ms). I need read-only Grafana access to `billing-prod` or an export of the /v1/charges traces from Tuesday 14:00–16:00 UTC. Handed the attempt to @santosh until then.",
        level: "blocker",
        task: IDS.tLatency,
        attempt: IDS.aLatency,
      },
    },
    {
      id: idAt("evt", ms(14), "r-warning"),
      at: at(14),
      by: agentOf(sCodex),
      report: {
        title: "retries_on_503 still fails on CI with the paused clock",
        body: "The scheduler spawns onto a separate runtime in tests, so `tokio::time::advance` doesn't reach it. Submitted anyway so you can see the approach; fix is to inject the runtime handle.",
        level: "warning",
        attempt: IDS.aFlaky,
      },
    },
    {
      id: idAt("evt", ms(35), "r-info-rate"),
      at: at(35),
      by: agentOf(sOpus),
      report: {
        title: "Rate limiter ready for review",
        body: "Bench (wrk, 64 conns, 1 key): 41.2k req/s before, 40.7k after (−1.2%). 10k distinct keys: 39.9k req/s, 2.1 MB of buckets.",
        level: "info",
        task: IDS.tRate,
        attempt: IDS.aRate,
      },
    },
    {
      id: idAt("evt", ms(88), "r-info-chatgpt"),
      at: at(88),
      by: agentOf(sChatgpt),
      report: {
        title: "Proposed plan for the webhook docs",
        body: "1. Header format and HMAC-SHA256 over `timestamp.body`. 2. 5-minute replay window. 3. Verification snippets (curl, Python, Node). 4. Key rotation.",
        level: "info",
        task: IDS.tDocs,
      },
    },
  ];

  // ---- commits ------------------------------------------------------------------------
  const trailers = (s: Session): [string, string][] => {
    const t: [string, string][] = [
      ["Gitbots-Session", s.id],
      ["Gitbots-Provider", s.agent.provider],
      ["Gitbots-Model", s.agent.model],
      ["Gitbots-Client", s.agent.client],
    ];
    if (s.parent) t.push(["Gitbots-Parent-Session", s.parent]);
    if (s.agent.provider === "anthropic") t.unshift(["Co-Authored-By", `Claude <noreply@anthropic.com>`]);
    return t;
  };
  const commit = (seed: string, subject: string, s: Session): CommitInfo => ({ sha: sha(`c-${seed}`), subject, trailers: trailers(s) });

  const commits: Record<string, CommitInfo[]> = {
    [IDS.aRate]: [
      commit("rate-1", "Add RateLimitConfig with burst and per-second refill", sOpus),
      commit("rate-2", "Token-bucket limiter keyed by API key (DashMap)", sOpus),
      commit("rate-3", "Wire RateLimitLayer into the router; 429 + Retry-After", sOpus),
      commit("rate-4", "Move util/time.rs to clock.rs; tests for the limiter", sOpus),
      commit("rate-5", "docs: rate limits page", sOpus),
    ],
    [IDS.aFlaky]: [
      commit("flaky-1", "Dedupe retry scheduling on duplicate 5xx", sCodex),
      commit("flaky-2", "tests: paused clock for retries_on_503", sCodex),
    ],
    [IDS.aIdem]: [
      commit("idem-1", "Idempotency-Key middleware for refunds", sHaikuTests),
      commit("idem-2", "Store responses in Redis for 24h", sHaikuTests),
      commit("idem-3", "tests: replayed refund returns the original body", sHaikuTests),
    ],
    [IDS.aCi]: [commit("ci-1", "ci: cache cargo registry; raise timeout", sHaikuCi)],
    [IDS.aLatency]: [commit("lat-1", "wip: add tracing spans around charge queries", sOpus)],
    [IDS.aPdf]: [
      commit("pdf-1", "Stream PDF pages into the response body", sOpusOld),
      commit("pdf-2", "Bound the page buffer; test with a 2,000-line invoice", sOpusOld),
    ],
    [IDS.aAxum]: [
      commit("axum-1", "Bump axum to 0.9 and regenerate Cargo.lock", sCodexOld),
      commit("axum-2", "Pass AppState by value; typed path params", sCodexOld),
    ],
    [IDS.aRateOld]: [commit("rate-old-1", "Global rate limiter", sCodexOld)],
    [IDS.aV0]: [],
  };

  const extras: Record<string, AttemptExtras> = {};
  const runsBy: Record<string, ActionRun[]> = {
    [IDS.aRate]: [runRate],
    [IDS.aFlaky]: [runFlaky],
    [IDS.aRateOld]: [runRateOld],
    [IDS.aPdf]: [runPdf],
    [IDS.aAxum]: [runAxum],
    [IDS.aCi]: [runCi],
  };
  for (const a of attempts) {
    extras[a.id] = {
      commits: commits[a.id] ?? [],
      runs: runsBy[a.id] ?? [],
      violations:
        a.id === IDS.aCi
          ? [
              { path: ".github/workflows/ci.yml", reason: "denied by .github/**" },
              { path: ".gitbots/actions/ci.toml", reason: "denied by .gitbots/**" },
            ]
          : [],
      workroom: a.state === "active" || a.state === "submitted" || a.state === "changes_requested"
        ? `~/.local/share/gitbots/workrooms/${IDS.project}/${a.branch.split("/").pop()}`
        : null,
    };
  }

  // ---- events (the ledger) ---------------------------------------------------------------
  const events: Event[] = [];
  let n = 0;
  const ev = <K extends KnownKind>(
    minAgo: number,
    actor: Actor,
    via: Via | undefined,
    body: { kind: K; data: EventDataMap[K] },
    extra: Partial<Event> = {},
  ) => {
    const e: Event = {
      v: 1,
      id: idAt("evt", ms(minAgo), `e${n++}`),
      ts: at(minAgo),
      actor,
      producer: "gitbots/0.1.0",
      kind: body.kind,
      data: body.data,
      ...extra,
    };
    if (via) e.via = via;
    events.push(e);
    return e;
  };
  const viaOf = (s: Session): Via => (s.agent.client === "chatgpt" ? "mcp" : s.parent ? "env" : "worktree");

  ev(4320, gstohl, "tty", { kind: "project.initialized", data: { project: IDS.project, name: "billing" } });
  // Old codex session: axum upgrade, merged.
  ev(2950, agentOf(sCodexOld), "env", { kind: "session.started", data: { session: sCodexOld } });
  ev(2960, santosh, "tty", { kind: "task.created", data: { task: IDS.tAxum, title: "Upgrade axum to 0.9", labels: ["deps"] } });
  ev(2930, agentOf(sCodexOld), "env", {
    kind: "attempt.started",
    data: { task: IDS.tAxum, attempt: IDS.aAxum, branch: attempts[0]!.branch, base: "main", base_commit: sha("main-axum"), session: IDS.sCodexOld },
  });
  for (const [i, c] of (commits[IDS.aAxum] ?? []).entries()) {
    ev(2900 - i * 20, agentOf(sCodexOld), "worktree", { kind: "commit.recorded", data: { sha: c.sha, subject: c.subject, branch: attempts[0]!.branch, attempt: IDS.aAxum, diff: { files: 31, insertions: 1260 + i * 40, deletions: 1180 } } }, { idem: `commit:${c.sha}` });
  }
  const subAxum = ev(2860, agentOf(sCodexOld), "worktree", { kind: "attempt.submitted", data: { attempt: IDS.aAxum, head: head("axum"), summary: attempts[0]!.summary ?? undefined, diff: attempts[0]!.diff ?? undefined } });
  ev(2856, actions, "system", { kind: "action.completed", data: runAxum });
  ev(2010, santosh, "ui", { kind: "review.decided", data: { attempt: IDS.aAxum, decision: "accept", reason: "Mechanical, checks green. Merging.", mandate: sha("manifest").slice(0, 40) } }, { on: subAxum.id });
  ev(2008, santosh, "ui", { kind: "attempt.merged", data: { attempt: IDS.aAxum, into: "main", commit: sha("merge-axum"), source_commits: (commits[IDS.aAxum] ?? []).map((c) => c.sha) } });
  ev(1990, agentOf(sCodexOld), "env", { kind: "session.ended", data: { session: IDS.sCodexOld, summary: "axum 0.9 merged." } });

  // Old opus session: PDF streaming (accepted), v0 removal (abandoned).
  ev(1650, gstohl, "tty", { kind: "task.created", data: { task: IDS.tPdf, title: "Stream invoice PDFs instead of buffering in memory", labels: ["perf", "invoices"] } });
  ev(1640, agentOf(sOpusOld), "env", { kind: "session.started", data: { session: sOpusOld } });
  ev(1620, agentOf(sOpusOld), "env", { kind: "attempt.started", data: { task: IDS.tPdf, attempt: IDS.aPdf, branch: attempts[1]!.branch, base: "main", base_commit: sha("main-pdf"), session: IDS.sOpusOld } });
  ev(1500, agentOf(sOpusOld), "worktree", { kind: "tool.called", data: { tool: "Bash", input: "cargo test -p billing invoices::pdf", ok: true, duration_ms: 48_200 } });
  ev(1480, agentOf(sOpusOld), "worktree", { kind: "commit.recorded", data: { sha: sha("c-pdf-1"), subject: "Stream PDF pages into the response body", attempt: IDS.aPdf, diff: { files: 3, insertions: 88, deletions: 41 } } });
  ev(1460, agentOf(sOpusOld), "worktree", { kind: "commit.recorded", data: { sha: sha("c-pdf-2"), subject: "Bound the page buffer; test with a 2,000-line invoice", attempt: IDS.aPdf, diff: { files: 2, insertions: 64, deletions: 3 } } });
  const subPdf = ev(1100, agentOf(sOpusOld), "worktree", { kind: "attempt.submitted", data: { attempt: IDS.aPdf, head: head("pdf"), summary: attempts[1]!.summary ?? undefined } });
  ev(1098, actions, "system", { kind: "action.completed", data: runPdf });
  ev(1320, gstohl, "tty", { kind: "task.created", data: { task: IDS.tV0, title: "Remove deprecated v0 endpoints", labels: ["api", "cleanup"] } });
  ev(1310, agentOf(sOpusOld), "env", { kind: "attempt.started", data: { task: IDS.tV0, attempt: IDS.aV0, branch: attempts[2]!.branch, base: "main", base_commit: sha("main-v0"), session: IDS.sOpusOld } });
  ev(1280, agentOf(sOpusOld), "worktree", { kind: "attempt.abandoned", data: { attempt: IDS.aV0, reason: "v0 still serves 3 active customers (acme-eu, northwind, initech). Needs a deprecation notice first." } });
  ev(1220, gstohl, "tty", { kind: "task.created", data: { task: IDS.tRate, title: "Add token-bucket rate limiting to the public API", labels: ["api", "reliability"] } });
  ev(1200, agentOf(sCodexOld), "env", { kind: "attempt.started", data: { task: IDS.tRate, attempt: IDS.aRateOld, branch: attempts[3]!.branch, base: "main", base_commit: sha("main-rate-old"), session: IDS.sCodexOld } });
  const subRateOld = ev(1170, agentOf(sCodexOld), "worktree", { kind: "attempt.submitted", data: { attempt: IDS.aRateOld, head: head("rate-old"), summary: attempts[3]!.summary ?? undefined } });
  ev(1168, actions, "system", { kind: "action.completed", data: runRateOld });
  ev(1150, gstohl, "ui", { kind: "review.decided", data: { attempt: IDS.aRateOld, decision: "reject", reason: attempts[3]!.review?.reason ?? undefined } }, { on: subRateOld.id });
  ev(905, santosh, "ui", { kind: "review.decided", data: { attempt: IDS.aPdf, decision: "accept", reason: attempts[1]!.review?.reason ?? undefined } }, { on: subPdf.id });
  ev(900, agentOf(sOpusOld), "env", { kind: "session.ended", data: { session: IDS.sOpusOld, summary: "PDF streaming accepted; v0 removal parked." } });

  // Today.
  ev(310, agentOf(sOpus), "env", { kind: "session.started", data: { session: sOpus } });
  ev(290, agentOf(sOpus), "env", { kind: "attempt.started", data: { task: IDS.tRate, attempt: IDS.aRate, branch: attempts[4]!.branch, base: "main", base_commit: baseCommit, session: IDS.sOpus } });
  ev(250, santosh, "tty", { kind: "task.created", data: { task: IDS.tFlaky, title: "Fix flaky test in webhook retry suite", labels: ["tests", "flaky"], body: "`retries_on_503` fails ~1 in 8 runs on CI." } });
  ev(245, agentOf(sCodex), "env", { kind: "session.started", data: { session: sCodex } });
  ev(230, agentOf(sCodex), "env", { kind: "attempt.started", data: { task: IDS.tFlaky, attempt: IDS.aFlaky, branch: attempts[7]!.branch, base: "main", base_commit: baseCommit, session: IDS.sCodex } });
  ev(215, gstohl, "ui", { kind: "task.created", data: { task: IDS.tIdem, title: "Add idempotency keys to POST /v1/refunds", labels: ["api", "payments"], recipe: "idempotency-key@1" } });
  ev(205, agentOf(sHaikuTests), "env", { kind: "session.started", data: { session: sHaikuTests } });
  ev(200, agentOf(sHaikuTests), "env", { kind: "attempt.started", data: { task: IDS.tIdem, attempt: IDS.aIdem, branch: attempts[8]!.branch, base: "release/2026.10", base_commit: sha("release-base"), session: IDS.sHaikuTests } });
  ev(195, santosh, "tty", { kind: "task.created", data: { task: IDS.tCi, title: "Cache the cargo registry in CI", labels: ["ci"] } });
  ev(185, agentOf(sHaikuCi), "env", { kind: "session.started", data: { session: sHaikuCi } });
  ev(180, agentOf(sHaikuCi), "env", { kind: "attempt.started", data: { task: IDS.tCi, attempt: IDS.aCi, branch: attempts[5]!.branch, base: "main", base_commit: baseCommit, session: IDS.sHaikuCi } });
  for (const [i, c] of (commits[IDS.aRate] ?? []).entries()) {
    ev(270 - i * 25, agentOf(sOpus), "worktree", { kind: "commit.recorded", data: { sha: c.sha, subject: c.subject, attempt: IDS.aRate, diff: { files: 2, insertions: 40 + i * 13, deletions: i * 3 } } }, { idem: `commit:${c.sha}` });
    ev(268 - i * 25, agentOf(sOpus), "worktree", { kind: "tool.called", data: { tool: "Bash", input: "cargo test --workspace", ok: i !== 1, duration_ms: 61_000 + i * 900 } });
  }
  ev(165, agentOf(sHaikuCi), "worktree", { kind: "commit.recorded", data: { sha: sha("c-ci-1"), subject: "ci: cache cargo registry; raise timeout", attempt: IDS.aCi, diff: { files: 2, insertions: 7, deletions: 1 } } });
  const subCi = ev(160, agentOf(sHaikuCi), "worktree", { kind: "attempt.submitted", data: { attempt: IDS.aCi, head: head("ci"), summary: attempts[5]!.summary ?? undefined } });
  ev(145, actions, "system", { kind: "action.completed", data: runCi });
  ev(150, santosh, "tty", { kind: "task.created", data: { task: IDS.tLatency, title: "Investigate p99 latency regression on /v1/charges", labels: ["perf", "incident"] } });
  ev(140, agentOf(sOpus), "env", { kind: "attempt.started", data: { task: IDS.tLatency, attempt: IDS.aLatency, branch: attempts[6]!.branch, base: "main", base_commit: baseCommit, session: IDS.sOpus } });
  ev(120, gstohl, "ui", { kind: "review.decided", data: { attempt: IDS.aCi, decision: "changes_requested", reason: attempts[5]!.review?.reason ?? undefined } }, { on: subCi.id });
  ev(110, agentOf(sCodex), "worktree", { kind: "commit.recorded", data: { sha: sha("c-flaky-1"), subject: "Dedupe retry scheduling on duplicate 5xx", attempt: IDS.aFlaky, diff: { files: 1, insertions: 12, deletions: 3 } } });
  ev(100, agentOf(sOpus), "worktree", { kind: "commit.recorded", data: { sha: sha("c-lat-1"), subject: "wip: add tracing spans around charge queries", attempt: IDS.aLatency, diff: { files: 2, insertions: 23, deletions: 2 } } });
  ev(95, agentOf(sChatgpt), "mcp", { kind: "session.started", data: { session: sChatgpt } }, { idem: `mcp:${IDS.sChatgpt}:1` });
  ev(88, agentOf(sChatgpt), "mcp", { kind: "report", data: reports[3]!.report }, { idem: `mcp:${IDS.sChatgpt}:4` });
  ev(80, agentOf(sChatgpt), "mcp", { kind: "session.ended", data: { session: IDS.sChatgpt } });
  ev(70, agentOf(sHaikuTests), "worktree", { kind: "commit.recorded", data: { sha: sha("c-idem-1"), subject: "Idempotency-Key middleware for refunds", attempt: IDS.aIdem, diff: { files: 3, insertions: 96, deletions: 4 } } });
  ev(62, gstohl, "ui", { kind: "task.created", data: { task: IDS.tDocs, title: "Document the webhook signature scheme", labels: ["docs"] } });
  ev(55, agentOf(sOpus), "worktree", { kind: "tool.called", data: { tool: "WebFetch", input: "https://grafana.internal/d/billing-prod", ok: false, duration_ms: 1_200 } });
  ev(48, agentOf(sOpus), "worktree", { kind: "attempt.handoff", data: { attempt: IDS.aLatency, to: { type: "human", handle: "santosh" }, note: "Need prod dashboard access; see blocker report." } });
  ev(47, agentOf(sOpus), "worktree", { kind: "report", data: reports[0]!.report });
  ev(40, agentOf(sCodex), "worktree", { kind: "commit.recorded", data: { sha: sha("c-flaky-2"), subject: "tests: paused clock for retries_on_503", attempt: IDS.aFlaky, diff: { files: 1, insertions: 3, deletions: 3 } } });
  ev(36, agentOf(sOpus), "worktree", { kind: "attempt.submitted", data: { attempt: IDS.aRate, head: head("rate"), summary: attempts[4]!.summary ?? undefined, diff: attempts[4]!.diff ?? undefined } });
  ev(35, agentOf(sOpus), "worktree", { kind: "report", data: reports[2]!.report });
  ev(33, actions, "system", { kind: "action.completed", data: runRate });
  ev(20, santosh, "tty", { kind: "task.created", data: { task: IDS.tCreds, title: "Rotate staging database credentials", labels: ["ops", "security"] } });
  ev(14, agentOf(sCodex), "worktree", { kind: "report", data: reports[1]!.report });
  ev(13, agentOf(sCodex), "worktree", { kind: "attempt.submitted", data: { attempt: IDS.aFlaky, head: head("flaky"), summary: attempts[7]!.summary ?? undefined } });
  ev(11, actions, "system", { kind: "action.completed", data: runFlaky });
  ev(9, agentOf(sHaikuTests), "worktree", { kind: "commit.recorded", data: { sha: sha("c-idem-3"), subject: "tests: replayed refund returns the original body", attempt: IDS.aIdem, diff: { files: 1, insertions: 54, deletions: 0 } } });
  ev(4, agentOf(sHaikuTests), viaOf(sHaikuTests), { kind: "attempt.submitted", data: { attempt: IDS.aIdem, head: head("idem"), summary: attempts[8]!.summary ?? undefined } });
  ev(3, agentOf(sHaikuTests), "env", { kind: "session.ended", data: { session: IDS.sHaikuTests, summary: "Submitted idempotency keys; checks queued." } });
  // A kind from a newer gitbots: the UI must render it gracefully.
  events.push({
    v: 1,
    id: idAt("evt", ms(2), "unknown-kind"),
    ts: at(2),
    actor: hooks,
    via: "system",
    producer: "gitbots/0.2.0",
    kind: "attempt.rebased",
    data: { attempt: IDS.aIdem, onto: "release/2026.10" },
  });
  events.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));

  // ---- stats (with history beyond the events above) -------------------------------------
  const zero = {
    sessions: 0, subagent_sessions: 0, tasks_created: 0, attempts_started: 0, attempts_submitted: 0,
    accepted: 0, rejected: 0, changes_requested: 0, merged: 0, abandoned: 0, handoffs: 0, commits: 0,
    lines_added: 0, lines_removed: 0, runs: 0, runs_passed: 0, reports: 0, tool_calls: 0, tool_failures: 0,
  };
  const stats: Stats = {
    by_actor: {
      "anthropic/claude-opus-5-5@claude-code": {
        ...zero, sessions: 14, subagent_sessions: 0, attempts_started: 31, attempts_submitted: 28, accepted: 21, rejected: 2,
        changes_requested: 4, merged: 18, abandoned: 3, handoffs: 2, commits: 142, lines_added: 9_812, lines_removed: 3_407,
        runs: 36, runs_passed: 33, reports: 19, tool_calls: 2_418, tool_failures: 97,
      },
      "openai/gpt-5-codex@codex": {
        ...zero, sessions: 11, attempts_started: 24, attempts_submitted: 22, accepted: 13, rejected: 5, changes_requested: 3,
        merged: 11, abandoned: 2, handoffs: 1, commits: 96, lines_added: 7_120, lines_removed: 4_980, runs: 29, runs_passed: 21,
        reports: 8, tool_calls: 1_874, tool_failures: 141,
      },
      "anthropic/claude-haiku-4-5@claude-code": {
        ...zero, sessions: 17, subagent_sessions: 17, attempts_started: 12, attempts_submitted: 11, accepted: 6, rejected: 1,
        changes_requested: 3, merged: 5, commits: 41, lines_added: 2_206, lines_removed: 311, runs: 9, runs_passed: 7,
        reports: 4, tool_calls: 903, tool_failures: 22,
      },
      "openai/gpt-5@chatgpt": { ...zero, sessions: 3, reports: 3, tool_calls: 12 },
      "@gstohl": { ...zero, tasks_created: 23, handoffs: 1 },
      "@santosh": { ...zero, tasks_created: 17 },
    },
  };

  // ---- workflows, recipes -----------------------------------------------------------------
  const workflows: WorkflowsResponse = {
    workflows: [
      {
        path: ".gitbots/actions/ci.toml",
        workflow: { name: "ci", on: ["attempt.submitted", "manual"], jobs: { fmt: {}, clippy: {}, test: {} } },
      },
    ],
    invalid: [{ path: ".gitbots/actions/nightly.toml", error: "jobs.bench: unknown field `runs_on`, did you mean `runs-on`?" }],
  };
  const recipes: RecipesResponse = [
    { path: ".gitbots/recipes/idempotency-key/recipe.toml", recipe: { name: "idempotency-key", version: 1 } },
    { path: ".gitbots/recipes/broken/recipe.toml", error: "missing field `name`" },
  ];

  // ---- project --------------------------------------------------------------------------
  const project: ProjectInfo = {
    manifest: {
      version: 1,
      project: { id: IDS.project, name: "billing", description: "Billing API: invoices, charges, refunds, webhooks." },
      tenancy: { owner: { kind: "org", handle: "acme" }, team: "payments", workspace: null },
      mandate: {
        goal: "Ship billing v2: rate limits, idempotent refunds and streamed invoices by Oct 31.",
        autonomy: "assisted",
        principals: [
          { handle: "gstohl", email: "dominik@gstohl.com", role: "owner" },
          { handle: "santosh", email: "santosh@gitbots.dev", role: "maintainer" },
          { handle: "priya", role: "reviewer" },
        ],
        agents: {
          allowed_paths: ["**"],
          denied_paths: [".gitbots/**", ".github/**", ".claude/**", ".codex/**", ".cursor/**", ".mcp.json", "**/CLAUDE.md", "**/AGENTS.md", "CLAUDE.md", "AGENTS.md", "migrations/prod/**"],
          protected_branches: ["main", "release/*"],
        },
        approvals: { accept_attempt: "reviewer", merge_protected: "maintainer", change_mandate: "owner", run_hosted_action: "maintainer" },
      },
      ledger: { activity_branch: "gitbots/activity", logs_branch: "gitbots/logs" },
      workrooms: { branch_prefix: "gitbots/attempt" },
    },
    manifest_source: { type: "trusted", branch: "main", oid: sha("manifest") },
    trusted_branch: "main",
    viewer: gstohl,
    can_decide: true,
    producer: "gitbots/0.1.0",
  };

  return { project, sessions, tasks, attempts, events, stats, reports, extras, diffs, logs, workflows, recipes, orphans: 1 };
}

/** States present in the fixtures, for tests that check coverage. */
export const ALL_STATES: AttemptState[] = ["active", "submitted", "changes_requested", "accepted", "rejected", "merged", "abandoned"];

# gitbots web

The human frontend for gitbots: inbox, board, attempt review, activity, agents,
sessions and mandate. SolidJS + TypeScript, built with Vite. This is the one
non-Rust part of the repo (see `AGENTS.md`).

The app only talks to the JSON API in [`docs/API.md`](../docs/API.md). It never
reads git, so the same build runs against `gitbots ui` locally and against the
hosted Worker ([`docs/CLOUD.md`](../docs/CLOUD.md)).

Node 22.22+, 24.15+ or 26+ (CI uses 24).

## Run with mock data (no repo needed)

```sh
cd web
npm install
npm run dev:mock          # http://localhost:5173
```

`VITE_GITBOTS_MOCK=1` swaps the network for an in-browser mock server
(`src/mocks/server.ts`) with realistic fixtures (`src/mocks/fixtures.ts`): three
agents (Claude Opus via Claude Code, Codex, a Claude Haiku subagent) plus a
ChatGPT planner, tasks in every status, attempts in every state, passing and
failing checks with logs, a blocker report, and a ~5,000-line diff. Writes
(new task, review) work and move the ledger tip, so live updates behave like
the real thing. The fixtures are typed against `src/api/types.ts`, so a
contract change that breaks them fails `npm run typecheck`.

The **mock** picker in the top bar switches scenarios (the page reloads):

| scenario | shows |
|----------|-------|
| Default | you are `@gstohl`, owner |
| Hosted | `hosted: true`; writes return 202 `{queued, outbox}`; a fake `gitbots sync` applies the outbox every 10 s |
| Viewer is @priya (reviewer) | the mandate refuses merges into `main` (server error shown verbatim) |
| Started by an agent | `can_decide: false`, read-only UI |
| Inbox zero | the empty state |
| Slow network | 1.5 s latency, for skeletons |
| Bad token (401) | the "open the link printed by `gitbots ui`" screen |

## Run against a real repo

```sh
# terminal 1, in an gitbots repo (or a demo: scripts/demo.sh, which prints the command)
gitbots ui                    # prints http://127.0.0.1:7777/#token=<hex>

# terminal 2
cd web && npm run dev
```

Open the printed link **on port 5173** with the same token:
`http://localhost:5173/#token=<hex>`. Vite proxies `/api` to
`http://127.0.0.1:7777` with `changeOrigin`, so the Host header passes the
server's localhost check. For another port:
`GITBOTS_API_TARGET=http://127.0.0.1:7799 npm run dev`.

The token is read from `#token=`, kept in `sessionStorage` for this tab and
removed from the URL. Any 401 shows a screen asking for the link again (a new
token is issued every time `gitbots ui` starts). If `gitbots ui` was started from an
agent session (`CLAUDECODE`, `CODEX_*` or `GITBOTS_SESSION` in its env), the UI is
read-only: `project.can_decide` is false and the forms explain why.

## Build

```sh
npm run build              # -> web/dist
gitbots ui --assets web/dist  # serves the app and the API on one port
```

Asset paths are root-relative (`base: "/"`). `gitbots ui` and the hosted Worker
both serve `index.html` for unknown non-`/api` paths, so client-side routes
(`/board`, `/attempts/:id`, ...) work on reload. Mock code is compiled out of
production builds.

## Hosted mode

When `/api/project` says `hosted: true`, `POST /api/tasks` and
`POST /api/attempts/{id}/review` may answer **202** `{queued: true, outbox}`.
The UI then shows "Queued: applied on the next `gitbots sync`" with a pending badge,
keeps the attempt marked "decision pending sync" until the tip moves and the
attempt leaves `submitted`, and shows `pending_outbox` in the top bar.

## Checks

```sh
npm run typecheck   # tsc --noEmit, strict
npm test            # vitest + jsdom
npm run build
```

CI runs all three (`web` job in `.github/workflows/ci.yml`).

Tests cover the diff parser (including a 5k-line performance check),
`describeEvent` for every kind plus unknown and malformed ones, the token and
error handling in the client, the stats rates and sorting, the mock server
(including the hosted outbox), and component tests for the inbox and the
review panel.

Two tests guard against contract drift:

- `src/lib/events.test.ts` reads `crates/gitbots-core/src/event.rs` and fails if
  `kind::ALL` and `KNOWN_KINDS` differ.
- `src/mocks/real/real.test.ts` checks real `gitbots ui` responses
  (`src/mocks/real/*.json`) against `src/api/types.ts`: at the type level via
  `satisfies` (literal unions widened, since JSON imports lose them) and at
  runtime for the unions and for keys the contract doesn't list. Refresh the
  snapshots against a running server with
  `npm run snapshot:real -- 'http://127.0.0.1:7799/#token=…'`. The script only
  does GETs, and the snapshots overwrite the files in `src/mocks/real/`.

## Keyboard

`g i` inbox, `g b` board, `g a` activity, `g g` agents, `g s` sessions,
`g m` mandate, `/` filter, `j`/`k` next/previous item, `?` help. On an attempt:
`1`–`4` switch tabs, `r` jumps to the review panel.

## Layout

```
web/
  index.html, vite.config.ts, tsconfig.json
  public/              favicon, theme-init.js (applies the stored theme before paint)
  scripts/             snapshot-real.ts (captures real API responses)
  src/
    index.tsx          entry: takes the token from the URL, mounts the app
    App.tsx            router, shell, global shortcuts, live polling
    state.tsx          shared live data: project, board, inbox
    api/
      types.ts         mirror of docs/API.md (plus per-kind event payloads)
      client.ts        typed fetch client: token, {error} parsing, 401, hosted 202
      live.ts          GET /api/tip every 3 s (paused when hidden) -> refetch
      pending.ts       hosted: decisions queued in the outbox
    lib/               pure logic: diff parser, describeEvent, stats, log, mandate, format
    components/        AgentChip, StatePill, RelativeTime, DiffView, LogViewer, ReviewPanel, ...
    pages/             Inbox, Board, Attempt, Activity, Agents, Sessions, Mandate
    styles/            tokens.css (light/dark design tokens), base, components, pages, diff
    mocks/             fixtures, mock server, diffs, logs; real/ = captured responses
    test/              test setup and component tests
```

No UI framework and no Tailwind. Styling is hand-written CSS on design tokens
(`src/styles/tokens.css`). Light and dark follow `prefers-color-scheme`, and the
toggle (system, light, dark) is stored in `localStorage`. Provider colors use
an eight-slot categorical palette that passes the CVD-separation checks in both
themes. Three light-mode slots sit below 3:1 against white, so a provider color
never appears without its text label (`model@client`, provider name).

## Dependencies

Runtime:

| package | why |
|---------|-----|
| `solid-js` | the UI library (the project's chosen frontend stack) |
| `@solidjs/router` | client-side routes (`/attempts/:id`, query-synced filters) |

Dev only:

| package | why |
|---------|-----|
| `vite` | dev server with the `/api` proxy, production build |
| `vite-plugin-solid` | compiles Solid's JSX |
| `typescript` | strict type checking (`tsc --noEmit`; v7, the native compiler) |
| `vitest` | test runner that shares the Vite config |
| `jsdom` | DOM for component tests |
| `@solidjs/testing-library` | renders Solid components in tests |

Nothing else: no CSS framework, no date library (`Intl`), no diff or ANSI
library (small, tested parsers in `src/lib`), no state library (Solid signals and
`createResource`).

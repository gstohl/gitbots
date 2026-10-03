-- gitbots-worker control plane (docs/CLOUD.md).

-- One row per provisioned project. The owner key is never stored, only its
-- SHA-256 (lowercase hex).
CREATE TABLE projects (
  id              TEXT PRIMARY KEY,          -- prj_<ULID>, as given at provisioning
  name            TEXT NOT NULL,
  repo            TEXT NOT NULL UNIQUE,      -- Artifacts repo of the code: <prj>
  logs_repo       TEXT NOT NULL UNIQUE,      -- <prj>-logs
  owner_key_hash  TEXT NOT NULL UNIQUE,
  trusted_branch  TEXT NOT NULL DEFAULT 'main',
  created_at      TEXT NOT NULL
);

-- Every Artifacts repo the Worker manages, with its indexing checkpoint.
CREATE TABLE repos (
  name            TEXT PRIMARY KEY,
  project_id      TEXT NOT NULL REFERENCES projects(id),
  role            TEXT NOT NULL CHECK (role IN ('main', 'logs', 'fork')),
  remote          TEXT NOT NULL,
  attempt         TEXT,                      -- forks: the attempt they were made for
  session         TEXT,                      -- forks: the session that asked for it
  created_at      TEXT NOT NULL,
  indexed_commit  TEXT,                      -- gitbots/activity commit last fully indexed
  indexed_tree    TEXT,                      -- its root tree (prunes the next walk)
  indexed_at      TEXT
);
CREATE INDEX repos_project ON repos(project_id, role);
CREATE UNIQUE INDEX repos_fork_attempt ON repos(project_id, attempt) WHERE role = 'fork';

-- Minted Artifacts tokens: which session got which repo (attested identity).
CREATE TABLE tokens (
  id              TEXT PRIMARY KEY,          -- Artifacts token id
  project_id      TEXT NOT NULL REFERENCES projects(id),
  repo            TEXT NOT NULL,
  scope           TEXT NOT NULL CHECK (scope IN ('read', 'write')),
  session         TEXT,
  created_at      TEXT NOT NULL,
  expires_at      TEXT NOT NULL
);
CREATE INDEX tokens_project ON tokens(project_id, repo);
CREATE INDEX tokens_session ON tokens(session);

-- Indexed ledger events, upserted by id (idempotent).
CREATE TABLE events (
  project_id      TEXT NOT NULL REFERENCES projects(id),
  id              TEXT NOT NULL,             -- evt_<ULID>
  kind            TEXT NOT NULL,
  ts              TEXT NOT NULL,
  session         TEXT,                      -- actor.session
  task            TEXT,                      -- the task the event names directly
  attempt         TEXT,                      -- the attempt the event names directly
  repo            TEXT NOT NULL,             -- repo it was indexed from
  path            TEXT NOT NULL,
  blob            TEXT NOT NULL,
  json            TEXT NOT NULL,             -- the ledger file, verbatim
  indexed_at      TEXT NOT NULL,
  PRIMARY KEY (project_id, id)
);
CREATE INDEX events_kind ON events(project_id, kind);
CREATE INDEX events_session ON events(project_id, session);
CREATE INDEX events_task ON events(project_id, task);
CREATE INDEX events_attempt ON events(project_id, attempt);

-- Human decisions from the hosted dashboard, applied by `gitbots sync`.
CREATE TABLE outbox (
  id              TEXT PRIMARY KEY,          -- obx_<ULID>
  project_id      TEXT NOT NULL REFERENCES projects(id),
  created_at      TEXT NOT NULL,
  kind            TEXT NOT NULL CHECK (kind IN ('task.create', 'review')),
  body            TEXT NOT NULL,             -- JSON
  actor           TEXT NOT NULL,             -- JSON gitbots_core::Actor (human)
  status          TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'done', 'failed')),
  event           TEXT,                      -- ledger event that recorded the decision
  last_error      TEXT,                      -- ack error; may come with `event` (e.g. the
                                             -- review was recorded but the merge failed)
  acked_at        TEXT
);
CREATE INDEX outbox_pending ON outbox(project_id, status, id);

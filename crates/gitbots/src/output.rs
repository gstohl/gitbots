//! Rendering for the CLI: terse text for humans, JSON for agents.

use anyhow::Result;
use serde::Serialize;
use serde_json::json;

use gitbots_cloud::api::TokenIssued;
use gitbots_core::board::{ReportView, SessionView};
use gitbots_core::event::{EventBody, short_sha};
use gitbots_core::ledger::LedgerKind;
use gitbots_core::{ActionRun, Board, Event, Recipe, Session, Stats};
use gitbots_git::{BranchPush, HookReport, MergeOutcome, PushStatus, SyncReport};

use crate::cloud::{self, CloudSyncReport, ForkReport};
use crate::project::{
    ActorCtx, AttemptInfo, Inbox, InitReport, ManifestSource, Project, SubmitOutcome,
};

pub struct Out {
    json: bool,
    /// One JSON document per line (for `--watch`).
    compact: bool,
}

impl Out {
    pub fn new(json: bool) -> Self {
        Self { json, compact: false }
    }

    /// JSON lines instead of pretty JSON, for output that repeats.
    pub fn lines(self) -> Self {
        Self { compact: true, ..self }
    }

    pub fn is_json(&self) -> bool {
        self.json
    }

    fn emit(&self, value: &impl Serialize, text: impl FnOnce() -> String) -> Result<()> {
        if self.json && self.compact {
            println!("{}", serde_json::to_string(value)?);
        } else if self.json {
            println!("{}", serde_json::to_string_pretty(value)?);
        } else {
            let text = text();
            if !text.is_empty() {
                println!("{}", text.trim_end());
            }
        }
        Ok(())
    }

    pub fn done(&self, message: &str, value: &impl Serialize) -> Result<()> {
        self.emit(value, || message.to_owned())
    }
}

pub fn init(out: &Out, r: &InitReport) -> Result<()> {
    let value = json!({
        "project": r.project,
        "manifest": r.manifest,
        "created_manifest": r.created_manifest,
        "created_ledgers": r.created_ledgers,
        "hooks": r.hooks.as_ref().map(|h| json!({"installed": h.installed, "skipped": h.skipped})),
        "committed": r.committed,
        "trusted_branch": r.trusted_branch,
    });
    out.emit(&value, || {
        let mut s = String::new();
        if r.created_manifest {
            s += &format!("initialized {} at {}\n", r.project, r.manifest.display());
        } else {
            s += &format!("joined {} ({})\n", r.project, r.manifest.display());
        }
        for b in &r.created_ledgers {
            s += &format!("created ledger branch {b}\n");
        }
        if let Some(h) = &r.hooks {
            for hook in &h.installed {
                s += &format!("installed hook {hook}\n");
            }
            for (hook, why) in &h.skipped {
                s += &format!("skipped hook {hook}: {why}\n");
            }
        }
        if r.created_manifest && !r.committed {
            s += &format!(
                "\nnext: review .gitbots/manifest.json, then commit .gitbots/ to `{}`;\n\
                 the mandate takes effect from the trusted branch.\n",
                r.trusted_branch
            );
        }
        s
    })
}

pub fn whoami(out: &Out, project: &Project, ctx: &ActorCtx) -> Result<()> {
    let manifest = match project.manifest_source() {
        ManifestSource::Trusted { branch, oid } => format!("{branch} ({})", short_sha(oid)),
        ManifestSource::WorkingTree => {
            "working tree (not committed to the trusted branch yet)".into()
        }
    };
    let value = json!({ "actor": ctx.actor, "via": ctx.via, "manifest": manifest, "attempt": project.current_attempt()? });
    out.emit(&value, || {
        let mut s =
            format!("{}  (via {:?})\nmandate from {manifest}\n", ctx.actor.label(), ctx.via);
        if let Ok(Some(a)) = project.current_attempt() {
            s += &format!("workroom for attempt {a}\n");
        }
        s
    })
}

pub fn session_started(out: &Out, session: &Session, bound: bool) -> Result<()> {
    out.emit(session, || {
        let mut s = format!("{}  {}\n", session.id, session.agent.key());
        if bound {
            s += "bound to this worktree\n";
        } else {
            s += &format!("export GITBOTS_SESSION={}\n", session.id);
        }
        s
    })
}

pub fn sessions(out: &Out, sessions: &[SessionView]) -> Result<()> {
    out.emit(&sessions, || {
        sessions
            .iter()
            .map(|v| {
                let s = &v.session;
                let parent =
                    s.parent.as_ref().map(|p| format!(" <- {}", p.short())).unwrap_or_default();
                let state = if v.ended { "ended" } else { "open" };
                format!(
                    "{}  {:<6} {}{}{}\n",
                    s.id.short(),
                    state,
                    s.agent.key(),
                    s.role.as_ref().map(|r| format!(" [{r}]")).unwrap_or_default(),
                    parent
                )
            })
            .collect()
    })
}

pub fn status(out: &Out, project: &Project, board: &Board) -> Result<()> {
    let value = json!({
        "project": project.manifest().project,
        "tasks": board.tasks.values().map(|t| json!({"task": t, "status": board.task_status(t)})).collect::<Vec<_>>(),
        "awaiting_review": board.awaiting_review().map(|a| &a.id).collect::<Vec<_>>(),
    });
    out.emit(&value, || {
        let m = project.manifest();
        let mut s = format!("{} · {:?} · {}\n", m.project.name, m.mandate.autonomy, m.project.id);
        if let Some(goal) = &m.mandate.goal {
            s += &format!("goal: {goal}\n");
        }
        if board.tasks.is_empty() {
            s += "\nno tasks yet: `gitbots task create \"...\"`\n";
        }
        for t in board.tasks.values() {
            s += &format!("\n{}  {:?}  {}\n", t.id.short(), board.task_status(t), t.title);
            for a in t.attempts.iter().filter_map(|id| board.attempts.get(id)) {
                s += &format!("  └ {}\n", attempt_line(a));
            }
        }
        let waiting = board.awaiting_review().count();
        if waiting > 0 {
            s += &format!("\n{waiting} attempt(s) awaiting review: `gitbots inbox`\n");
        }
        s
    })
}

fn attempt_line(a: &gitbots_core::AttemptView) -> String {
    let checks = match a.checks_passed() {
        Some(true) => " ✓checks",
        Some(false) => " ✗checks",
        None => "",
    };
    let who = a.submitted_by.as_ref().unwrap_or(&a.started_by).label();
    let diff = a.diff.map(|d| format!(" +{}-{}", d.insertions, d.deletions)).unwrap_or_default();
    format!("{} {:<17} {}{}{}  {}", a.id.short(), a.state.as_str(), a.branch, diff, checks, who)
}

pub fn tasks(out: &Out, board: &Board) -> Result<()> {
    let list: Vec<_> = board.tasks.values().collect();
    out.emit(&list, || {
        board
            .tasks
            .values()
            .map(|t| {
                format!(
                    "{}  {:<10} {} ({} attempts)\n",
                    t.id.short(),
                    format!("{:?}", board.task_status(t)),
                    t.title,
                    t.attempts.len()
                )
            })
            .collect()
    })
}

pub fn task(out: &Out, board: &Board, query: &str) -> Result<()> {
    let t = board.find_task(query)?;
    let attempts: Vec<_> = t.attempts.iter().filter_map(|id| board.attempts.get(id)).collect();
    out.emit(&json!({"task": t, "status": board.task_status(t), "attempts": attempts}), || {
        let mut s = format!(
            "{}  {}\n{:?}, created by {}\n",
            t.id,
            t.title,
            board.task_status(t),
            t.created_by.label()
        );
        if let Some(r) = &t.recipe {
            s += &format!("recipe: {r}\n");
        }
        if let Some(b) = &t.body {
            s += &format!("\n{b}\n");
        }
        for a in attempts {
            s += &format!("\n{}\n", attempt_line(a));
        }
        s
    })
}

pub fn attempt_started(out: &Out, info: &AttemptInfo) -> Result<()> {
    out.emit(info, || {
        format!(
            "attempt {} on {}\nworkroom: {}\n\ncd {}\n",
            info.attempt.short(),
            info.branch,
            info.workroom.display(),
            info.workroom.display()
        )
    })
}

pub fn attempts(out: &Out, project: &Project, board: &Board) -> Result<()> {
    let list: Vec<_> = board.attempts.values().collect();
    out.emit(&list, || {
        board
            .attempts
            .values()
            .map(|a| {
                let wt = project
                    .workroom_of(a)
                    .ok()
                    .flatten()
                    .map(|p| format!("\n    {}", p.display()))
                    .unwrap_or_default();
                format!("{}{wt}\n", attempt_line(a))
            })
            .collect()
    })
}

pub fn attempt(out: &Out, project: &Project, board: &Board, query: &str) -> Result<()> {
    let a = board.find_attempt(query)?;
    let workroom = project.workroom_of(a)?;
    let violations: Vec<String> = project
        .violations(a)
        .map(|v| v.into_iter().map(|v| format!("{} ({})", v.path, v.reason)).collect())
        .unwrap_or_default();
    out.emit(&json!({"attempt": a, "workroom": workroom, "violations": violations}), || {
        let mut s = format!("{}\n{}\n", a.id, attempt_line(a));
        if let Some(t) = board.tasks.get(&a.task) {
            s += &format!("task: {} {}\n", t.id.short(), t.title);
        }
        if let Some(w) = &workroom {
            s += &format!("workroom: {}\n", w.display());
        }
        if let Some(sum) = &a.summary {
            s += &format!("summary: {sum}\n");
        }
        for r in &a.runs {
            s += &format!("run {} {} {}\n", r.run.short(), r.workflow, r.status.as_str());
        }
        if let Some(r) = &a.review {
            s += &format!(
                "review: {:?} by {}{}\n",
                r.decision,
                r.by.label(),
                r.reason.as_ref().map(|x| format!(": {x}")).unwrap_or_default()
            );
        }
        for v in &violations {
            s += &format!("mandate violation: {v}\n");
        }
        s
    })
}

pub fn submitted(out: &Out, o: &SubmitOutcome) -> Result<()> {
    out.emit(o, || {
        let mut s = format!(
            "submitted {} at {} (+{} -{} in {} files)\n",
            o.attempt.short(),
            short_sha(&o.head),
            o.diff.insertions,
            o.diff.deletions,
            o.diff.files
        );
        s += &runs_text(&o.runs, &o.invalid_workflows);
        s
    })
}

fn runs_text(runs: &[ActionRun], invalid: &[(String, String)]) -> String {
    let mut s = String::new();
    for r in runs {
        s +=
            &format!("{} {} ({} ms, {})\n", r.workflow, r.status.as_str(), r.duration_ms, r.runner);
        for j in &r.jobs {
            let log = j.log.as_ref().map(|l| format!("  {l}")).unwrap_or_default();
            s += &format!("  {} {}{}\n", j.name, j.status.as_str(), log);
        }
    }
    for (path, err) in invalid {
        s += &format!("invalid workflow {path}: {err}\n");
    }
    s
}

pub fn runs(out: &Out, runs: &[ActionRun], invalid: &[(String, String)]) -> Result<()> {
    out.emit(&json!({"runs": runs, "invalid_workflows": invalid}), || {
        if runs.is_empty() && invalid.is_empty() {
            "no matching workflows\n".into()
        } else {
            runs_text(runs, invalid)
        }
    })
}

pub fn inbox(out: &Out, inbox: &Inbox) -> Result<()> {
    out.emit(inbox, || {
        let mut s = String::new();
        if inbox.awaiting_review.is_empty() && inbox.reports.is_empty() {
            return "inbox zero\n".into();
        }
        if !inbox.awaiting_review.is_empty() {
            s += "awaiting review:\n";
            for a in &inbox.awaiting_review {
                s += &format!("  {}\n", attempt_line(a));
                if let Some(sum) = &a.summary {
                    s += &format!("    {sum}\n");
                }
            }
            s += "  -> gitbots review <attempt> accept|reject|changes [--merge]\n\n";
        }
        for r in &inbox.reports {
            s += &report_line(r);
        }
        s
    })
}

fn report_line(r: &ReportView) -> String {
    let mut s = format!("[{:?}] {}  — {}\n", r.report.level, r.report.title, r.by.label());
    if let Some(b) = &r.report.body {
        s += &format!("    {}\n", b.replace('\n', "\n    "));
    }
    s
}

pub fn events(out: &Out, events: &[Event]) -> Result<()> {
    out.emit(&events, || {
        events
            .iter()
            .map(|e| {
                let ts =
                    e.ts.format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
                let extra = match &e.body {
                    EventBody::Unknown { .. } => " (unknown kind)",
                    _ => "",
                };
                format!(
                    "{} {:<18} {:<40} {}{extra}\n",
                    &ts[..ts.len().min(19)],
                    e.kind(),
                    e.actor.label(),
                    e.body.summary()
                )
            })
            .collect()
    })
}

pub fn stats(out: &Out, stats: &Stats) -> Result<()> {
    out.emit(stats, || {
        if stats.by_actor.is_empty() {
            return "no activity yet\n".into();
        }
        let mut s = format!(
            "{:<44} {:>4} {:>4} {:>4} {:>6} {:>7} {:>8} {:>9}\n",
            "actor", "ses", "att", "sub", "acc%", "runs✓", "commits", "+/-"
        );
        for (who, st) in &stats.by_actor {
            let pct = |r: Option<f64>| r.map_or("-".into(), |r| format!("{:.0}", r * 100.0));
            s += &format!(
                "{:<44} {:>4} {:>4} {:>4} {:>6} {:>7} {:>8} {:>9}\n",
                who,
                st.sessions,
                st.attempts_started,
                st.attempts_submitted,
                pct(st.acceptance_rate()),
                format!("{}/{}", st.runs_passed, st.runs),
                st.commits,
                format!("+{}-{}", st.lines_added, st.lines_removed),
            );
        }
        s
    })
}

pub fn workflows(
    out: &Out,
    ok: &[(String, gitbots_actions::Workflow)],
    bad: &[(String, String)],
) -> Result<()> {
    out.emit(&json!({"workflows": ok, "invalid": bad}), || {
        let mut s = String::new();
        for (path, wf) in ok {
            s += &format!("{:<20} on {:<32} {}\n", wf.name, wf.on.join(","), path);
        }
        for (path, err) in bad {
            s += &format!("invalid {path}: {err}\n");
        }
        if s.is_empty() {
            s = "no workflows in .gitbots/actions on the trusted branch\n".into();
        }
        s
    })
}

pub fn recipes(out: &Out, recipes: &[(String, anyhow::Result<Recipe>)]) -> Result<()> {
    let value: Vec<_> = recipes
        .iter()
        .map(|(p, r)| match r {
            Ok(r) => json!({"path": p, "recipe": r}),
            Err(e) => json!({"path": p, "error": e.to_string()}),
        })
        .collect();
    out.emit(&value, || {
        let mut s = String::new();
        for (path, r) in recipes {
            match r {
                Ok(r) => {
                    s += &format!("{:<28} {}\n", r.id(), r.description.as_deref().unwrap_or(""))
                }
                Err(e) => s += &format!("invalid {path}: {e:#}\n"),
            }
        }
        if s.is_empty() {
            s = "no recipes in .gitbots/recipes on the trusted branch\n".into();
        }
        s
    })
}

pub fn sync(out: &Out, reports: &[SyncReport]) -> Result<()> {
    let value: Vec<_> = reports.iter().map(ledger_json).collect();
    out.emit(&value, || reports.iter().map(ledger_text).collect())
}

fn ledger_json(r: &SyncReport) -> serde_json::Value {
    json!({
        "branch": r.branch,
        "fetched": r.fetched,
        "outcome": r.outcome.as_ref().map(merge_label),
        "conflicts": match &r.outcome { Some(MergeOutcome::Merged { conflicts, .. }) => conflicts.clone(), _ => vec![] },
        "pushed": r.pushed,
    })
}

fn ledger_text(r: &SyncReport) -> String {
    let outcome = r.outcome.as_ref().map_or("nothing to merge", merge_label);
    let mut s = format!("{}: {outcome}{}\n", r.branch, if r.pushed { ", pushed" } else { "" });
    if let Some(MergeOutcome::Merged { conflicts, .. }) = &r.outcome {
        for c in conflicts {
            s += &format!("  CONFLICT quarantined: {c}\n");
        }
    }
    s
}

fn push_json(p: &BranchPush) -> serde_json::Value {
    let reason = match &p.status {
        PushStatus::Rejected { reason } => Some(reason),
        _ => None,
    };
    json!({ "branch": p.branch, "status": push_label(&p.status), "reason": reason })
}

fn push_text(p: &BranchPush) -> String {
    match &p.status {
        PushStatus::Rejected { reason } => format!("{}: not pushed: {reason}\n", p.branch),
        status => format!("{}: {}\n", p.branch, push_label(status)),
    }
}

fn push_label(status: &PushStatus) -> &'static str {
    match status {
        PushStatus::Created => "created",
        PushStatus::Updated => "pushed",
        PushStatus::UpToDate => "up to date",
        PushStatus::Rejected { .. } => "rejected",
    }
}

/// Warnings go to stderr in text mode and into the JSON otherwise.
fn warn_text(out: &Out, warnings: &[String]) {
    if !out.json {
        for w in warnings {
            eprintln!("gitbots: warning: {w}");
        }
    }
}

pub fn cloud_sync(out: &Out, r: &CloudSyncReport) -> Result<()> {
    let mut warnings = r.warnings.clone();
    if let Some(st) = &r.steward {
        warnings.extend(st.warnings.iter().cloned());
    }
    let value = json!({
        "remote": cloud::MAIN_REMOTE,
        "actor": r.actor,
        "ledgers": r.ledgers.iter().map(ledger_json).collect::<Vec<_>>(),
        "branches": r.branches.iter().map(push_json).collect::<Vec<_>>(),
        "outbox": r.steward.as_ref().map(|st| json!({
            "items": st.items,
            "trusted": st.trusted.as_ref().map(push_json),
        })),
        "outbox_skipped": r.steward_skipped,
        "ingest": r.ingest,
        "warnings": warnings,
    });
    warn_text(out, &warnings);
    out.emit(&value, || {
        let mut s: String = r.ledgers.iter().map(ledger_text).collect();
        let moved = r.branches.iter().filter(|p| p.status != PushStatus::UpToDate);
        s += &moved.map(push_text).collect::<String>();
        if let Some(st) = &r.steward {
            for i in &st.items {
                let what = match &i.error {
                    Some(e) => format!("failed: {e}"),
                    None => i.summary.clone(),
                };
                let ack = if i.acked { "" } else { " (ack pending)" };
                s += &format!("outbox {} {}: {what}{ack}\n", i.id, i.kind);
            }
            if let Some(p) = &st.trusted {
                s += &push_text(p);
            }
        }
        if let Some(why) = &r.steward_skipped {
            s += &format!("outbox: not applied ({why})\n");
        }
        if let Some(ingest) = &r.ingest {
            let new: u32 = ingest.repos.iter().map(|x| x.new_events).sum();
            s += &format!("dashboard: {new} new event(s) indexed\n");
        }
        s
    })
}

pub fn cloud_init(out: &Out, r: &cloud::InitReport) -> Result<()> {
    let value = json!({
        "url": r.config.url,
        "project": r.config.project,
        "created": r.created,
        "verified_only": r.verified_only,
        "namespace": r.namespace,
        "remotes": { cloud::MAIN_REMOTE: r.remotes.main, cloud::LOGS_REMOTE: r.remotes.logs },
        "key": r.key_source,
        "ledgers": r.ledgers.iter().map(ledger_json).collect::<Vec<_>>(),
        "branches": r.branches.iter().map(push_json).collect::<Vec<_>>(),
        "ingest": r.ingest,
        "warnings": r.warnings,
        "dashboard": r.dashboard,
    });
    warn_text(out, &r.warnings);
    out.emit(&value, || {
        let mut s = if r.created {
            format!("provisioned {} at {}\n", r.config.project, r.config.url)
        } else if r.verified_only {
            format!("already set up: {} at {} (verified)\n", r.config.project, r.config.url)
        } else {
            format!("joined {} at {}\n", r.config.project, r.config.url)
        };
        s += &format!("owner key: {}\n", r.key_source);
        s += &format!(
            "{}: {}\n{}: {}\n",
            cloud::MAIN_REMOTE,
            r.remotes.main,
            cloud::LOGS_REMOTE,
            r.remotes.logs
        );
        s += &r.ledgers.iter().map(ledger_text).collect::<String>();
        s += &r.branches.iter().map(push_text).collect::<String>();
        s += &format!("\ndashboard (keep private, it signs you in):\n  {}\n", r.dashboard);
        s
    })
}

pub fn cloud_status(out: &Out, st: &cloud::Status) -> Result<()> {
    warn_text(out, &st.errors);
    out.emit(st, || {
        if !st.configured {
            return "gitbots cloud is not set up here: `gitbots cloud init --url <worker-url>`\n"
                .into();
        }
        let mut s = format!(
            "{} at {}\n",
            st.project.as_deref().unwrap_or("?"),
            st.url.as_deref().unwrap_or("?")
        );
        for (name, url) in &st.remotes {
            s += &format!("{name}: {}\n", url.as_deref().unwrap_or("(missing)"));
        }
        match &st.key {
            Some(k) => s += &format!("owner key: {k}\n"),
            None => s += "owner key: missing\n",
        }
        s += &format!("auto-sync: {}\n", if st.auto_sync { "on" } else { "off" });
        if let Some(info) = &st.info {
            s += &format!(
                "worker: {} \"{}\" in namespace {}, since {}\n",
                info.project_id, info.name, info.namespace, info.created_at
            );
        }
        if let Some(n) = st.pending_outbox {
            s += &format!(
                "outbox: {n} pending decision(s){}\n",
                if n > 0 { ": `gitbots sync`" } else { "" }
            );
        }
        s
    })
}

pub fn cloud_token(out: &Out, t: &TokenIssued) -> Result<()> {
    out.emit(t, || {
        format!(
            "token:   {}\nremote:  {}\nexpires: {}\n\ngit -c http.extraHeader=\"Authorization: Bearer $TOKEN\" push {} <branch>\n",
            t.token, t.remote, t.expires_at, t.remote
        )
    })
}

pub fn cloud_fork(out: &Out, f: &ForkReport) -> Result<()> {
    let value = json!({
        "attempt": f.attempt,
        "repo": f.fork.repo,
        "remote": f.fork.remote,
        "git_remote": f.git_remote,
        "token": f.fork.token,
        "expires_at": f.fork.expires_at,
    });
    out.emit(&value, || {
        format!(
            "fork {} of attempt {}\nremote:  {} (git remote {})\ntoken:   {}\nexpires: {}\n\ngit -c http.extraHeader=\"Authorization: Bearer $TOKEN\" push {} <branch>\n",
            f.fork.repo,
            f.attempt.short(),
            f.fork.remote,
            f.git_remote,
            f.fork.token,
            f.fork.expires_at,
            f.git_remote
        )
    })
}

pub fn cloud_dashboard(out: &Out, link: &str) -> Result<()> {
    out.emit(&json!({ "dashboard": link }), || format!("{link}\n"))
}

fn merge_label(m: &MergeOutcome) -> &'static str {
    match m {
        MergeOutcome::UpToDate => "up to date",
        MergeOutcome::FastForward => "fast-forwarded",
        MergeOutcome::Merged { .. } => "merged",
    }
}

pub fn hooks(out: &Out, r: &HookReport) -> Result<()> {
    out.emit(&json!({"installed": r.installed, "skipped": r.skipped}), || {
        let mut s = String::new();
        for h in &r.installed {
            s += &format!("installed {h}\n");
        }
        for (h, why) in &r.skipped {
            s += &format!("skipped {h}: {why}\n");
        }
        s
    })
}

#[allow(dead_code)]
fn ledger_label(kind: LedgerKind) -> &'static str {
    match kind {
        LedgerKind::Activity => "activity",
        LedgerKind::Logs => "logs",
    }
}

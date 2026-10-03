//! Per-agent observability and benchmark numbers, folded from events.
//!
//! Outcomes (review decisions, merges, action runs) are credited to the
//! actor that last *submitted* the attempt, not to the reviewer.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::event::{Event, EventBody, ReviewDecision};
use crate::id::{AttemptId, SessionId};
use crate::identity::{Actor, AgentDescriptor};

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ActorStats {
    pub sessions: u32,
    pub subagent_sessions: u32,
    pub tasks_created: u32,
    pub attempts_started: u32,
    pub attempts_submitted: u32,
    pub accepted: u32,
    pub rejected: u32,
    pub changes_requested: u32,
    pub merged: u32,
    pub abandoned: u32,
    pub handoffs: u32,
    pub commits: u32,
    pub lines_added: u64,
    pub lines_removed: u64,
    pub runs: u32,
    pub runs_passed: u32,
    pub reports: u32,
    pub tool_calls: u32,
    pub tool_failures: u32,
}

impl ActorStats {
    /// accepted / (accepted + rejected + changes requested).
    pub fn acceptance_rate(&self) -> Option<f64> {
        let decided = self.accepted + self.rejected + self.changes_requested;
        (decided > 0).then(|| f64::from(self.accepted) / f64::from(decided))
    }

    pub fn run_pass_rate(&self) -> Option<f64> {
        (self.runs > 0).then(|| f64::from(self.runs_passed) / f64::from(self.runs))
    }
}

/// Keyed by `provider/model@client` for agents and `@handle` for humans.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub by_actor: BTreeMap<String, ActorStats>,
}

impl Stats {
    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a Event>) -> Self {
        let mut sorted: Vec<&Event> = events.into_iter().collect();
        sorted.sort_by(|a, b| a.id.cmp(&b.id));

        let mut stats = Self::default();
        let mut session_agents: HashMap<SessionId, AgentDescriptor> = HashMap::new();
        let mut attempt_owner: HashMap<AttemptId, String> = HashMap::new();

        for e in sorted {
            let me = actor_key(&e.actor);
            match &e.body {
                EventBody::SessionStarted(s) => {
                    session_agents.insert(s.session.id.clone(), s.session.agent.clone());
                    let st = stats.entry(s.session.agent.key());
                    st.sessions += 1;
                    if s.session.parent.is_some() {
                        st.subagent_sessions += 1;
                    }
                }
                EventBody::TaskCreated(_) => stats.bump(&me, |s| s.tasks_created += 1),
                EventBody::AttemptStarted(a) => {
                    // A human may start an attempt on behalf of a bound session.
                    let owner = a
                        .session
                        .as_ref()
                        .and_then(|s| session_agents.get(s))
                        .map(AgentDescriptor::key)
                        .or_else(|| me.clone());
                    if let Some(owner) = owner {
                        attempt_owner.insert(a.attempt.clone(), owner.clone());
                        stats.entry(owner).attempts_started += 1;
                    }
                }
                EventBody::AttemptSubmitted(a) => {
                    if let Some(me) = &me {
                        attempt_owner.insert(a.attempt.clone(), me.clone());
                        stats.entry(me.clone()).attempts_submitted += 1;
                    }
                }
                EventBody::AttemptHandoff(_) => stats.bump(&me, |s| s.handoffs += 1),
                EventBody::ReviewDecided(r) => {
                    stats.bump(&attempt_owner.get(&r.attempt).cloned(), |s| match r.decision {
                        ReviewDecision::Accept => s.accepted += 1,
                        ReviewDecision::Reject => s.rejected += 1,
                        ReviewDecision::ChangesRequested => s.changes_requested += 1,
                    })
                }
                EventBody::AttemptMerged(m) => {
                    stats.bump(&attempt_owner.get(&m.attempt).cloned(), |s| s.merged += 1)
                }
                EventBody::AttemptAbandoned(a) => {
                    stats.bump(&attempt_owner.get(&a.attempt).cloned(), |s| s.abandoned += 1)
                }
                EventBody::CommitRecorded(c) => stats.bump(&me, |s| {
                    s.commits += 1;
                    if let Some(d) = c.diff {
                        s.lines_added += u64::from(d.insertions);
                        s.lines_removed += u64::from(d.deletions);
                    }
                }),
                EventBody::ActionCompleted(run) => {
                    let owner = run.attempt.as_ref().and_then(|a| attempt_owner.get(a)).cloned();
                    stats.bump(&owner, |s| {
                        s.runs += 1;
                        if run.status.is_success() {
                            s.runs_passed += 1;
                        }
                    })
                }
                EventBody::Report(_) => stats.bump(&me, |s| s.reports += 1),
                EventBody::ToolCalled(t) => stats.bump(&me, |s| {
                    s.tool_calls += 1;
                    if !t.ok {
                        s.tool_failures += 1;
                    }
                }),
                _ => {}
            }
        }
        stats
    }

    fn entry(&mut self, key: String) -> &mut ActorStats {
        self.by_actor.entry(key).or_default()
    }

    fn bump(&mut self, key: &Option<String>, f: impl FnOnce(&mut ActorStats)) {
        if let Some(k) = key {
            f(self.entry(k.clone()));
        }
    }
}

fn actor_key(actor: &Actor) -> Option<String> {
    match actor {
        Actor::Agent { agent, .. } => Some(agent.key()),
        Actor::Human { handle, .. } => Some(format!("@{handle}")),
        Actor::System { .. } | Actor::Unknown => None,
    }
}

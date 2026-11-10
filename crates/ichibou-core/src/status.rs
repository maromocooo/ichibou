//! Rules that turn evidence into what the UI shows.
//!
//! Nothing here trusts an agent's word. Claims only move a requirement to
//! "in progress" or "needs verification"; the check mark comes from runner
//! verdicts on the current snapshot.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::manifest::Manifest;
use crate::model::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CriterionState {
    /// No scenario covers this criterion.
    NoScenario,
    /// A covering scenario has never been approved by the user.
    AwaitingApproval,
    NotRun,
    /// Passed, but on an older snapshot.
    Stale,
    Passed,
    Failed,
    Error,
    Aborted,
    Skipped,
}

impl CriterionState {
    /// Higher is worse; the worst covering scenario decides the criterion.
    fn severity(self) -> u8 {
        match self {
            CriterionState::Passed => 0,
            CriterionState::Stale => 1,
            CriterionState::NotRun => 2,
            CriterionState::AwaitingApproval => 3,
            CriterionState::NoScenario => 4,
            CriterionState::Skipped => 5,
            CriterionState::Aborted => 6,
            CriterionState::Error => 7,
            CriterionState::Failed => 8,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequirementState {
    NotStarted,
    InProgress,
    NeedsVerification,
    Failing,
    /// Every criterion passed; a human check is still open.
    AwaitingManual,
    /// Every criterion passed; a fix annotation is still open.
    BlockedByAnnotations,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScenarioStatus {
    pub scenario: String,
    pub state: CriterionState,
    /// The working-tree definition differs from the approved one. Runs keep
    /// using the approved definition until the user approves the change.
    pub pending_change: bool,
    pub last_result: Option<String>,
    /// Hash of the definition in the working tree that this status was
    /// computed from. Sent back when the user approves the scenario.
    pub definition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CriterionStatus {
    pub criterion: String,
    pub state: CriterionState,
    pub scenarios: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManualStatus {
    pub check: String,
    pub confirmed: bool,
    /// Confirmed, but on an older snapshot.
    pub stale: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleKind {
    /// An agent is working on it now.
    Now,
    /// First in line.
    Next,
    /// Ready, behind `position - 1` other requirements.
    Queued,
    /// Waiting for the requirements in `waiting_on`.
    Blocked,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Schedule {
    pub kind: ScheduleKind,
    /// 1-based position among requirements that are not done.
    pub position: usize,
    pub waiting_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequirementStatus {
    pub requirement: String,
    pub state: RequirementState,
    pub criteria: Vec<CriterionStatus>,
    pub manual: Vec<ManualStatus>,
    pub open_fixes: Vec<String>,
    /// Run currently claiming this requirement, if any.
    pub claimed_by: Option<String>,
    pub schedule: Schedule,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Only exists in the manifest.
    Declared,
    /// Its source paths exist in the working tree.
    Present,
    /// Every requirement attached to it is done.
    Verified,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArchStatus {
    pub nodes: HashMap<String, Provenance>,
    /// Keyed by `from->to`.
    pub edges: HashMap<String, Provenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectStatus {
    pub snapshot: SnapshotId,
    pub requirements: Vec<RequirementStatus>,
    pub scenarios: Vec<ScenarioStatus>,
    pub architecture: ArchStatus,
    /// True when every requirement is done: the user's request is complete.
    pub complete: bool,
}

pub struct Evidence<'a> {
    pub current: &'a SnapshotId,
    pub results: &'a [VerificationResult],
    pub approvals: &'a [ScenarioApproval],
    /// Scenario id -> definition hash of the working tree.
    pub definitions: &'a HashMap<String, String>,
    /// Scenario id -> the scenario entry the user approved. What an approved
    /// scenario covers is read from here, not from the manifest an agent can
    /// edit: otherwise adding a requirement to a passing scenario's `covers`
    /// would mark that requirement as verified.
    pub approved: &'a HashMap<String, Scenario>,
    /// Hash of the targets file the user currently approves, if any. Only
    /// verdicts produced with exactly that file count: a run under other
    /// commands says nothing about these.
    pub targets: Option<&'a str>,
    pub confirmations: &'a [ManualConfirmation],
    /// Claims of runs that are still alive.
    pub claims: &'a [Claim],
    pub annotations: &'a [Annotation],
}

fn scenario_status(scenario: &Scenario, ev: &Evidence) -> ScenarioStatus {
    let approval = ev.approvals.iter().find(|a| a.scenario == scenario.id);
    let Some(approval) = approval else {
        return ScenarioStatus {
            scenario: scenario.id.clone(),
            state: CriterionState::AwaitingApproval,
            pending_change: false,
            last_result: None,
            definition: ev.definitions.get(&scenario.id).cloned(),
        };
    };
    let pending_change = ev
        .definitions
        .get(&scenario.id)
        .is_some_and(|d| d != &approval.definition);
    // Only verdicts for the approved definition count.
    let latest = ev
        .results
        .iter()
        .filter(|r| {
            r.scenario == scenario.id
                && r.definition == approval.definition
                && ev.targets == Some(r.targets.as_str())
        })
        .max_by_key(|r| r.at);
    let state = match latest {
        None => CriterionState::NotRun,
        Some(r) if &r.snapshot != ev.current => match r.outcome {
            Outcome::Passed => CriterionState::Stale,
            // An old failure says nothing about the current tree.
            _ => CriterionState::NotRun,
        },
        Some(r) => match r.outcome {
            Outcome::Passed => CriterionState::Passed,
            Outcome::Failed => CriterionState::Failed,
            Outcome::Error => CriterionState::Error,
            Outcome::Aborted => CriterionState::Aborted,
            Outcome::Skipped => CriterionState::Skipped,
            Outcome::NotApplicable => CriterionState::NotRun,
        },
    };
    ScenarioStatus {
        scenario: scenario.id.clone(),
        state,
        pending_change,
        last_result: latest.map(|r| r.id.clone()),
        definition: ev.definitions.get(&scenario.id).cloned(),
    }
}

/// Requirement ids in an order that respects `depends_on`, keeping manifest
/// order otherwise. `None` when dependencies form a cycle.
pub fn dependency_order(doc: &RequirementsDoc) -> Option<Vec<String>> {
    let known: HashSet<&str> = doc.requirements.iter().map(|r| r.id.as_str()).collect();
    let mut placed: HashSet<&str> = HashSet::new();
    let mut order = Vec::with_capacity(doc.requirements.len());
    while order.len() < doc.requirements.len() {
        let before = order.len();
        for req in &doc.requirements {
            if placed.contains(req.id.as_str()) {
                continue;
            }
            let ready = req
                .depends_on
                .iter()
                .all(|d| placed.contains(d.as_str()) || !known.contains(d.as_str()));
            if ready {
                placed.insert(req.id.as_str());
                order.push(req.id.clone());
            }
        }
        if order.len() == before {
            return None;
        }
    }
    Some(order)
}

pub fn project_status(
    manifest: &Manifest,
    ev: &Evidence,
    path_exists: &dyn Fn(&str) -> bool,
) -> ProjectStatus {
    let scenarios: Vec<ScenarioStatus> = manifest
        .scenarios
        .scenarios
        .iter()
        .map(|s| scenario_status(s, ev))
        .collect();
    let scenario_state: HashMap<&str, CriterionState> = scenarios
        .iter()
        .map(|s| (s.scenario.as_str(), s.state))
        .collect();

    let mut requirements: Vec<RequirementStatus> = manifest
        .requirements
        .requirements
        .iter()
        .map(|req| requirement_status(req, manifest, ev, &scenario_state))
        .collect();

    apply_schedule(&manifest.requirements, &mut requirements);

    let done: HashSet<&str> = requirements
        .iter()
        .filter(|r| r.state == RequirementState::Done)
        .map(|r| r.requirement.as_str())
        .collect();
    let all_done = |ids: &[String]| !ids.is_empty() && ids.iter().all(|id| done.contains(id.as_str()));

    let mut nodes = HashMap::new();
    for node in &manifest.architecture.nodes {
        let present = !node.paths.is_empty() && node.paths.iter().all(|p| path_exists(p));
        let state = if all_done(&node.requirements) && (present || node.paths.is_empty()) {
            Provenance::Verified
        } else if present {
            Provenance::Present
        } else {
            Provenance::Declared
        };
        nodes.insert(node.id.clone(), state);
    }
    let mut edges = HashMap::new();
    for edge in &manifest.architecture.edges {
        let end = |id: &str| nodes.get(id).copied().unwrap_or(Provenance::Declared);
        let (from, to) = (end(&edge.from), end(&edge.to));
        let state = if all_done(&edge.requirements) {
            Provenance::Verified
        } else if from != Provenance::Declared && to != Provenance::Declared {
            Provenance::Present
        } else {
            Provenance::Declared
        };
        edges.insert(format!("{}->{}", edge.from, edge.to), state);
    }

    let complete = !requirements.is_empty()
        && requirements
            .iter()
            .all(|r| r.state == RequirementState::Done);

    ProjectStatus {
        snapshot: ev.current.clone(),
        requirements,
        scenarios,
        architecture: ArchStatus { nodes, edges },
        complete,
    }
}

fn requirement_status(
    req: &Requirement,
    manifest: &Manifest,
    ev: &Evidence,
    scenario_state: &HashMap<&str, CriterionState>,
) -> RequirementStatus {
    let criteria: Vec<CriterionStatus> = req
        .criteria
        .iter()
        .map(|criterion| {
            let covering: Vec<&Scenario> = manifest
                .scenarios
                .scenarios
                .iter()
                .filter(|s| {
                    ev.approved
                        .get(&s.id)
                        .unwrap_or(s)
                        .covers
                        .iter()
                        .any(|c| c.requirement == req.id && c.criterion == criterion.id)
                })
                .collect();
            let state = covering
                .iter()
                .map(|s| scenario_state[s.id.as_str()])
                .max_by_key(|s| s.severity())
                .unwrap_or(CriterionState::NoScenario);
            CriterionStatus {
                criterion: criterion.id.clone(),
                state,
                scenarios: covering.iter().map(|s| s.id.clone()).collect(),
            }
        })
        .collect();

    let manual: Vec<ManualStatus> = req
        .manual_checks
        .iter()
        .map(|check| {
            let mine = || {
                ev.confirmations
                    .iter()
                    .filter(|c| c.requirement == req.id && c.check == check.id)
            };
            let confirmed = mine().any(|c| &c.snapshot == ev.current);
            ManualStatus {
                check: check.id.clone(),
                confirmed,
                stale: !confirmed && mine().next().is_some(),
            }
        })
        .collect();

    let open_fixes: Vec<String> = ev
        .annotations
        .iter()
        .filter(|a| {
            a.intent == AnnotationIntent::Fix
                && a.state != AnnotationState::Resolved
                && a.requirements.iter().any(|r| r == &req.id)
        })
        .map(|a| a.id.clone())
        .collect();

    let claim = ev.claims.iter().find(|c| c.requirement == req.id);
    let working = claim.is_some_and(|c| !c.reported_done);

    let has = |state: CriterionState| criteria.iter().any(|c| c.state == state);
    // A requirement without criteria can never be verified, so never done.
    let all_passed =
        !criteria.is_empty() && criteria.iter().all(|c| c.state == CriterionState::Passed);
    let touched = claim.is_some()
        || criteria.iter().any(|c| {
            matches!(
                c.state,
                CriterionState::Passed
                    | CriterionState::Stale
                    | CriterionState::Aborted
                    | CriterionState::Skipped
            )
        });

    let state = if all_passed {
        if !open_fixes.is_empty() {
            RequirementState::BlockedByAnnotations
        } else if manual.iter().any(|m| !m.confirmed) {
            RequirementState::AwaitingManual
        } else {
            RequirementState::Done
        }
    } else if working {
        RequirementState::InProgress
    } else if has(CriterionState::Failed) || has(CriterionState::Error) {
        RequirementState::Failing
    } else if touched {
        RequirementState::NeedsVerification
    } else {
        RequirementState::NotStarted
    };

    RequirementStatus {
        requirement: req.id.clone(),
        state,
        criteria,
        manual,
        open_fixes,
        claimed_by: claim.map(|c| c.run.clone()),
        schedule: Schedule {
            kind: ScheduleKind::Queued,
            position: 0,
            waiting_on: Vec::new(),
        },
    }
}

/// Answers "when will this be worked on" with order and dependencies, not
/// clock time: a time estimate with no data behind it would be noise.
fn apply_schedule(doc: &RequirementsDoc, statuses: &mut [RequirementStatus]) {
    let order = dependency_order(doc)
        .unwrap_or_else(|| doc.requirements.iter().map(|r| r.id.clone()).collect());
    let done: HashSet<String> = statuses
        .iter()
        .filter(|s| s.state == RequirementState::Done)
        .map(|s| s.requirement.clone())
        .collect();
    let deps: HashMap<&str, &Vec<String>> = doc
        .requirements
        .iter()
        .map(|r| (r.id.as_str(), &r.depends_on))
        .collect();

    let mut position = 0;
    let mut next_given = false;
    for id in &order {
        let Some(status) = statuses.iter_mut().find(|s| &s.requirement == id) else {
            continue;
        };
        if status.state == RequirementState::Done {
            status.schedule.kind = ScheduleKind::Done;
            continue;
        }
        position += 1;
        status.schedule.position = position;
        let waiting_on: Vec<String> = deps
            .get(id.as_str())
            .map(|d| d.iter().filter(|d| !done.contains(*d)).cloned().collect())
            .unwrap_or_default();
        status.schedule.kind = if status.state == RequirementState::InProgress {
            ScheduleKind::Now
        } else if !waiting_on.is_empty() {
            ScheduleKind::Blocked
        } else if !next_given {
            next_given = true;
            ScheduleKind::Next
        } else {
            ScheduleKind::Queued
        };
        status.schedule.waiting_on = waiting_on;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn manifest() -> Manifest {
        let req = |id: &str, deps: &[&str]| Requirement {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
            criteria: vec![Criterion {
                id: "c1".into(),
                text: "works".into(),
            }],
            manual_checks: vec![],
        };
        let scenario = |id: &str, req: &str| Scenario {
            id: id.into(),
            title: id.into(),
            target: "web".into(),
            runner: RunnerKind::Playwright,
            covers: vec![Coverage {
                requirement: req.into(),
                criterion: "c1".into(),
            }],
            files: vec![],
            command: None,
        };
        Manifest {
            requirements: RequirementsDoc {
                requirements: vec![req("A", &[]), req("B", &["A"]), req("C", &[])],
            },
            scenarios: ScenariosDoc {
                scenarios: vec![scenario("sa", "A"), scenario("sb", "B"), scenario("sc", "C")],
            },
            architecture: ArchitectureDoc {
                nodes: vec![ArchNode {
                    id: "api".into(),
                    kind: NodeKind::Api,
                    title: "API".into(),
                    description: String::new(),
                    paths: vec!["server".into()],
                    requirements: vec!["A".into()],
                }],
                edges: vec![],
            },
            ..Default::default()
        }
    }

    struct World {
        current: SnapshotId,
        results: Vec<VerificationResult>,
        approvals: Vec<ScenarioApproval>,
        definitions: HashMap<String, String>,
        approved: HashMap<String, Scenario>,
        targets: Option<String>,
        confirmations: Vec<ManualConfirmation>,
        claims: Vec<Claim>,
        annotations: Vec<Annotation>,
    }

    impl World {
        fn new() -> Self {
            let approvals = ["sa", "sb", "sc"]
                .iter()
                .map(|s| ScenarioApproval {
                    scenario: s.to_string(),
                    definition: "d1".into(),
                    at: Utc::now(),
                })
                .collect();
            World {
                current: SnapshotId("s1".into()),
                results: vec![],
                approvals,
                definitions: HashMap::new(),
                approved: HashMap::new(),
                targets: Some("t1".into()),
                confirmations: vec![],
                claims: vec![],
                annotations: vec![],
            }
        }

        fn result(&mut self, scenario: &str, outcome: Outcome, snapshot: &str, definition: &str) {
            let at = Utc::now() + Duration::seconds(self.results.len() as i64);
            self.results.push(VerificationResult {
                id: format!("r{}", self.results.len()),
                scenario: scenario.into(),
                outcome,
                snapshot: SnapshotId(snapshot.into()),
                definition: definition.into(),
                targets: "t1".into(),
                at,
                detail: String::new(),
                artifacts: None,
            });
        }

        fn status(&self, manifest: &Manifest) -> ProjectStatus {
            let ev = Evidence {
                current: &self.current,
                results: &self.results,
                approvals: &self.approvals,
                definitions: &self.definitions,
                approved: &self.approved,
                targets: self.targets.as_deref(),
                confirmations: &self.confirmations,
                claims: &self.claims,
                annotations: &self.annotations,
            };
            project_status(manifest, &ev, &|_| true)
        }
    }

    fn state(status: &ProjectStatus, id: &str) -> RequirementState {
        status
            .requirements
            .iter()
            .find(|r| r.requirement == id)
            .unwrap()
            .state
    }

    #[test]
    fn pass_on_current_snapshot_is_done() {
        let m = manifest();
        let mut w = World::new();
        w.result("sa", Outcome::Passed, "s1", "d1");
        let s = w.status(&m);
        assert_eq!(state(&s, "A"), RequirementState::Done);
        assert_eq!(s.architecture.nodes["api"], Provenance::Verified);
        assert!(!s.complete);
    }

    #[test]
    fn any_change_demotes_a_pass() {
        let m = manifest();
        let mut w = World::new();
        w.result("sa", Outcome::Passed, "s1", "d1");
        w.current = SnapshotId("s2".into());
        let s = w.status(&m);
        assert_eq!(state(&s, "A"), RequirementState::NeedsVerification);
        assert_eq!(s.requirements[0].criteria[0].state, CriterionState::Stale);
        assert_eq!(s.architecture.nodes["api"], Provenance::Present);
    }

    #[test]
    fn agent_claim_never_completes() {
        let m = manifest();
        let mut w = World::new();
        w.claims.push(Claim {
            run: "run1".into(),
            requirement: "A".into(),
            note: String::new(),
            reported_done: true,
            at: Utc::now(),
        });
        assert_eq!(state(&w.status(&m), "A"), RequirementState::NeedsVerification);
        w.claims[0].reported_done = false;
        assert_eq!(state(&w.status(&m), "A"), RequirementState::InProgress);
    }

    #[test]
    fn only_passed_counts() {
        let m = manifest();
        for outcome in [
            Outcome::Failed,
            Outcome::Error,
            Outcome::Aborted,
            Outcome::Skipped,
            Outcome::NotApplicable,
        ] {
            let mut w = World::new();
            w.result("sa", outcome, "s1", "d1");
            assert_ne!(state(&w.status(&m), "A"), RequirementState::Done, "{outcome:?}");
        }
    }

    #[test]
    fn unapproved_or_changed_definition_does_not_count() {
        let m = manifest();
        let mut w = World::new();
        // A pass produced with a definition the user never approved.
        w.result("sa", Outcome::Passed, "s1", "d2");
        assert_eq!(state(&w.status(&m), "A"), RequirementState::NotStarted);

        // The agent edited the test: flagged, and the approved definition still rules.
        w.result("sa", Outcome::Passed, "s1", "d1");
        w.definitions.insert("sa".into(), "d2".into());
        let s = w.status(&m);
        assert_eq!(state(&s, "A"), RequirementState::Done);
        assert!(s.scenarios[0].pending_change);

        w.approvals.retain(|a| a.scenario != "sa");
        assert_ne!(state(&w.status(&m), "A"), RequirementState::Done);
    }

    #[test]
    fn coverage_comes_from_the_approved_scenario() {
        let mut m = manifest();
        let mut w = World::new();
        // The user approved `sa` as covering A only, and it passes.
        w.approved.insert("sa".into(), m.scenarios.scenarios[0].clone());
        w.result("sa", Outcome::Passed, "s1", "d1");
        // An agent edits the manifest so `sa` also claims to cover C, and
        // drops C's own scenario.
        m.scenarios.scenarios[0].covers.push(Coverage {
            requirement: "C".into(),
            criterion: "c1".into(),
        });
        m.scenarios.scenarios.retain(|s| s.id != "sc");
        w.definitions.insert("sa".into(), "d2".into());
        let s = w.status(&m);
        assert_eq!(state(&s, "A"), RequirementState::Done);
        assert_ne!(state(&s, "C"), RequirementState::Done);
        assert_eq!(
            s.requirements[2].criteria[0].state,
            CriterionState::NoScenario
        );
    }

    #[test]
    fn a_pass_only_counts_under_the_targets_it_ran_with() {
        let m = manifest();
        let mut w = World::new();
        w.result("sa", Outcome::Passed, "s1", "d1");
        assert_eq!(state(&w.status(&m), "A"), RequirementState::Done);
        // The targets file changed and was approved again: other commands.
        w.targets = Some("t2".into());
        assert_ne!(state(&w.status(&m), "A"), RequirementState::Done);
        // Nothing approved at all: nothing counts.
        w.targets = None;
        assert_ne!(state(&w.status(&m), "A"), RequirementState::Done);
    }

    #[test]
    fn newer_failure_overrides_older_pass() {
        let m = manifest();
        let mut w = World::new();
        w.result("sa", Outcome::Passed, "s1", "d1");
        w.result("sa", Outcome::Failed, "s1", "d1");
        assert_eq!(state(&w.status(&m), "A"), RequirementState::Failing);
    }

    #[test]
    fn open_fix_and_manual_checks_block_done() {
        let mut m = manifest();
        m.requirements.requirements[0].manual_checks.push(ManualCheck {
            id: "m1".into(),
            text: "looks right".into(),
        });
        let mut w = World::new();
        w.result("sa", Outcome::Passed, "s1", "d1");
        assert_eq!(state(&w.status(&m), "A"), RequirementState::AwaitingManual);

        w.confirmations.push(ManualConfirmation {
            requirement: "A".into(),
            check: "m1".into(),
            snapshot: SnapshotId("s0".into()),
            at: Utc::now(),
        });
        let s = w.status(&m);
        assert_eq!(state(&s, "A"), RequirementState::AwaitingManual);
        assert!(s.requirements[0].manual[0].stale);

        w.confirmations[0].snapshot = SnapshotId("s1".into());
        assert_eq!(state(&w.status(&m), "A"), RequirementState::Done);

        w.annotations.push(Annotation {
            id: "a1".into(),
            intent: AnnotationIntent::Fix,
            state: AnnotationState::Addressed,
            anchor: Anchor::Requirement {
                requirement: "A".into(),
            },
            body: "fix".into(),
            snapshot: SnapshotId("s1".into()),
            requirements: vec!["A".into()],
            image: None,
            created_at: Utc::now(),
            comments: vec![],
        });
        assert_eq!(state(&w.status(&m), "A"), RequirementState::BlockedByAnnotations);
        w.annotations[0].state = AnnotationState::Resolved;
        assert_eq!(state(&w.status(&m), "A"), RequirementState::Done);
    }

    #[test]
    fn schedule_uses_order_and_dependencies() {
        let m = manifest();
        let mut w = World::new();
        let s = w.status(&m);
        let kinds: Vec<_> = s.requirements.iter().map(|r| r.schedule.kind).collect();
        assert_eq!(
            kinds,
            vec![ScheduleKind::Next, ScheduleKind::Blocked, ScheduleKind::Queued]
        );
        assert_eq!(s.requirements[1].schedule.waiting_on, vec!["A".to_string()]);

        w.result("sa", Outcome::Passed, "s1", "d1");
        let s = w.status(&m);
        assert_eq!(s.requirements[0].schedule.kind, ScheduleKind::Done);
        assert_eq!(s.requirements[1].schedule.kind, ScheduleKind::Next);
        assert_eq!(s.requirements[1].schedule.position, 1);
    }

    #[test]
    fn complete_needs_every_requirement() {
        let m = manifest();
        let mut w = World::new();
        for s in ["sa", "sb", "sc"] {
            w.result(s, Outcome::Passed, "s1", "d1");
        }
        assert!(w.status(&m).complete);
    }

    #[test]
    fn cycle_is_detected() {
        let mut m = manifest();
        m.requirements.requirements[0].depends_on = vec!["B".into()];
        assert!(dependency_order(&m.requirements).is_none());
    }
}

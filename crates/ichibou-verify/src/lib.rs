//! The verification runner: the only producer of pass/fail evidence.
//!
//! A run never tests the working tree an agent may be editing. It copies the
//! current snapshot to a private directory, restores the scenario files the
//! user approved, builds and starts the app there, and runs the scenarios
//! against that instance. Every verdict records the snapshot and the scenario
//! definition it applies to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ichibou_core::manifest::{Manifest, Project};
use ichibou_core::store::{FrozenScenario, Store};
use ichibou_core::*;
use chrono::Utc;

pub mod process;
pub mod report;

use process::Background;

const SETUP_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const START_TIMEOUT: Duration = Duration::from_secs(120);
/// Results kept per scenario before old ones (and their artifacts) are removed.
const KEEP_RESULTS: usize = 20;

pub struct Verifier {
    pub project: Project,
    pub store: Arc<Store>,
}

#[derive(Debug)]
pub struct Report {
    pub snapshot: SnapshotId,
    pub results: Vec<VerificationResult>,
}

/// The workspace kept changing while it was being copied. Not a verdict:
/// the caller should try again once the agent is quiet.
#[derive(Debug)]
pub struct Unstable;

impl std::fmt::Display for Unstable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the workspace changed while it was being copied")
    }
}

impl std::error::Error for Unstable {}

struct Approved {
    frozen: FrozenScenario,
    definition: String,
}

impl Verifier {
    /// Runs every approved scenario, or only those in `only`.
    pub async fn run(&self, only: Option<&[String]>) -> Result<Report> {
        let manifest = self.project.load()?;
        let approvals = self.store.approvals()?;
        let mut approved = Vec::new();
        for approval in approvals {
            if only.is_some_and(|ids| !ids.contains(&approval.scenario)) {
                continue;
            }
            // A scenario removed from the manifest no longer covers anything.
            if !manifest
                .scenarios
                .scenarios
                .iter()
                .any(|s| s.id == approval.scenario)
            {
                continue;
            }
            if let Some(frozen) = self.store.frozen_scenario(&approval.scenario)? {
                approved.push(Approved {
                    frozen,
                    definition: approval.definition,
                });
            }
        }

        let root = self.project.root.clone();
        let work = self.project.local_dir().join("verify");
        let extras: Vec<String> = manifest
            .targets
            .targets
            .iter()
            .filter_map(|t| t.verify.as_ref())
            .flat_map(|v| v.copy.iter().cloned())
            .collect();
        let snapshot = tokio::task::spawn_blocking({
            let (root, work) = (root.clone(), work.clone());
            move || copy_stable(&root, &work, &extras)
        })
        .await??;

        if approved.is_empty() {
            return Ok(Report {
                snapshot,
                results: vec![],
            });
        }

        for scenario in &approved {
            restore_approved_files(&work, &scenario.frozen)?;
        }

        let mut by_target: BTreeMap<String, Vec<&Approved>> = BTreeMap::new();
        for scenario in &approved {
            by_target
                .entry(scenario.frozen.scenario.target.clone())
                .or_default()
                .push(scenario);
        }

        let mut results = Vec::new();
        for (target, scenarios) in by_target {
            results.extend(
                self.run_target(&manifest, &target, &scenarios, &work, &snapshot)
                    .await,
            );
        }
        for result in &results {
            self.store.add_result(result)?;
        }
        for dir in self.store.prune_results(KEEP_RESULTS)? {
            let _ = std::fs::remove_dir_all(self.project.local_dir().join(dir));
        }
        Ok(Report { snapshot, results })
    }

    fn new_result(&self) -> Result<(String, PathBuf, String)> {
        let id = uuid::Uuid::new_v4().to_string();
        let relative = format!("artifacts/{id}");
        let dir = self.project.local_dir().join(&relative);
        std::fs::create_dir_all(&dir)?;
        Ok((id, dir, relative))
    }

    async fn run_target(
        &self,
        manifest: &Manifest,
        target_name: &str,
        scenarios: &[&Approved],
        work: &Path,
        snapshot: &SnapshotId,
    ) -> Vec<VerificationResult> {
        let fail_all = |detail: String| -> Vec<VerificationResult> {
            scenarios
                .iter()
                .map(|s| VerificationResult {
                    id: uuid::Uuid::new_v4().to_string(),
                    scenario: s.frozen.scenario.id.clone(),
                    outcome: Outcome::Error,
                    snapshot: snapshot.clone(),
                    definition: s.definition.clone(),
                    at: Utc::now(),
                    detail: detail.clone(),
                    artifacts: None,
                })
                .collect()
        };

        let Some(target) = manifest.targets.targets.iter().find(|t| t.name == target_name) else {
            return fail_all(format!("target `{target_name}` is not defined in targets.yaml"));
        };
        let env_spec = target.verify.clone().unwrap_or_default();
        let launch = env_spec.start.clone().or_else(|| target.dev.clone());
        let setup_log = work.join(".ichibou-verify.log");

        for dir in &env_spec.link {
            // Only directories inside the project are linked, and only to
            // the same relative place inside the copy.
            let Some(from) = ichibou_core::manifest::resolve_inside(&self.project.root, dir) else {
                continue;
            };
            let to = work.join(dir);
            if from.is_dir() && !to.exists() {
                if let Some(parent) = to.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                #[cfg(unix)]
                let _ = std::os::unix::fs::symlink(&from, &to);
            }
        }

        for command in &env_spec.setup {
            match process::run(command, work, &[], &setup_log, SETUP_TIMEOUT).await {
                Ok(Some(0)) => {}
                Ok(Some(code)) => {
                    return fail_all(format!(
                        "setup `{command}` exited with {code}\n{}",
                        tail(&setup_log)
                    ))
                }
                Ok(None) => return fail_all(format!("setup `{command}` timed out")),
                Err(err) => return fail_all(format!("setup `{command}` failed: {err:#}")),
            }
        }

        let mut env: Vec<(String, String)> = Vec::new();
        let mut app: Option<Background> = None;
        if let Some(launch) = &launch {
            let port = match process::free_port() {
                Ok(port) => port,
                Err(err) => return fail_all(format!("no free port: {err}")),
            };
            let fill = |text: &str| text.replace("{port}", &port.to_string());
            env.push(("PORT".into(), port.to_string()));
            let cwd = match &launch.cwd {
                Some(cwd) if ichibou_core::manifest::is_safe_relative(cwd) => work.join(cwd),
                Some(cwd) => {
                    return fail_all(format!("`cwd: {cwd}` must be a relative path inside the project"))
                }
                None => work.to_path_buf(),
            };
            let mut process = match Background::spawn(&fill(&launch.command), &cwd, &env, &setup_log)
            {
                Ok(process) => process,
                Err(err) => return fail_all(format!("cannot start the app: {err:#}")),
            };
            if let Some(url) = &launch.url {
                let url = fill(url);
                if !process::wait_for_url(&url, START_TIMEOUT, &mut process).await {
                    process.stop().await;
                    return fail_all(format!(
                        "the app did not answer on {url}\n{}",
                        tail(&setup_log)
                    ));
                }
                env.push(("ICHIBOU_BASE_URL".into(), url.clone()));
                env.push(("BASE_URL".into(), url));
            }
            app = Some(process);
        }

        let mut results = Vec::new();
        for scenario in scenarios {
            results.push(self.run_scenario(scenario, work, &env, snapshot).await);
        }
        if let Some(app) = app {
            app.stop().await;
        }
        results
    }

    async fn run_scenario(
        &self,
        approved: &Approved,
        work: &Path,
        env: &[(String, String)],
        snapshot: &SnapshotId,
    ) -> VerificationResult {
        let scenario = &approved.frozen.scenario;
        let finish = |id: String, outcome: Outcome, detail: String, artifacts: Option<String>| {
            VerificationResult {
                id,
                scenario: scenario.id.clone(),
                outcome,
                snapshot: snapshot.clone(),
                definition: approved.definition.clone(),
                at: Utc::now(),
                detail,
                artifacts,
            }
        };
        let (id, dir, relative) = match self.new_result() {
            Ok(parts) => parts,
            Err(err) => {
                return finish(
                    uuid::Uuid::new_v4().to_string(),
                    Outcome::Error,
                    format!("{err:#}"),
                    None,
                )
            }
        };
        let log = dir.join("log.txt");
        let files = scenario
            .files
            .iter()
            .map(|f| shell_quote(f))
            .collect::<Vec<_>>()
            .join(" ");
        let mut env = env.to_vec();
        let base_url = env
            .iter()
            .find(|(k, _)| k == "ICHIBOU_BASE_URL")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        let (command, report_path, parse): (String, Option<PathBuf>, fn(&str) -> Option<report::Counts>) =
            match scenario.runner {
                RunnerKind::Playwright => {
                    let report = dir.join("report.json");
                    env.push((
                        "PLAYWRIGHT_JSON_OUTPUT_NAME".into(),
                        report.to_string_lossy().into_owned(),
                    ));
                    env.push((
                        "PLAYWRIGHT_HTML_OPEN".into(),
                        "never".into(),
                    ));
                    (
                        format!(
                            "npx playwright test {files} --reporter=json --output {}",
                            shell_quote(&dir.join("playwright").to_string_lossy())
                        ),
                        Some(report),
                        report::playwright,
                    )
                }
                RunnerKind::Maestro => {
                    let report = dir.join("report.xml");
                    (
                        format!(
                            "maestro test {files} --format junit --output {}",
                            shell_quote(&report.to_string_lossy())
                        ),
                        Some(report),
                        report::junit,
                    )
                }
                RunnerKind::Command => {
                    let Some(template) = &scenario.command else {
                        return finish(
                            id,
                            Outcome::Error,
                            "a `command` scenario needs a command".into(),
                            Some(relative),
                        );
                    };
                    let report = dir.join("report.xml");
                    let uses_report = template.contains("{report}");
                    (
                        template
                            .replace("{report}", &shell_quote(&report.to_string_lossy()))
                            .replace("{url}", &base_url),
                        uses_report.then_some(report),
                        report::junit,
                    )
                }
            };

        let code = match process::run(&command, work, &env, &log, SCENARIO_TIMEOUT).await {
            Ok(code) => code,
            Err(err) => return finish(id, Outcome::Error, format!("{err:#}"), Some(relative)),
        };
        let Some(code) = code else {
            return finish(id, Outcome::Aborted, "timed out".into(), Some(relative));
        };

        let (outcome, mut detail) = match &report_path {
            Some(path) => match std::fs::read_to_string(path).ok().and_then(|t| parse(&t)) {
                Some(counts) => {
                    let mut outcome = counts.outcome();
                    // A report that says "pass" from a runner that exited
                    // non-zero is not trusted.
                    if outcome == Outcome::Passed && code != 0 {
                        outcome = Outcome::Error;
                    }
                    (outcome, counts.describe())
                }
                None => (
                    Outcome::Error,
                    format!("the runner exited with {code} and wrote no readable report"),
                ),
            },
            None => (
                if code == 0 {
                    Outcome::Passed
                } else {
                    Outcome::Failed
                },
                format!("exit code {code}"),
            ),
        };
        if outcome != Outcome::Passed {
            detail = format!("{detail}\n{}", tail(&log));
        }
        finish(id, outcome, detail, Some(relative))
    }
}

/// Copies the snapshot and checks the copy matches what was hashed. Retries
/// a few times because an agent may be mid-edit.
fn copy_stable(root: &Path, work: &Path, extras: &[String]) -> Result<SnapshotId> {
    for _ in 0..3 {
        if work.exists() {
            std::fs::remove_dir_all(work).context("cannot clear the verification directory")?;
        }
        let expected = snapshot::take(root)?;
        let copied = snapshot::materialize(root, work, extras)?;
        if copied == expected && snapshot::take(root)? == expected {
            return Ok(expected);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!(Unstable)
}

/// Puts the approved version of each scenario file into the copy, so an
/// agent's edit to a test cannot change what is being judged.
fn restore_approved_files(work: &Path, frozen: &FrozenScenario) -> Result<()> {
    for (path, content) in &frozen.files {
        // Never write outside the copy, whatever a stored path says.
        if !ichibou_core::manifest::is_safe_relative(path) {
            bail!("approved scenario file `{path}` is not a path inside the project");
        }
        let full = work.join(path);
        match content {
            Some(bytes) => {
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&full, bytes)?;
            }
            None => {
                let _ = std::fs::remove_file(&full);
            }
        }
    }
    Ok(())
}

/// Reads the scenario's current files once and returns the approval record
/// together with the frozen copy; the definition hash is computed from the
/// very bytes that are frozen.
pub fn freeze(project: &Project, scenario: &Scenario) -> Result<(ScenarioApproval, FrozenScenario)> {
    let files = ichibou_core::manifest::read_scenario_files(&project.root, scenario);
    let approval = ScenarioApproval {
        scenario: scenario.id.clone(),
        definition: ichibou_core::manifest::definition_of(scenario, &files)?,
        at: Utc::now(),
    };
    Ok((
        approval,
        FrozenScenario {
            scenario: scenario.clone(),
            files,
        },
    ))
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(3000);
    chars[start..].iter().collect()
}

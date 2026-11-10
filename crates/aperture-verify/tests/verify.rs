use std::fs;
use std::sync::Arc;

use aperture_core::manifest::Project;
use aperture_core::status::{project_status, Evidence, RequirementState};
use aperture_core::store::Store;
use aperture_core::*;
use aperture_verify::{freeze, Verifier};

/// Keeps test state out of the real per-user data directory.
fn isolate_state() {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().unwrap());
    std::env::set_var("APERTURE_STATE_DIR", dir.path());
}

struct Fixture {
    _dir: tempfile::TempDir,
    project: Project,
    store: Arc<Store>,
    scenario: Scenario,
}

/// A project whose single requirement is checked by a shell command that
/// writes a JUnit report: it passes when `answer.txt` contains 42.
fn fixture() -> Fixture {
    isolate_state();
    let dir = tempfile::tempdir().unwrap();
    let project = Project::new(dir.path());
    project.init().unwrap();
    fs::write(dir.path().join("answer.txt"), "42\n").unwrap();
    fs::write(
        dir.path().join("check.sh"),
        r#"if [ "$(cat answer.txt)" = "42" ]; then
  echo '<testsuite><testcase name="answer"/></testsuite>' > "$1"
else
  echo '<testsuite><testcase name="answer"><failure>wrong</failure></testcase></testsuite>' > "$1"
  exit 1
fi
"#,
    )
    .unwrap();
    project
        .save_requirements(&RequirementsDoc {
            requirements: vec![Requirement {
                id: "REQ-1".into(),
                title: "Answer".into(),
                description: String::new(),
                depends_on: vec![],
                criteria: vec![Criterion {
                    id: "c1".into(),
                    text: "answer is 42".into(),
                }],
                manual_checks: vec![],
            }],
        })
        .unwrap();
    project
        .save_targets(&TargetsDoc {
            targets: vec![Target {
                name: "cli".into(),
                kind: TargetKind::Desktop,
                dev: None,
                verify: None,
                app_id: None,
            }],
            ..Default::default()
        })
        .unwrap();
    let scenario = Scenario {
        id: "answer".into(),
        title: "Answer".into(),
        target: "cli".into(),
        runner: RunnerKind::Command,
        covers: vec![Coverage {
            requirement: "REQ-1".into(),
            criterion: "c1".into(),
        }],
        files: vec!["check.sh".into()],
        command: Some("sh check.sh {report}".into()),
    };
    project
        .save_scenarios(&ScenariosDoc {
            scenarios: vec![scenario.clone()],
        })
        .unwrap();
    let store = Arc::new(Store::open(&project.local_dir().join("state.db")).unwrap());
    Fixture {
        _dir: dir,
        project,
        store,
        scenario,
    }
}

fn state(f: &Fixture) -> RequirementState {
    let manifest = f.project.load().unwrap();
    let current = snapshot::take(&f.project.root).unwrap();
    let definitions = manifest
        .scenarios
        .scenarios
        .iter()
        .map(|s| (s.id.clone(), f.project.scenario_definition(s).unwrap()))
        .collect();
    let ev = Evidence {
        current: &current,
        results: &f.store.results().unwrap(),
        approvals: &f.store.approvals().unwrap(),
        definitions: &definitions,
        confirmations: &[],
        claims: &[],
        annotations: &[],
    };
    project_status(&manifest, &ev, &|_| true).requirements[0].state
}

fn verifier(f: &Fixture) -> Verifier {
    Verifier {
        project: f.project.clone(),
        store: f.store.clone(),
    }
}

#[tokio::test]
async fn unapproved_scenarios_are_not_run() {
    let f = fixture();
    let report = verifier(&f).run(None).await.unwrap();
    assert!(report.results.is_empty());
    assert_eq!(state(&f), RequirementState::NotStarted);
}

#[tokio::test]
async fn pass_then_edit_demotes_then_reverify_fails() {
    let f = fixture();
    let (approval, frozen) = freeze(&f.project, &f.scenario).unwrap();
    f.store.approve_scenario(&approval, &frozen).unwrap();

    let report = verifier(&f).run(None).await.unwrap();
    assert_eq!(report.results[0].outcome, Outcome::Passed, "{}", report.results[0].detail);
    assert_eq!(report.snapshot, snapshot::take(&f.project.root).unwrap());
    assert_eq!(state(&f), RequirementState::Done);

    // Any edit invalidates the evidence until it is verified again.
    fs::write(f.project.root.join("answer.txt"), "41\n").unwrap();
    assert_eq!(state(&f), RequirementState::NeedsVerification);

    let report = verifier(&f).run(None).await.unwrap();
    assert_eq!(report.results[0].outcome, Outcome::Failed);
    assert!(report.results[0].detail.contains("1 failed"));
    assert_eq!(state(&f), RequirementState::Failing);
}

#[tokio::test]
async fn an_agent_cannot_pass_by_weakening_the_approved_test() {
    let f = fixture();
    let (approval, frozen) = freeze(&f.project, &f.scenario).unwrap();
    f.store.approve_scenario(&approval, &frozen).unwrap();

    // Break the implementation, then rewrite the test so it always passes.
    fs::write(f.project.root.join("answer.txt"), "0\n").unwrap();
    fs::write(
        f.project.root.join("check.sh"),
        "echo '<testsuite><testcase name=\"answer\"/></testsuite>' > \"$1\"\n",
    )
    .unwrap();

    let report = verifier(&f).run(None).await.unwrap();
    // The approved check ran, not the rewritten one.
    assert_eq!(report.results[0].outcome, Outcome::Failed);
    assert_eq!(report.results[0].definition, approval.definition);
    assert_eq!(state(&f), RequirementState::Failing);

    // Once the user approves the new definition, it is what gets judged.
    let (approval, frozen) = freeze(&f.project, &f.scenario).unwrap();
    f.store.approve_scenario(&approval, &frozen).unwrap();
    let report = verifier(&f).run(None).await.unwrap();
    assert_eq!(report.results[0].outcome, Outcome::Passed);
}

#[tokio::test]
async fn app_is_started_from_the_copy_and_stopped() {
    let f = fixture();
    // The app serves the copied directory; the scenario fetches a file from it.
    let mut targets = f.project.load().unwrap().targets;
    targets.targets[0].verify = Some(VerifyEnv {
        setup: vec!["echo built > built.txt".into()],
        start: Some(Launch {
            command: "python3 -m http.server {port} --bind 127.0.0.1".into(),
            cwd: None,
            url: Some("http://127.0.0.1:{port}/built.txt".into()),
            port: None,
        }),
        copy: vec![],
        link: vec![],
    });
    f.project.save_targets(&targets).unwrap();
    let mut scenario = f.scenario.clone();
    scenario.command = Some(
        "curl -sf \"$APERTURE_BASE_URL\" | grep -q built && sh check.sh {report}".into(),
    );
    f.project
        .save_scenarios(&ScenariosDoc {
            scenarios: vec![scenario.clone()],
        })
        .unwrap();
    let (approval, frozen) = freeze(&f.project, &scenario).unwrap();
    f.store.approve_scenario(&approval, &frozen).unwrap();

    let report = verifier(&f).run(None).await.unwrap();
    assert_eq!(report.results[0].outcome, Outcome::Passed, "{}", report.results[0].detail);
    // Setup output stays in the copy, not in the user's working tree.
    assert!(!f.project.root.join("built.txt").exists());
}

#[tokio::test]
async fn broken_setup_is_an_error_not_a_failure_or_pass() {
    let f = fixture();
    let mut targets = f.project.load().unwrap().targets;
    targets.targets[0].verify = Some(VerifyEnv {
        setup: vec!["echo nope >&2; exit 3".into()],
        ..Default::default()
    });
    f.project.save_targets(&targets).unwrap();
    let (approval, frozen) = freeze(&f.project, &f.scenario).unwrap();
    f.store.approve_scenario(&approval, &frozen).unwrap();
    let report = verifier(&f).run(None).await.unwrap();
    assert_eq!(report.results[0].outcome, Outcome::Error);
    assert!(report.results[0].detail.contains("nope"));
}

use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use ichibou_core::manifest::Project;
use ichibou_core::status::{project_status, Evidence, RequirementState};
use ichibou_core::store::Store;
use ichibou_core::*;
use ichibou_verify::{freeze, Verifier};

/// Keeps test state out of the real per-user data directory.
fn isolate_state() {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().unwrap());
    std::env::set_var("ICHIBOU_STATE_DIR", dir.path());
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
                guard: true,
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
        approved: &HashMap::new(),
        targets: Some("t1"),
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
        targets: f.project.load().unwrap().targets,
        targets_hash: "t1".into(),
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
        "curl -sf \"$ICHIBOU_BASE_URL\" | grep -q built && sh check.sh {report}".into(),
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

#[cfg(unix)]
#[tokio::test]
async fn a_link_planted_where_an_approved_test_goes_is_not_followed() {
    let f = fixture();
    let (approval, frozen) = freeze(&f.project, &f.scenario).unwrap();
    f.store.approve_scenario(&approval, &frozen).unwrap();

    // After approval the agent replaces the test with a link to a file
    // outside the project, hoping the restore writes through it.
    let outside = f.project.root.parent().unwrap().join(format!(
        "outside-{}.txt",
        f.project.root.file_name().unwrap().to_string_lossy()
    ));
    fs::write(&outside, "untouched").unwrap();
    fs::remove_file(f.project.root.join("check.sh")).unwrap();
    std::os::unix::fs::symlink(&outside, f.project.root.join("check.sh")).unwrap();

    let report = verifier(&f).run(None).await.unwrap();
    assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched");
    // The approved check still ran, from the approved copy.
    assert_eq!(report.results[0].outcome, Outcome::Passed, "{}", report.results[0].detail);
    let _ = fs::remove_file(&outside);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn commands_under_verification_cannot_touch_state_or_approved_tests() {
    let f = fixture();
    let state = f.project.local_dir().join("state.db");
    // Setup tries to forge a verdict store and to weaken the approved test.
    let mut targets = f.project.load().unwrap().targets;
    targets.targets[0].verify = Some(VerifyEnv {
        setup: vec![format!(
            "echo forged > '{}' 2>/dev/null; echo 'exit 0' > check.sh 2>/dev/null; true",
            state.with_extension("forged").display()
        )],
        ..Default::default()
    });
    f.project.save_targets(&targets).unwrap();
    fs::write(f.project.root.join("answer.txt"), "0\n").unwrap();
    let (approval, frozen) = freeze(&f.project, &f.scenario).unwrap();
    f.store.approve_scenario(&approval, &frozen).unwrap();

    let report = verifier(&f).run(None).await.unwrap();
    assert!(!state.with_extension("forged").exists());
    // The real check ran against the wrong answer and failed.
    assert_eq!(report.results[0].outcome, Outcome::Failed, "{}", report.results[0].detail);
}

#[tokio::test]
async fn copied_inputs_are_part_of_the_snapshot() {
    let f = fixture();
    fs::write(f.project.root.join(".gitignore"), ".env.test\n").unwrap();
    fs::write(f.project.root.join(".env.test"), "MODE=a\n").unwrap();
    let mut targets = f.project.load().unwrap().targets;
    targets.targets[0].verify = Some(VerifyEnv {
        copy: vec![".env.test".into()],
        ..Default::default()
    });
    f.project.save_targets(&targets).unwrap();
    let (approval, frozen) = freeze(&f.project, &f.scenario).unwrap();
    f.store.approve_scenario(&approval, &frozen).unwrap();

    let first = verifier(&f).run(None).await.unwrap().snapshot;
    // An ignored file that verification copies in is an input: changing it
    // gives a different snapshot, so earlier passes no longer count.
    fs::write(f.project.root.join(".env.test"), "MODE=b\n").unwrap();
    let second = verifier(&f).run(None).await.unwrap().snapshot;
    assert_ne!(first, second);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_self_sandboxing_tool_gets_a_hint_and_can_opt_out() {
    let f = fixture();
    // Setup runs a tool that applies its own sandbox, as SwiftPM does.
    let mut targets = f.project.load().unwrap().targets;
    targets.targets[0].verify = Some(VerifyEnv {
        setup: vec!["/usr/bin/sandbox-exec -p '(version 1)(deny default)(allow process*)(allow file-read*)' /usr/bin/true".into()],
        ..Default::default()
    });
    f.project.save_targets(&targets).unwrap();
    let (approval, frozen) = freeze(&f.project, &f.scenario).unwrap();
    f.store.approve_scenario(&approval, &frozen).unwrap();

    let guarded = verifier(&f).run(None).await.unwrap();
    if guarded.results[0].outcome == Outcome::Error {
        // Inside the guard the tool cannot start, and the error says what to do.
        assert!(guarded.results[0].detail.contains("guard: false"), "{}", guarded.results[0].detail);
    }

    targets.targets[0].guard = false;
    f.project.save_targets(&targets).unwrap();
    let open = verifier(&f).run(None).await.unwrap();
    assert_eq!(open.results[0].outcome, Outcome::Passed, "{}", open.results[0].detail);
}

#[cfg(unix)]
#[tokio::test]
async fn an_approved_executable_test_is_still_executable_when_restored() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let script = f.project.root.join("check.sh");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let mut scenario = f.scenario.clone();
    // Run it directly rather than through `sh`: needs the executable bit.
    scenario.command = Some("./check.sh {report}".into());
    f.project
        .save_scenarios(&ScenariosDoc { scenarios: vec![scenario.clone()] })
        .unwrap();
    let (approval, frozen) = freeze(&f.project, &scenario).unwrap();
    assert_eq!(frozen.executable, vec!["check.sh".to_string()]);
    f.store.approve_scenario(&approval, &frozen).unwrap();
    let report = verifier(&f).run(None).await.unwrap();
    assert_eq!(report.results[0].outcome, Outcome::Passed, "{}", report.results[0].detail);
    assert_eq!(report.results[0].targets, "t1");

    // The executable bit is part of what was approved: taking it away is a
    // change to the definition, so approving that version starts over
    // instead of inheriting the pass earned by the executable one.
    fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();
    let (changed, _) = freeze(&f.project, &scenario).unwrap();
    assert_ne!(changed.definition, approval.definition);
}

#[tokio::test]
async fn files_ignored_only_by_git_exclude_do_not_break_the_copy() {
    let f = fixture();
    // Ignored through .git/info/exclude, which the copy will not have.
    fs::create_dir_all(f.project.root.join(".git/info")).unwrap();
    fs::write(f.project.root.join(".git/info/exclude"), "local.env\n").unwrap();
    fs::write(f.project.root.join("local.env"), "MODE=test\n").unwrap();
    let mut targets = f.project.load().unwrap().targets;
    targets.targets[0].verify = Some(VerifyEnv {
        copy: vec!["local.env".into()],
        ..Default::default()
    });
    f.project.save_targets(&targets).unwrap();
    let (approval, frozen) = freeze(&f.project, &f.scenario).unwrap();
    f.store.approve_scenario(&approval, &frozen).unwrap();
    let report = verifier(&f).run(None).await.unwrap();
    assert_eq!(report.results[0].outcome, Outcome::Passed, "{}", report.results[0].detail);
    // The verdict is for the snapshot the status rules compute from the
    // workspace.
    assert_eq!(report.snapshot, snapshot::take_with(&f.project.root, &["local.env".into()]).unwrap());
}

#[tokio::test]
async fn a_test_file_changed_during_the_run_voids_the_result() {
    let f = fixture();
    // No guard on this target, so the command is able to rewrite the test;
    // the result must still not be trusted.
    let mut targets = f.project.load().unwrap().targets;
    targets.targets[0].guard = false;
    f.project.save_targets(&targets).unwrap();
    let mut scenario = f.scenario.clone();
    scenario.command = Some("sh check.sh {report} && echo '# tampered' >> check.sh".into());
    f.project
        .save_scenarios(&ScenariosDoc { scenarios: vec![scenario.clone()] })
        .unwrap();
    let (approval, frozen) = freeze(&f.project, &scenario).unwrap();
    f.store.approve_scenario(&approval, &frozen).unwrap();
    let report = verifier(&f).run(None).await.unwrap();
    assert_eq!(report.results[0].outcome, Outcome::Error);
    assert!(report.results[0].detail.contains("modified during the run"), "{}", report.results[0].detail);
}

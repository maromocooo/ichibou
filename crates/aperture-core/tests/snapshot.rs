use std::fs;

use aperture_core::manifest::Project;
use aperture_core::snapshot;
use aperture_core::*;

#[test]
fn snapshot_tracks_content_and_skips_ignored_and_local_state() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join(".gitignore"), "node_modules/\n.env\n").unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/app.ts"), "export const a = 1;\n").unwrap();
    let first = snapshot::take(root).unwrap();

    // Ignored files, local state and git internals do not change the snapshot.
    fs::create_dir_all(root.join("node_modules/x")).unwrap();
    fs::write(root.join("node_modules/x/index.js"), "x").unwrap();
    fs::write(root.join(".env"), "SECRET=1").unwrap();
    fs::create_dir_all(root.join(".aperture/local")).unwrap();
    fs::write(root.join(".aperture/local/state.db"), "db").unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git/HEAD"), "ref").unwrap();
    assert_eq!(snapshot::take(root).unwrap(), first);

    // An untracked new file does, and so does an edit.
    fs::write(root.join("src/new.ts"), "export {};\n").unwrap();
    let second = snapshot::take(root).unwrap();
    assert_ne!(second, first);
    fs::write(root.join("src/app.ts"), "export const a = 2;\n").unwrap();
    assert_ne!(snapshot::take(root).unwrap(), second);
}

#[test]
fn materialized_copy_has_the_same_id_and_only_requested_extras() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join(".gitignore"), ".env*\n").unwrap();
    fs::write(root.join("src/app.ts"), "a").unwrap();
    fs::write(root.join(".env"), "SECRET=1").unwrap();
    fs::write(root.join(".env.test"), "TEST=1").unwrap();
    fs::create_dir_all(root.join(".aperture/local")).unwrap();
    fs::write(root.join(".aperture/local/state.db"), "db").unwrap();

    let id = snapshot::take(&root).unwrap();
    let dest = dir.path().join("copy");
    let copied = snapshot::materialize(&root, &dest, &[".env.test".into()]).unwrap();
    assert_eq!(copied, id);
    assert!(dest.join("src/app.ts").exists());
    assert!(dest.join(".env.test").exists());
    assert!(!dest.join(".env").exists());
    assert!(!dest.join(".aperture/local").exists());
}

#[test]
fn manifest_round_trips_and_scenario_definition_follows_test_files() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project::new(dir.path());
    project.init().unwrap();
    assert_eq!(project.load().unwrap(), Default::default());

    let requirements = RequirementsDoc {
        requirements: vec![Requirement {
            id: "REQ-LOGIN-01".into(),
            title: "Sign in".into(),
            description: String::new(),
            depends_on: vec![],
            criteria: vec![Criterion {
                id: "ok".into(),
                text: "A valid user can sign in".into(),
            }],
            manual_checks: vec![],
        }],
    };
    project.save_requirements(&requirements).unwrap();

    let scenario = Scenario {
        id: "login".into(),
        title: "Login".into(),
        target: "web".into(),
        runner: RunnerKind::Playwright,
        covers: vec![Coverage {
            requirement: "REQ-LOGIN-01".into(),
            criterion: "ok".into(),
        }],
        files: vec!["e2e/login.spec.ts".into()],
        command: None,
    };
    project
        .save_scenarios(&ScenariosDoc {
            scenarios: vec![scenario.clone()],
        })
        .unwrap();

    let loaded = project.load().unwrap();
    assert_eq!(loaded.requirements, requirements);
    assert_eq!(loaded.scenarios.scenarios[0], scenario);

    let missing = project.scenario_definition(&scenario).unwrap();
    fs::create_dir_all(dir.path().join("e2e")).unwrap();
    fs::write(dir.path().join("e2e/login.spec.ts"), "expect(1).toBe(1)").unwrap();
    let written = project.scenario_definition(&scenario).unwrap();
    fs::write(dir.path().join("e2e/login.spec.ts"), "expect(1).toBe(2)").unwrap();
    let edited = project.scenario_definition(&scenario).unwrap();
    assert_ne!(missing, written);
    assert_ne!(written, edited);

    // Duplicate ids and unknown dependencies are rejected.
    let mut bad = requirements.clone();
    bad.requirements.push(bad.requirements[0].clone());
    assert!(project.save_requirements(&bad).is_err());
    let mut bad = requirements;
    bad.requirements[0].depends_on = vec!["NOPE".into()];
    assert!(project.save_requirements(&bad).is_err());
}

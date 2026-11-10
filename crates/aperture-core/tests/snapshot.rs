use std::fs;

use aperture_core::manifest::Project;
use aperture_core::snapshot;
use aperture_core::*;

/// Keeps test state out of the real per-user data directory.
fn isolate_state() {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().unwrap());
    std::env::set_var("APERTURE_STATE_DIR", dir.path());
}

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
    isolate_state();
    let project = Project::new(dir.path());
    project.init().unwrap();
    assert_eq!(project.load().unwrap(), Default::default());
    // Aperture's own state is not inside the workspace an agent can write to.
    assert!(!project.local_dir().starts_with(dir.path().canonicalize().unwrap()));
    assert!(project.local_dir().is_dir());
    assert!(!dir.path().join(".aperture/local").exists());

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

#[test]
fn manifest_paths_cannot_leave_the_project() {
    use aperture_core::manifest::{is_safe_relative, read_scenario_files, resolve_inside};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    fs::create_dir_all(root.join("e2e")).unwrap();
    fs::write(root.join("e2e/ok.spec.ts"), "ok").unwrap();
    fs::write(dir.path().join("secret.txt"), "secret").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.path().join("secret.txt"), root.join("e2e/link.spec.ts")).unwrap();

    assert!(is_safe_relative("e2e/ok.spec.ts"));
    for bad in ["", "../secret.txt", "e2e/../../secret.txt", "/etc/passwd"] {
        assert!(!is_safe_relative(bad), "{bad}");
        assert!(resolve_inside(&root, bad).is_none(), "{bad}");
    }
    // A symlink inside the project that points outside is refused too.
    assert!(resolve_inside(&root, "e2e/link.spec.ts").is_none());

    let scenario = Scenario {
        id: "s".into(),
        title: "s".into(),
        target: "web".into(),
        runner: RunnerKind::Command,
        covers: vec![],
        files: vec![
            "e2e/ok.spec.ts".into(),
            "../secret.txt".into(),
            "e2e/link.spec.ts".into(),
        ],
        command: None,
    };
    let files = read_scenario_files(&root, &scenario);
    assert_eq!(files["e2e/ok.spec.ts"].as_deref(), Some(&b"ok"[..]));
    assert!(files["../secret.txt"].is_none());
    assert!(files["e2e/link.spec.ts"].is_none());

    // The manifest refuses such paths when written through Aperture, and
    // ignored-file copies from targets.yaml never reach outside.
    isolate_state();
    let project = Project::new(&root);
    project.init().unwrap();
    assert!(project
        .save_scenarios(&ScenariosDoc { scenarios: vec![scenario] })
        .is_err());
    let dest = dir.path().join("copy");
    snapshot::materialize(&root, &dest, &["../secret.txt".into()]).unwrap();
    assert!(fs::read_dir(&dest).unwrap().all(|e| e.unwrap().file_name() != "secret.txt"));
    assert_eq!(fs::read_to_string(dir.path().join("secret.txt")).unwrap(), "secret");
}

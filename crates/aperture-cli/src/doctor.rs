//! `aperture doctor`: reports which of the tools Aperture drives are present.

use std::process::Command;

struct Check {
    name: &'static str,
    /// What stops working without it.
    needed_for: &'static str,
    required: bool,
    program: &'static str,
    args: &'static [&'static str],
}

const CHECKS: &[Check] = &[
    Check {
        name: "Claude Code",
        needed_for: "running Claude as the agent",
        required: false,
        program: "claude",
        args: &["--version"],
    },
    Check {
        name: "Codex CLI",
        needed_for: "running Codex as the agent",
        required: false,
        program: "codex",
        args: &["--version"],
    },
    Check {
        name: "Node.js",
        needed_for: "Playwright scenarios (web, API, Electron)",
        required: false,
        program: "node",
        args: &["--version"],
    },
    Check {
        name: "Xcode simulator tools",
        needed_for: "iOS Simulator preview",
        required: false,
        program: "xcrun",
        args: &["simctl", "help"],
    },
    Check {
        name: "idb",
        needed_for: "tapping and typing in the iOS Simulator preview",
        required: false,
        program: "idb",
        args: &["--help"],
    },
    Check {
        name: "serve-sim",
        needed_for: "live video of the iOS Simulator (screenshots are used without it)",
        required: false,
        program: "serve-sim",
        args: &["--version"],
    },
    Check {
        name: "adb",
        needed_for: "Android Emulator preview",
        required: false,
        program: "adb",
        args: &["version"],
    },
    Check {
        name: "Maestro",
        needed_for: "iOS and Android scenarios",
        required: false,
        program: "maestro",
        args: &["--version"],
    },
];

/// Prints the report. Returns false when nothing can drive an agent.
pub fn run() -> bool {
    let mut agents = 0;
    for check in CHECKS {
        let output = Command::new(check.program).args(check.args).output();
        let found = output.as_ref().is_ok_and(|o| o.status.success());
        let version = output
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string())
            .unwrap_or_default();
        if found && matches!(check.program, "claude" | "codex") {
            agents += 1;
        }
        let mark = if found {
            "ok     "
        } else if check.required {
            "MISSING"
        } else {
            "missing"
        };
        if found {
            println!("{mark} {:<24} {version}", check.name);
        } else {
            println!("{mark} {:<24} needed for {}", check.name, check.needed_for);
        }
    }
    if agents == 0 {
        println!("\nNo agent CLI found. Install Claude Code or Codex and sign in with it first.");
    }
    agents > 0
}

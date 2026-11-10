//! Keeping child processes away from Ichibou's own state.
//!
//! Everything Ichibou starts for a project — the agent, the app being
//! previewed, build and test commands — runs code an agent wrote or can
//! influence. Approvals and verdicts are only worth something if that code
//! cannot rewrite them, so on macOS those processes are started under
//! `sandbox-exec` with writes to the state directory denied. Nothing else is
//! restricted. On other systems there is no such guard yet.

use std::path::{Path, PathBuf};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

#[derive(Debug, Clone, Default)]
pub struct Guard {
    /// Directories the process may not write under.
    pub deny: Vec<PathBuf>,
    /// Directories inside a denied one that stay writable.
    pub allow: Vec<PathBuf>,
    /// Single files that may not be written, even inside an allowed directory.
    pub deny_files: Vec<PathBuf>,
}

/// Resolves links in the part of `path` that exists; the kernel matches
/// sandbox rules against real paths.
fn real(path: &Path) -> PathBuf {
    if let Ok(full) = path.canonicalize() {
        return full;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => real(parent).join(name),
        _ => path.to_path_buf(),
    }
}

fn quoted(path: &Path) -> String {
    let text = real(path).to_string_lossy().into_owned();
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Guard {
    /// Denies writes to Ichibou's state for every project.
    pub fn state() -> Self {
        Self {
            deny: vec![crate::manifest::state_base()],
            ..Default::default()
        }
    }

    /// The sandbox profile: allow everything, then the write rules. Later
    /// rules win, so the order is deny directories, allow exceptions, deny
    /// single files.
    pub fn profile(&self) -> String {
        let mut profile = String::from("(version 1)(allow default)");
        let rule = |verb: &str, kind: &str, paths: &[PathBuf]| -> String {
            if paths.is_empty() {
                return String::new();
            }
            let list: Vec<String> = paths.iter().map(|p| format!("({kind} {})", quoted(p))).collect();
            format!("({verb} file-write* {})", list.join(" "))
        };
        profile.push_str(&rule("deny", "subpath", &self.deny));
        profile.push_str(&rule("allow", "subpath", &self.allow));
        // Rules match paths. Renaming a directory above a protected one
        // would move it out from under its rule, so the directories above
        // each denied directory, and the allowed directories themselves,
        // may not be renamed, removed or replaced either. A `literal` rule
        // covers the entry itself, not what is created inside it.
        let mut entries: Vec<PathBuf> = self.deny_files.clone();
        for dir in &self.deny {
            let mut above = real(dir);
            while above.pop() {
                if above.as_os_str().is_empty() || above == Path::new("/") {
                    break;
                }
                entries.push(above.clone());
            }
        }
        entries.extend(self.allow.iter().cloned());
        entries.sort();
        entries.dedup();
        profile.push_str(&rule("deny", "literal", &entries));
        profile
    }

    /// The command prefix that applies this guard, when this system has one.
    pub fn prefix(&self) -> Option<Vec<String>> {
        if self.deny.is_empty() && self.deny_files.is_empty() {
            return None;
        }
        if !cfg!(target_os = "macos") || !Path::new(SANDBOX_EXEC).exists() {
            return None;
        }
        Some(vec![SANDBOX_EXEC.to_string(), "-p".to_string(), self.profile()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_orders_rules_so_exceptions_and_file_denials_apply() {
        let guard = Guard {
            deny: vec!["/state".into()],
            allow: vec!["/state/p/verify".into()],
            deny_files: vec!["/state/p/verify/e2e/a \"b\".spec.js".into()],
        };
        let profile = guard.profile();
        assert!(profile.starts_with(
            "(version 1)(allow default)\
             (deny file-write* (subpath \"/state\"))\
             (allow file-write* (subpath \"/state/p/verify\"))\
             (deny file-write* "
        ));
        // The approved file (quotes escaped) and the allowed root itself.
        assert!(profile.contains("(literal \"/state/p/verify/e2e/a \\\"b\\\".spec.js\")"));
        assert!(profile.contains("(literal \"/state/p/verify\")"));
        assert!(Guard::default().prefix().is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn guarded_process_cannot_write_denied_paths() {
        let dir = std::env::temp_dir().join(format!("ichibou-sandbox-{}", std::process::id()));
        let state = dir.join("state");
        let work = state.join("verify");
        std::fs::create_dir_all(work.join("e2e")).unwrap();
        std::fs::write(work.join("e2e/approved.spec.js"), "approved").unwrap();
        let guard = Guard {
            deny: vec![state.clone()],
            allow: vec![work.clone()],
            deny_files: vec![work.join("e2e/approved.spec.js"), work.join("e2e")],
        };
        let prefix = guard.prefix().expect("sandbox-exec is part of macOS");
        let run = |script: &str| {
            std::process::Command::new(&prefix[0])
                .args(&prefix[1..])
                .args(["sh", "-c", script])
                .current_dir(&dir)
                .status()
                .unwrap()
                .success()
        };
        // The state directory is closed, its verification copy is open, and
        // an approved test inside the copy is closed again.
        assert!(!run("echo forged > state/state.db"));
        assert!(run("echo built > state/verify/out.txt"));
        assert!(!run("echo weakened > state/verify/e2e/approved.spec.js"));
        assert!(run("echo ok > elsewhere.txt"));
        // New files next to an approved one are fine...
        assert!(run("echo helper > state/verify/e2e/helper.js"));
        // ...but nothing above a protected path can be moved aside to get
        // around its rule: not the test's directory, the copy, or the state
        // directory's own parent.
        assert!(!run("mv state/verify/e2e state/verify/e2e-moved"));
        assert!(!run("mv state/verify state/verify-moved"));
        assert!(!run("mv state state-moved"));
        assert!(!run(&format!("mv '{}' '{}-moved'", dir.display(), dir.display())));
        assert_eq!(
            std::fs::read_to_string(work.join("e2e/approved.spec.js")).unwrap(),
            "approved"
        );
        assert!(!state.join("state.db").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

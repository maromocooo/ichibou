//! Reading and writing the git-tracked manifest in `<project>/.ichibou/`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};

use crate::model::*;

pub const DIR: &str = ".ichibou";

/// Where Ichibou keeps its own state for all projects. It is deliberately
/// outside every workspace: approvals and verdicts live there, and an agent
/// that can write to the workspace must not be able to rewrite them.
/// `ICHIBOU_STATE_DIR` overrides the location (used by tests).
pub fn state_base() -> PathBuf {
    if let Some(dir) = std::env::var_os("ICHIBOU_STATE_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Ichibou/projects")
    } else {
        match std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
            Some(dir) => PathBuf::from(dir).join("ichibou/projects"),
            None => home.join(".local/state/ichibou/projects"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub requirements: RequirementsDoc,
    pub architecture: ArchitectureDoc,
    pub targets: TargetsDoc,
    pub scenarios: ScenariosDoc,
}

impl Project {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn manifest_dir(&self) -> PathBuf {
        self.root.join(DIR)
    }

    /// This project's state directory: `<state base>/<name>-<hash of path>`.
    pub fn local_dir(&self) -> PathBuf {
        let root = self.root.canonicalize().unwrap_or_else(|_| self.root.clone());
        let digest = Sha256::digest(root.to_string_lossy().as_bytes());
        let name: String = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .take(40)
            .collect();
        state_base().join(format!("{name}-{}", &hex(&digest)[..16]))
    }

    fn file(&self, name: &str) -> PathBuf {
        self.manifest_dir().join(name)
    }

    /// Refuses to write when the manifest directory (or its `scenarios`
    /// subdirectory) is a symbolic link: the write would land wherever the
    /// link points, outside the project.
    fn check_writable(&self) -> Result<()> {
        for dir in [self.manifest_dir(), self.manifest_dir().join("scenarios")] {
            if let Ok(meta) = fs::symlink_metadata(&dir) {
                if !meta.is_dir() {
                    bail!("{} is not a real directory; refusing to write through it", dir.display());
                }
            }
        }
        Ok(())
    }

    /// Creates the manifest directory and this project's state directory.
    pub fn init(&self) -> Result<()> {
        fs::create_dir_all(self.manifest_dir())?;
        fs::create_dir_all(self.local_dir())?;
        Ok(())
    }

    pub fn load(&self) -> Result<Manifest> {
        Ok(Manifest {
            requirements: read_yaml(&self.file("requirements.yaml"))?,
            architecture: read_yaml(&self.file("architecture.yaml"))?,
            targets: read_yaml(&self.file("targets.yaml"))?,
            scenarios: read_yaml(&self.file("scenarios/index.yaml"))?,
        })
    }

    /// Returns the text that was written, so callers can bind an approval
    /// to exactly these bytes.
    pub fn save_requirements(&self, doc: &RequirementsDoc) -> Result<String> {
        self.check_writable()?;
        validate_requirements(doc)?;
        write_yaml(&self.file("requirements.yaml"), doc)
    }

    pub fn save_architecture(&self, doc: &ArchitectureDoc) -> Result<()> {
        self.check_writable()?;
        validate_architecture(doc)?;
        write_yaml(&self.file("architecture.yaml"), doc).map(|_| ())
    }

    /// Returns the text that was written, so callers can bind an approval
    /// to exactly these bytes.
    pub fn save_targets(&self, doc: &TargetsDoc) -> Result<String> {
        self.check_writable()?;
        for target in &doc.targets {
            let launches = [
                target.dev.as_ref(),
                target.verify.as_ref().and_then(|v| v.start.as_ref()),
            ];
            let cwds = launches.into_iter().flatten().filter_map(|l| l.cwd.as_deref());
            let lists = target
                .verify
                .iter()
                .flat_map(|v| v.copy.iter().chain(v.link.iter()))
                .map(String::as_str);
            for path in cwds.chain(lists) {
                if !is_safe_relative(path) {
                    bail!("target {}: `{path}` must be a relative path inside the project", target.name);
                }
            }
        }
        write_yaml(&self.file("targets.yaml"), doc)
    }

    pub fn save_scenarios(&self, doc: &ScenariosDoc) -> Result<()> {
        self.check_writable()?;
        for scenario in &doc.scenarios {
            for file in &scenario.files {
                if !is_safe_relative(file) {
                    bail!("scenario {}: `{file}` must be a relative path inside the project", scenario.id);
                }
            }
        }
        write_yaml(&self.file("scenarios/index.yaml"), doc).map(|_| ())
    }

    /// Hash of everything that defines what a scenario checks: its manifest
    /// entry plus the content of its test files. A missing file hashes as
    /// such, so deleting a test changes the definition.
    pub fn scenario_definition(&self, scenario: &Scenario) -> Result<String> {
        self.scenario_definition_in(&self.root, scenario)
    }

    /// Same as [`scenario_definition`], reading test files from another root
    /// (a materialized snapshot).
    pub fn scenario_definition_in(&self, root: &Path, scenario: &Scenario) -> Result<String> {
        let files = read_scenario_files(root, scenario);
        definition_of(scenario, &files)
    }
}

/// Largest scenario test file Ichibou will read, freeze and show.
pub const MAX_SCENARIO_FILE: u64 = 4 * 1024 * 1024;

/// Whether a path taken from a manifest file is a plain relative path: not
/// absolute and with no `..`. Manifest files live in the workspace, where an
/// agent can write them, so their paths are never trusted to stay inside it.
pub fn is_safe_relative(path: &str) -> bool {
    use std::path::Component;
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Resolves a manifest path to an existing location inside `root`. Returns
/// `None` when the path is not a plain relative one, does not exist, or
/// (through a symlink) ends up outside `root`.
pub fn resolve_inside(root: &Path, relative: &str) -> Option<PathBuf> {
    if !is_safe_relative(relative) {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let full = root.join(relative).canonicalize().ok()?;
    full.starts_with(&root).then_some(full)
}

/// Reads a scenario's test files once. `None` marks a file that is missing,
/// is not a regular file inside the project, or is larger than
/// [`MAX_SCENARIO_FILE`]; such a file can never be part of a passing run.
pub fn read_scenario_files(
    root: &Path,
    scenario: &Scenario,
) -> std::collections::HashMap<String, Option<Vec<u8>>> {
    use std::io::Read;
    let read = |file: &str| -> Option<Vec<u8>> {
        let path = resolve_inside(root, file)?;
        let meta = fs::metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > MAX_SCENARIO_FILE {
            return None;
        }
        // The size is checked again while reading: the file may grow.
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .ok()?
            .take(MAX_SCENARIO_FILE + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 <= MAX_SCENARIO_FILE).then_some(bytes)
    };
    scenario
        .files
        .iter()
        .map(|file| (file.clone(), read(file)))
        .collect()
}

/// Hash of everything that defines what a scenario checks, computed from
/// file contents already in memory. Approval freezes these same bytes, so
/// the hash and the frozen copy cannot disagree.
pub fn definition_of(
    scenario: &Scenario,
    files: &std::collections::HashMap<String, Option<Vec<u8>>>,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(scenario)?);
    let mut names = scenario.files.clone();
    names.sort();
    for name in names {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        match files.get(&name).and_then(|content| content.as_ref()) {
            Some(bytes) => {
                hasher.update([1]);
                hasher.update(bytes);
            }
            None => hasher.update([0]),
        }
    }
    Ok(hex(&hasher.finalize()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parses a manifest document from bytes already read (empty means default).
pub fn parse<T: DeserializeOwned + Default>(bytes: &[u8]) -> Result<T> {
    let text = std::str::from_utf8(bytes).context("manifest is not UTF-8")?;
    if text.trim().is_empty() {
        return Ok(T::default());
    }
    serde_yaml::from_str(text).context("invalid manifest")
}

fn read_yaml<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(T::default()),
        Ok(text) => {
            serde_yaml::from_str(&text).with_context(|| format!("invalid {}", path.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Writes a manifest file without following symbolic links. The manifest
/// directory is in the workspace, so an agent could have replaced the
/// directory, the file's temporary sibling, or anything on the way with a
/// link to somewhere else.
fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<String> {
    let parent = path.parent().context("manifest file has no directory")?;
    fs::create_dir_all(parent)?;
    let text = serde_yaml::to_string(value)?;
    // A new, uniquely named sibling: `create_new` refuses an existing entry
    // (including a link), and the rename replaces the destination itself,
    // never what a link there points at.
    let unique = format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = parent.join(unique);
    let written = (|| -> Result<()> {
        use std::io::Write;
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written?;
    Ok(text)
}

pub fn validate_requirements(doc: &RequirementsDoc) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for req in &doc.requirements {
        if req.id.trim().is_empty() {
            bail!("a requirement has an empty id");
        }
        if !seen.insert(req.id.as_str()) {
            bail!("duplicate requirement id {}", req.id);
        }
        let mut ids = std::collections::HashSet::new();
        for id in req
            .criteria
            .iter()
            .map(|c| &c.id)
            .chain(req.manual_checks.iter().map(|m| &m.id))
        {
            if !ids.insert(id.as_str()) {
                bail!("duplicate criterion or check id {id} in {}", req.id);
            }
        }
    }
    for req in &doc.requirements {
        for dep in &req.depends_on {
            if !seen.contains(dep.as_str()) {
                bail!("{} depends on unknown requirement {dep}", req.id);
            }
        }
    }
    if crate::status::dependency_order(doc).is_none() {
        bail!("requirement dependencies form a cycle");
    }
    Ok(())
}

pub fn validate_architecture(doc: &ArchitectureDoc) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for node in &doc.nodes {
        if !seen.insert(node.id.as_str()) {
            bail!("duplicate architecture node id {}", node.id);
        }
    }
    for edge in &doc.edges {
        for end in [&edge.from, &edge.to] {
            if !seen.contains(end.as_str()) {
                bail!("edge references unknown node {end}");
            }
        }
    }
    Ok(())
}

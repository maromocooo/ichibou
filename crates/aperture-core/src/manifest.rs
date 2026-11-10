//! Reading and writing the git-tracked manifest in `<project>/.aperture/`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};

use crate::model::*;

pub const DIR: &str = ".aperture";

/// Where Aperture keeps its own state for all projects. It is deliberately
/// outside every workspace: approvals and verdicts live there, and an agent
/// that can write to the workspace must not be able to rewrite them.
/// `APERTURE_STATE_DIR` overrides the location (used by tests).
pub fn state_base() -> PathBuf {
    if let Some(dir) = std::env::var_os("APERTURE_STATE_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Aperture/projects")
    } else {
        match std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
            Some(dir) => PathBuf::from(dir).join("aperture/projects"),
            None => home.join(".local/state/aperture/projects"),
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
        validate_requirements(doc)?;
        write_yaml(&self.file("requirements.yaml"), doc)
    }

    pub fn save_architecture(&self, doc: &ArchitectureDoc) -> Result<()> {
        validate_architecture(doc)?;
        write_yaml(&self.file("architecture.yaml"), doc).map(|_| ())
    }

    /// Returns the text that was written, so callers can bind an approval
    /// to exactly these bytes.
    pub fn save_targets(&self, doc: &TargetsDoc) -> Result<String> {
        write_yaml(&self.file("targets.yaml"), doc)
    }

    pub fn save_scenarios(&self, doc: &ScenariosDoc) -> Result<()> {
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
        let mut hasher = Sha256::new();
        hasher.update(serde_json::to_vec(scenario)?);
        let mut files = scenario.files.clone();
        files.sort();
        for file in files {
            hasher.update(file.as_bytes());
            hasher.update([0]);
            match fs::read(root.join(&file)) {
                Ok(bytes) => {
                    hasher.update([1]);
                    hasher.update(&bytes);
                }
                Err(_) => hasher.update([0]),
            }
        }
        Ok(hex(&hasher.finalize()))
    }
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

fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_yaml::to_string(value)?;
    // Write to a sibling and rename so a crash never leaves a half-written manifest.
    let tmp = path.with_extension("yaml.tmp");
    fs::write(&tmp, &text)?;
    fs::rename(&tmp, path)?;
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

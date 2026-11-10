//! Content snapshots of the working tree.
//!
//! A snapshot covers every file git would track or show as untracked: ignored
//! files are left out, as are `.git` and Ichibou's local state. It does not
//! touch the user's index. Verification runs against a materialized copy of a
//! snapshot, so the verdict is tied to exactly the bytes that were hashed.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

use crate::manifest::hex;
use crate::model::SnapshotId;

/// Paths never included, relative to the project root.
const EXCLUDED: &[&str] = &[".git", ".ichibou/local"];

fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .require_git(false)
        .parents(false)
        .filter_entry(|entry| {
            let path = entry.path();
            !EXCLUDED.iter().any(|ex| path.ends_with(ex))
        })
        .build();
    for entry in walker {
        let entry = entry?;
        let Some(kind) = entry.file_type() else { continue };
        if kind.is_file() || kind.is_symlink() {
            out.push(entry.path().strip_prefix(root)?.to_path_buf());
        }
    }
    out.sort();
    Ok(out)
}

fn is_executable(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// The files a snapshot of `root` covers, relative to it, in a fixed order.
pub fn list(root: &Path) -> Result<Vec<PathBuf>> {
    files(root)
}

/// Hashes the listed files below `root`: path, kind (link, executable or
/// plain) and content.
fn hash_tree(root: &Path, list: &[PathBuf]) -> Result<SnapshotId> {
    let mut hasher = Sha256::new();
    for rel in list {
        let path = root.join(rel);
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update([0]);
        // A file can vanish between the walk and the read while an agent is
        // editing; treat it as absent rather than failing the snapshot.
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                hasher.update(b"-");
                hasher.update([0]);
                continue;
            }
            Err(err) => return Err(err).context(format!("cannot stat {}", path.display())),
        };
        if meta.file_type().is_symlink() {
            hasher.update(b"l");
            hasher.update(fs::read_link(&path)?.to_string_lossy().as_bytes());
        } else {
            hasher.update(if is_executable(&meta) { b"x" } else { b"f" });
            match fs::read(&path) {
                Ok(bytes) => {
                    hasher.update((bytes.len() as u64).to_le_bytes());
                    hasher.update(&bytes);
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => hasher.update(b"-"),
                Err(err) => return Err(err).context(format!("cannot read {}", path.display())),
            }
        }
        hasher.update([0]);
    }
    Ok(SnapshotId(hex(&hasher.finalize())))
}

/// Hashes the working tree.
pub fn take(root: &Path) -> Result<SnapshotId> {
    hash_tree(root, &files(root)?)
}

/// Hashes the working tree together with the ignored files that
/// verification copies in (`verify.copy` in targets.yaml, for example
/// `.env.test`). They are inputs to a run, so changing one must invalidate
/// earlier verdicts just like editing a tracked file does.
pub fn take_with(root: &Path, extra: &[String]) -> Result<SnapshotId> {
    hash_listed(root, &files(root)?, extra)
}

/// Like [`take_with`], for a given file list instead of walking `root`.
/// A copy is hashed with the list taken from the original: which files are
/// ignored depends on things the copy does not have (`.git/info/exclude`),
/// so walking the copy could see a different set for the same content.
pub fn hash_listed(root: &Path, list: &[PathBuf], extra: &[String]) -> Result<SnapshotId> {
    let tree = hash_tree(root, list)?;
    let mut names: Vec<&String> = extra.iter().collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        return Ok(tree);
    }
    let mut hasher = Sha256::new();
    hasher.update(tree.0.as_bytes());
    for name in names {
        hasher.update([0]);
        hasher.update(name.as_bytes());
        hasher.update([0]);
        // Same rule as when copying: only files inside the project count.
        match crate::manifest::resolve_inside(root, name).and_then(|p| fs::read(p).ok()) {
            Some(bytes) => {
                hasher.update([1]);
                hasher.update(&bytes);
            }
            None => hasher.update([0]),
        }
    }
    Ok(SnapshotId(hex(&hasher.finalize())))
}

/// Copies the snapshot's files into `dest` and returns the id of what was
/// actually copied. Callers compare it with the id they expected: if the tree
/// changed while copying, the two differ and the copy must not be used as
/// evidence for the expected snapshot.
///
/// `extra` lists ignored files (for example `.env.test`) to copy as well;
/// they are not part of the hash.
pub fn materialize(root: &Path, dest: &Path, extra: &[String]) -> Result<SnapshotId> {
    let list = files(root)?;
    materialize_listed(root, dest, &list, extra)?;
    hash_tree(dest, &list)
}

/// Copies the listed files, and the extra ignored ones, from `root` into
/// `dest`.
pub fn materialize_listed(root: &Path, dest: &Path, list: &[PathBuf], extra: &[String]) -> Result<()> {
    fs::create_dir_all(dest)?;
    for rel in list {
        let from = root.join(rel);
        let to = dest.join(rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        let meta = match fs::symlink_metadata(&from) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err.into()),
        };
        if meta.file_type().is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(&from)?, &to)?;
        } else {
            match fs::copy(&from, &to) {
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err).context(format!("cannot copy {}", from.display())),
            }
        }
    }
    for rel in extra {
        // The list comes from targets.yaml. A path that leaves the project
        // is neither read nor written.
        let Some(from) = crate::manifest::resolve_inside(root, rel) else {
            continue;
        };
        if from.is_file() {
            let to = dest.join(rel);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

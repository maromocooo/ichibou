//! Writing below a directory without ever following a symbolic link.
//!
//! Ichibou writes into two places an agent's code can also write to: the
//! workspace (manifest files) and the verification copy (approved test
//! files, while the app under test may be running). Checking a path and then
//! opening it leaves a gap in which a directory on the way can be swapped
//! for a link. Here each directory is opened relative to its parent's
//! descriptor with `O_NOFOLLOW`, so the walk cannot be redirected, and the
//! file is created under a new name and renamed into place.

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use anyhow::{bail, Context, Result};

fn name(part: &OsStr) -> Result<CString> {
    CString::new(part.as_bytes()).context("path contains a NUL byte")
}

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

/// Opens `name` inside `parent` as a directory, refusing links. Creates it
/// when missing. When something that is not a directory is in the way it is
/// removed if `replace` is set, and an error otherwise.
fn subdir(parent: &OwnedFd, part: &OsStr, replace: bool) -> Result<OwnedFd> {
    let c = name(part)?;
    for _ in 0..4 {
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd >= 0 {
            return Ok(unsafe { OwnedFd::from_raw_fd(fd) });
        }
        let err = last_error();
        match err.raw_os_error() {
            Some(libc::ENOENT) => {
                // Lost a race to create it is fine: the next open decides.
                unsafe { libc::mkdirat(parent.as_raw_fd(), c.as_ptr(), 0o755) };
            }
            // A link (ELOOP) or a file (ENOTDIR) where a directory belongs.
            Some(libc::ELOOP) | Some(libc::ENOTDIR) if replace => {
                if unsafe { libc::unlinkat(parent.as_raw_fd(), c.as_ptr(), 0) } != 0 {
                    return Err(last_error()).context("cannot replace a non-directory");
                }
            }
            _ => {
                return Err(err)
                    .with_context(|| format!("`{}` is not a real directory", part.to_string_lossy()))
            }
        }
    }
    bail!("`{}` keeps changing", part.to_string_lossy())
}

/// Writes `content` to `relative` below `root`, or removes that file when
/// `content` is `None`. No component of `relative` is followed if it is a
/// link. With `replace`, links and files standing where directories belong
/// are replaced by directories; without it they are an error.
pub fn write_under(
    root: &Path,
    relative: &str,
    content: Option<&[u8]>,
    executable: bool,
    replace: bool,
) -> Result<()> {
    if !crate::manifest::is_safe_relative(relative) {
        bail!("`{relative}` is not a plain relative path");
    }
    let parts: Vec<&OsStr> = Path::new(relative)
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    let Some((file, dirs)) = parts.split_last() else {
        bail!("empty path");
    };

    let root_c = CString::new(root.as_os_str().as_bytes())?;
    let fd = unsafe { libc::open(root_c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(last_error()).with_context(|| format!("cannot open {}", root.display()));
    }
    let mut dir = unsafe { OwnedFd::from_raw_fd(fd) };
    for part in dirs {
        dir = subdir(&dir, part, replace)?;
    }

    let target = name(file)?;
    let Some(bytes) = content else {
        if unsafe { libc::unlinkat(dir.as_raw_fd(), target.as_ptr(), 0) } != 0 {
            let err = last_error();
            if err.raw_os_error() != Some(libc::ENOENT) {
                // A directory in the file's place: remove it if it is empty.
                unsafe { libc::unlinkat(dir.as_raw_fd(), target.as_ptr(), libc::AT_REMOVEDIR) };
            }
        }
        return Ok(());
    };

    // A fresh name: O_EXCL refuses anything already there, link or not.
    let unique = format!(
        ".{}.{}.{}.tmp",
        file.to_string_lossy(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = CString::new(unique)?;
    let mode: libc::c_uint = if executable { 0o755 } else { 0o644 };
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            tmp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return Err(last_error()).context("cannot create a temporary file");
    }
    let written = (|| -> Result<()> {
        let mut out = unsafe { File::from_raw_fd(fd) };
        out.write_all(bytes)?;
        out.sync_all()?;
        drop(out);
        // rename replaces the entry itself, never what a link there points at.
        if unsafe { libc::renameat(dir.as_raw_fd(), tmp.as_ptr(), dir.as_raw_fd(), target.as_ptr()) } != 0 {
            let err = last_error();
            if !replace {
                return Err(err).context("cannot move the file into place");
            }
            // A directory stands where the file goes: clear it if empty.
            unsafe { libc::unlinkat(dir.as_raw_fd(), target.as_ptr(), libc::AT_REMOVEDIR) };
            if unsafe { libc::renameat(dir.as_raw_fd(), tmp.as_ptr(), dir.as_raw_fd(), target.as_ptr()) } != 0 {
                return Err(last_error()).context("cannot move the file into place");
            }
        }
        Ok(())
    })();
    if written.is_err() {
        unsafe { libc::unlinkat(dir.as_raw_fd(), tmp.as_ptr(), 0) };
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ichibou-safefs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("root")).unwrap();
        dir
    }

    #[test]
    fn writes_creates_directories_and_keeps_the_executable_bit() {
        let dir = scratch("plain");
        let root = dir.join("root");
        write_under(&root, "a/b/run.sh", Some(b"echo hi"), true, false).unwrap();
        assert_eq!(fs::read(root.join("a/b/run.sh")).unwrap(), b"echo hi");
        assert_ne!(fs::metadata(root.join("a/b/run.sh")).unwrap().permissions().mode() & 0o111, 0);
        write_under(&root, "a/b/run.sh", Some(b"v2"), false, false).unwrap();
        assert_eq!(fs::read(root.join("a/b/run.sh")).unwrap(), b"v2");
        assert_eq!(fs::metadata(root.join("a/b/run.sh")).unwrap().permissions().mode() & 0o111, 0);
        write_under(&root, "a/b/run.sh", None, false, false).unwrap();
        assert!(!root.join("a/b/run.sh").exists());
        assert!(write_under(&root, "../escape", Some(b"x"), false, true).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn links_are_never_followed() {
        let dir = scratch("links");
        let root = dir.join("root");
        let outside = dir.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("victim.txt"), "untouched").unwrap();

        // A link where the file goes: replaced, the victim untouched.
        symlink(outside.join("victim.txt"), root.join("file.txt")).unwrap();
        write_under(&root, "file.txt", Some(b"new"), false, false).unwrap();
        assert_eq!(fs::read_to_string(outside.join("victim.txt")).unwrap(), "untouched");
        assert!(fs::symlink_metadata(root.join("file.txt")).unwrap().is_file());

        // A link where a directory goes: an error, or replaced when asked.
        symlink(&outside, root.join("dir")).unwrap();
        assert!(write_under(&root, "dir/inner.txt", Some(b"x"), false, false).is_err());
        assert!(!outside.join("inner.txt").exists());
        write_under(&root, "dir/inner.txt", Some(b"x"), false, true).unwrap();
        assert!(!outside.join("inner.txt").exists());
        assert!(fs::symlink_metadata(root.join("dir")).unwrap().is_dir());
        assert_eq!(fs::read(root.join("dir/inner.txt")).unwrap(), b"x");
        let _ = fs::remove_dir_all(&dir);
    }
}

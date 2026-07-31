//! Clone + clean pipeline. We shell out to `git` for reliable
//! `--recurse-submodules` support (no libgit2/GitPython submodule pain).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// A token looks like a git object id (hex, 7..=40 chars).
pub fn looks_like_commit(s: &str) -> bool {
    let len = s.len();
    len >= 7 && len <= 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn run_git(args: &[&str]) -> Result<()> {
    let st = Command::new("git")
        .args(["-c", "advice.detachedHead=false"])
        .args(args)
        .status()
        .with_context(|| format!("failed to invoke git {:?}", args))?;
    if !st.success() {
        bail!("git {:?} failed (exit {:?})", args, st.code());
    }
    Ok(())
}

/// Shallow-clone `url` at `tag` (branch/tag) or `tag` (commit) into `dst`.
pub fn shallow_clone(url: &str, tag: &str, dst: &Path, force_commit: bool) -> Result<()> {
    if dst.exists() {
        std::fs::remove_dir_all(dst).ok();
    }
    std::fs::create_dir_all(dst.parent().unwrap_or(Path::new(".")))?;
    let dst_str = dst.to_string_lossy();

    if force_commit || looks_like_commit(tag) {
        // full clone (server may refuse shallow-by-sha), then checkout + submodules
        run_git(&["clone", "--no-checkout", url, &dst_str])
            .with_context(|| format!("clone {}", url))?;
        run_git(&["-C", &dst_str, "checkout", tag])?;
        run_git(&["-C", &dst_str, "submodule", "update", "--init", "--recursive"])?;
        return Ok(());
    }

    // branch/tag: try shallow + recurse, fall back to full + recurse
    let attempts: &[&[&str]] = &[
        &["clone", "--depth", "1", "--branch", tag, "--recurse-submodules", url, &dst_str],
        &["clone", "--branch", tag, "--recurse-submodules", url, &dst_str],
        &["clone", "--branch", tag, url, &dst_str],
    ];
    let mut last_err: Option<anyhow::Error> = None;
    for args in attempts {
        match run_git(args) {
            Ok(()) => {
                if *args == &["clone", "--branch", tag, url, &dst_str] {
                    // ensure submodules after the non-recursive fallback
                    run_git(&["-C", &dst_str, "submodule", "update", "--init", "--recursive"])?;
                }
                return Ok(());
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("git clone failed")))
}

// ---- VCS cleanup ------------------------------------------------------------

const VCS_DIRS: &[&str] = &[".git", ".svn", ".hg", ".bzr", "_darcs", ".fossil", ".cargo"];
const VCS_FILES: &[&str] = &[
    ".gitmodules",
    ".gitignore",
    ".gitattributes",
    ".gitreview",
    ".mailmap",
    ".cvsignore",
    ".hgignore",
    ".bzrignore",
];

/// Clear read-only bits across a tree so VCS metadata (e.g. git pack files) can
/// be deleted. Cross-platform: on Unix this sets the owner-write bit (0o200),
/// on Windows it clears FILE_ATTRIBUTE_READONLY — without this, `remove_dir_all`
/// fails to delete `.git` on Windows and archives get VCS-contaminated.
fn make_tree_writable(root: &Path) {
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.permissions().readonly() {
                let mut perms = meta.permissions();
                perms.set_readonly(false);
                let _ = std::fs::set_permissions(entry.path(), perms);
            }
        }
    }
}

/// Remove version-control metadata so it doesn't bloat the archive.
pub fn clean_vcs(root: &Path) -> Result<()> {
    make_tree_writable(root);

    let mut to_remove: Vec<(PathBuf, bool /*is_dir*/)> = Vec::new();
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        let name = entry.file_name().to_string_lossy();
        let is_dir = entry.file_type().is_dir();
        let is_file = entry.file_type().is_file();
        if VCS_DIRS.contains(&&*name) {
            // matches a dir (.git/, .svn/) OR a file (.git gitlink inside a submodule)
            to_remove.push((entry.path().to_path_buf(), is_dir));
        } else if is_file && VCS_FILES.contains(&&*name) {
            to_remove.push((entry.path().to_path_buf(), false));
        }
    }
    for (p, is_dir) in to_remove {
        let res = if is_dir {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        if let Err(e) = res {
            // non-fatal: a stray VCS file shouldn't abort the whole build
            eprintln!("warn: could not remove {}: {}", p.display(), e);
        }
    }
    Ok(())
}

// ---- symlink dereference (opt-in, for portability) -------------------------

/// Replace symlinks with copies of their targets (in-place). Mirrors the
/// `rm_link.sh` behavior for libs (e.g. boost) that ship symlinks.
pub fn dereference_symlinks(root: &Path) -> Result<()> {
    let mut links: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        if entry.file_type().is_symlink() {
            links.push(entry.path().to_path_buf());
        }
    }
    // deepest first so we don't replace a parent dir-symlink before its children
    links.sort_by_key(|p| std::cmp::Reverse(p.components().count()));

    for link in links {
        let target = match std::fs::read_link(&link) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let resolved = if target.is_absolute() {
            target
        } else {
            link.parent().unwrap_or(Path::new(".")).join(target)
        };
        // remove the symlink itself. On Unix `remove_file` handles both file and
        // dir symlinks; on Windows a dir symlink needs `remove_dir` (and never
        // `remove_dir_all`, which would recurse into the link's target).
        if !std::fs::remove_file(&link).is_ok() {
            let _ = std::fs::remove_dir(&link);
        }
        if resolved.is_dir() {
            copy_dir_recursive(&resolved, &link)?;
        } else if let Ok(_) = std::fs::metadata(&resolved) {
            if let Some(parent) = link.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::copy(&resolved, &link)?;
        }
    }
    Ok(())
}

pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in walkdir::WalkDir::new(src).into_iter().flatten() {
        let rel = entry.path().strip_prefix(src)?;
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

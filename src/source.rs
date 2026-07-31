//! Source acquisition: turn a `--kind` source (git remote, archive URI, or local
//! path) into a source tree the shared pipeline (clean_vcs -> deref -> zip) packs.
//!
//! Invariant (assumed, never verified): every source named for a given version
//! yields the same source snapshot. We do not cross-check git vs fetch content.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Git,
    Fetch,
}

/// Extensions treated as fetch archives (lowercased).
const ARCH_EXTS: &[&str] = &[
    ".zip", ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.bz2", ".tbz2", ".tar",
];

fn basename(url: &str) -> &str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/').next().unwrap_or(path)
}

pub fn strip_archive_ext(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    for e in ARCH_EXTS {
        if lower.ends_with(e) {
            return &name[..name.len() - e.len()];
        }
    }
    name
}

/// basename of `url` with any archive extension removed — the thing version
/// derivation looks at for fetch sources.
pub fn url_stem(url: &str) -> &str {
    strip_archive_ext(basename(url))
}

fn has_archive_ext(url: &str) -> bool {
    let lower = basename(url).to_ascii_lowercase();
    ARCH_EXTS.iter().any(|e| lower.ends_with(e))
}

/// Classify a source by its URL/path. Archive extension or a local file/dir ->
/// Fetch; a local git-repo dir -> Git; otherwise Git (remote URL).
pub fn detect_kind(url: &str) -> Kind {
    if has_archive_ext(url) {
        return Kind::Fetch;
    }
    let p = Path::new(url);
    if p.exists() {
        if p.is_dir() && p.join(".git").exists() {
            return Kind::Git;
        }
        return Kind::Fetch;
    }
    Kind::Git
}

/// Download `url` into `dst` (HTTP GET via ureq).
pub fn download(url: &str, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let resp = ureq::get(url).set("User-Agent", "cpm-cli").call()?;
    let mut file = fs::File::create(dst)?;
    let mut reader = resp.into_reader();
    std::io::copy(&mut reader, &mut file)?;
    Ok(())
}

/// Acquire the source tree into `work` (a fresh temp dir). Returns the source
/// root (== `work` for git/dir; possibly a single top-level subdir for archives).
pub fn acquire(url: &str, kind: Kind, tag: Option<&str>, force_commit: bool, work: &Path) -> Result<PathBuf> {
    if work.exists() {
        let _ = fs::remove_dir_all(work);
    }
    fs::create_dir_all(work)?;
    match kind {
        Kind::Git => {
            let t = tag.with_context(|| "git source needs a tag/branch/commit")?;
            crate::git::shallow_clone(url, t, work, force_commit)?;
            Ok(work.to_path_buf())
        }
        Kind::Fetch => {
            let p = Path::new(url);
            if p.is_dir() {
                crate::git::copy_dir_recursive(p, work)?;
                return Ok(work.to_path_buf());
            }
            let archfile = if p.is_file() {
                p.to_path_buf()
            } else {
                let dst = work.join(basename(url));
                println!("downloading {} ...", url);
                download(url, &dst)?;
                dst
            };
            println!("extracting {} ...", archfile.display());
            extract_archive(&archfile, work)?;
            let root = unwrap_single_top(work);
            if root != work {
                println!(
                    "  (single top-level dir: {})",
                    root.file_name().unwrap_or_default().to_string_lossy()
                );
            }
            Ok(root)
        }
    }
}

fn extract_archive(archive: &Path, destdir: &Path) -> Result<()> {
    let lower = archive.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".zip") {
        extract_zip(archive, destdir)
    } else {
        extract_tar(archive, destdir)
    }
}

fn extract_zip(archive: &Path, destdir: &Path) -> Result<()> {
    let file = fs::File::open(archive)?;
    let mut za = zip::ZipArchive::new(file)?;
    for i in 0..za.len() {
        let mut entry = za.by_index(i)?;
        let name = entry.name().to_string();
        if name.ends_with('/') || entry.is_dir() {
            fs::create_dir_all(destdir.join(&name))?;
            continue;
        }
        // zip-slip guard
        if name.starts_with('/') || name.split('/').any(|c| c == "..") {
            bail!("unsafe zip entry: {name}");
        }
        let out = destdir.join(&name);
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut of = fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut of)?;
    }
    Ok(())
}

/// Shell out to the system `tar` (handles .tar/.tar.gz/.tar.xz/.tar.bz2 uniformly).
fn extract_tar(archive: &Path, destdir: &Path) -> Result<()> {
    let st = Command::new("tar")
        .arg("xf")
        .arg(archive)
        .arg("-C")
        .arg(destdir)
        .status()
        .with_context(|| format!("invoke tar for {}", archive.display()))?;
    if !st.success() {
        bail!("tar extract failed (exit {:?})", st.code());
    }
    Ok(())
}

/// If `dir` holds exactly one subdir, return it (typical tarball shape); else `dir`.
fn unwrap_single_top(dir: &Path) -> PathBuf {
    let entries: Vec<_> = match fs::read_dir(dir) {
        Ok(it) => it.flatten().collect(),
        Err(_) => return dir.to_path_buf(),
    };
    if entries.len() == 1 && entries[0].path().is_dir() {
        entries[0].path()
    } else {
        dir.to_path_buf()
    }
}

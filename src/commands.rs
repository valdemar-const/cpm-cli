//! Command implementations.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::archive;
use crate::config;
use crate::deps::{self, Dep};
use crate::git;

// ---- helpers ---------------------------------------------------------------

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Derive a clean version string from a tag (handles `vX`, `name-X`, `release-X`).
pub fn strip_version(tag: &str, name: &str) -> String {
    let lower = tag.to_ascii_lowercase();
    let name_prefix = format!("{}-", name.to_ascii_lowercase());
    if lower.starts_with(&name_prefix) {
        tag[name_prefix.len()..].to_string()
    } else if lower.starts_with("release-") {
        tag["release-".len()..].to_string()
    } else if lower.starts_with('v')
        && lower[1..].chars().next().map_or(false, |c| c.is_ascii_digit())
    {
        tag[1..].to_string()
    } else {
        tag.to_string()
    }
}

fn default_archive_name(name: &str, tag: &str) -> String {
    format!("{}-{}.zip", name, strip_version(tag, name))
}

fn human_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", b, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

// ---- add -------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn add(
    name: &str,
    url: &str,
    tag: &str,
    archive_override: Option<&str>,
    version_override: Option<&str>,
    force_commit: bool,
    dereference: bool,
    force: bool,
) -> Result<()> {
    config::ensure_dirs()?;
    let preload = config::preload_dir()?;
    let mut reg = deps::load()?;

    let archive = archive_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| default_archive_name(name, tag));
    let archive_path = preload.join(&archive);

    if archive_path.exists() {
        if force {
            let _ = fs::remove_file(&archive_path);
        } else {
            bail!(
                "archive already exists: {}\n  use --force to overwrite, or --archive <name>",
                archive_path.display()
            );
        }
    }

    let tmp_root = config::tmp_dir()?;
    let clone_dir = tmp_root.join(format!(
        "{}-{}",
        sanitize_fs(name),
        sanitize_fs(tag)
    ));
    if clone_dir.exists() {
        let _ = fs::remove_dir_all(&clone_dir);
    }

    println!("cloning {} @ {} ...", url, tag);
    git::shallow_clone(url, tag, &clone_dir, force_commit)?;

    let has_submodules = clone_dir.join(".gitmodules").exists();

    println!("cleaning VCS metadata ...");
    git::clean_vcs(&clone_dir)?;

    if dereference {
        println!("dereferencing symlinks ...");
        git::dereference_symlinks(&clone_dir)?;
    }

    println!("packing {} ...", archive);
    archive::zip_dir(&clone_dir, &archive_path)?;
    let size = fs::metadata(&archive_path)?.len();
    let sha = archive::sha256_file(&archive_path)?;

    // remove temp clone
    let _ = fs::remove_dir_all(&clone_dir);

    let version = version_override
        .map(|s| s.to_string())
        .or_else(|| Some(strip_version(tag, name)));

    reg.deps.insert(
        name.to_string(),
        Dep {
            url: url.to_string(),
            tag: tag.to_string(),
            archive,
            version,
            sha256: Some(sha),
            added_at: Some(now_rfc3339()),
            submodules: Some(has_submodules),
        },
    );
    deps::save(&reg)?;

    println!(
        "added {} -> {} ({})",
        name,
        archive_path.display(),
        human_bytes(size)
    );
    println!("use `cpm show {}` to get the CPMAddPackage snippet", name);
    Ok(())
}

fn sanitize_fs(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect()
}

// ---- fetch -----------------------------------------------------------------

pub fn fetch(force: bool) -> Result<()> {
    let reg = deps::load()?;
    if reg.deps.is_empty() {
        println!("(registry is empty; use `cpm add <name> <url> <tag>` first)");
        return Ok(());
    }
    for (name, dep) in reg.deps.iter() {
        let path = config::preload_dir()?.join(&dep.archive);
        if path.exists() && !force {
            println!("skip {} ({})", name, dep.archive);
            continue;
        }
        println!("==> {} @ {}", name, dep.tag);
        add(
            name,
            &dep.url,
            &dep.tag,
            Some(&dep.archive),
            dep.version.as_deref(),
            git::looks_like_commit(&dep.tag),
            false,
            true,
        )?;
    }
    Ok(())
}

// ---- list ------------------------------------------------------------------

pub fn list() -> Result<()> {
    let reg = deps::load()?;
    if reg.deps.is_empty() {
        println!("(registry is empty; use `cpm add <name> <url> <tag>` first)");
        return Ok(());
    }
    let preload = config::preload_dir_opt();

    println!(
        "{:<20} {:<12} {:<22} {:<10} {:<32} STATUS",
        "NAME", "VERSION", "TAG", "SIZE", "ARCHIVE"
    );
    println!("{}", "-".repeat(110));
    for (name, dep) in &reg.deps {
        let version = dep.version.clone().unwrap_or_else(|| "-".into());
        let (size_str, status) = match &preload {
            Some(p) => {
                let path = p.join(&dep.archive);
                if path.exists() {
                    let sz = fs::metadata(&path).map(|m| human_bytes(m.len())).unwrap_or_default();
                    (sz, "ok".to_string())
                } else {
                    ("-".into(), "MISSING".to_string())
                }
            }
            None => ("?".into(), "no CPM_PRELOAD".to_string()),
        };
        println!(
            "{:<20} {:<12} {:<22} {:<10} {:<32} {}",
            name, version, dep.tag, size_str, dep.archive, status
        );
    }
    Ok(())
}

// ---- show ------------------------------------------------------------------

pub fn show(name: &str, with_hash: bool) -> Result<()> {
    let reg = deps::load()?;
    let dep = reg
        .deps
        .get(name)
        .with_context(|| format!("unknown dep '{name}'"))?;
    let preload = config::preload_dir()?;
    let path = preload.join(&dep.archive);
    let size = fs::metadata(&path).map(|m| human_bytes(m.len())).ok();
    let version = dep.version.clone().unwrap_or_else(|| dep.tag.clone());

    println!("# {}   tag {}   ({}, {})", name, dep.tag, dep.archive, size.unwrap_or_else(|| "missing".into()));

    println!("CPMAddPackage(");
    println!("  NAME {}", name);
    println!("  VERSION {}", version);
    println!("  URL \"file://{}\"", path.display());
    if with_hash {
        let sha = if let Some(s) = &dep.sha256 {
            s.clone()
        } else {
            archive::sha256_file(&path).unwrap_or_default()
        };
        if !sha.is_empty() {
            println!("  URL_HASH \"SHA256={}\"", sha);
        }
    }
    println!(")");
    Ok(())
}

// ---- verify ----------------------------------------------------------------

pub fn verify(target: &str) -> Result<()> {
    let path = resolve_target(target)?;
    if !path.exists() {
        bail!("not found: {}", path.display());
    }
    let file = fs::File::open(&path)?;
    let mut za = zip::ZipArchive::new(file)?;

    let mut n_files = 0usize;
    let mut n_dirs = 0usize;
    let mut total_uncompressed = 0u64;
    let mut git_contaminated = 0usize;
    let mut top_level: std::collections::BTreeSet<String> = Default::default();
    let mut has_cmakelists_top = false;
    let mut has_cmakelists_any = false;

    for i in 0..za.len() {
        let entry = za.by_index(i)?;
        let name = entry.name().to_string();
        let is_dir = entry.is_dir();
        if is_dir {
            n_dirs += 1;
        } else {
            n_files += 1;
            total_uncompressed += entry.size();
        }
        if name.contains(".git/") || name == ".git" || name.starts_with(".git/") {
            git_contaminated += 1;
        }
        if let Some(top) = name.split('/').next() {
            if !top.is_empty() {
                top_level.insert(top.to_string());
            }
        }
        if name.ends_with("CMakeLists.txt") {
            has_cmakelists_any = true;
            if !name.contains('/') {
                has_cmakelists_top = true;
            }
        }
    }

    let sha = archive::sha256_file(&path)?;
    let on_disk = fs::metadata(&path)?.len();

    println!("archive : {}", path.display());
    println!("sha256  : {}", sha);
    println!("size    : {} (on disk), {} uncompressed", human_bytes(on_disk), human_bytes(total_uncompressed));
    println!("entries : {} files, {} dirs", n_files, n_dirs);
    println!(
        "cmake   : {}",
        match (has_cmakelists_top, has_cmakelists_any) {
            (true, _) => "CMakeLists.txt at top level",
            (false, true) => "CMakeLists.txt present (nested)",
            (false, false) => "NO CMakeLists.txt found",
        }
    );
    println!(
        ".git    : {}",
        if git_contaminated == 0 {
            "clean".to_string()
        } else {
            format!("CONTAMINATED ({} entries)", git_contaminated)
        }
    );
    println!(
        "toplevel: {} ({} entr{})",
        top_level.len(),
        top_level.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
        if top_level.len() > 5 { " ..." } else { "" }
    );

    // cross-check with registry if it was a dep name
    if let Some(stripped) = path.file_name().and_then(|s| s.to_str()) {
        let reg = deps::load()?;
        for (name, dep) in &reg.deps {
            if dep.archive == stripped {
                match &dep.sha256 {
                    Some(s) if s == &sha => println!("registry: {} — hash matches", name),
                    Some(_) => println!("registry: {} — HASH MISMATCH", name),
                    None => println!("registry: {} (no stored hash)", name),
                }
                break;
            }
        }
    }
    Ok(())
}

fn resolve_target(target: &str) -> Result<PathBuf> {
    let as_path = Path::new(target);
    if as_path.is_file() {
        return Ok(as_path.to_path_buf());
    }
    // try as archive filename inside CPM_PRELOAD
    if let Ok(preload) = config::preload_dir() {
        let p = preload.join(target);
        if p.is_file() {
            return Ok(p);
        }
    }
    // try as a dep name in the registry
    let reg = deps::load()?;
    if let Some(dep) = reg.deps.get(target) {
        return Ok(config::preload_dir()?.join(&dep.archive));
    }
    Ok(as_path.to_path_buf())
}

// ---- env -------------------------------------------------------------------

pub fn env(export: bool) -> Result<()> {
    let home = config::cpm_home()?;
    let depsp = config::deps_file()?;
    let cfgp = config::config_file()?;
    let pre = config::preload_dir_opt();
    let sc = config::source_cache_opt();
    let reg = deps::load()?;

    let cpmver = reg.meta.cpm_version.clone();
    let cpm_dir = config::cpm_cmake_dir()?;
    let cpm_cmake = match &cpmver {
        Some(v) => {
            let p = cpm_dir.join(v).join("CPM.cmake");
            if p.exists() {
                p.display().to_string()
            } else {
                "(not bootstrapped; run `cpm bootstrap`)".into()
            }
        }
        None => "(no version selected; run `cpm bootstrap`)".into(),
    };

    println!("CPM_HOME         {}", home.display());
    println!(
        "CPM_PRELOAD      {}",
        pre.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(unset)".into())
    );
    println!(
        "CPM_SOURCE_CACHE {}",
        sc.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(unset)".into())
    );
    println!("deps registry    {} ({} deps)", depsp.display(), reg.deps.len());
    println!("config           {}", cfgp.display());
    println!("CPM.cmake        {}", cpm_cmake);

    if export {
        println!();
        println!("# append to your shell rc:");
        println!("export CPM_SOURCE_CACHE=\"{}\"", sc.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "$HOME/cmake/cpm_cache".into()));
        if let Some(p) = &pre {
            println!("export CPM_PRELOAD=\"{}\"", p.display());
        }
        println!("export CPM_HOME=\"{}\"", home.display());
    }
    Ok(())
}

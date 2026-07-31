//! Command implementations.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::archive;
use crate::config;
use crate::deps::{self, Dep};
use crate::git;

// ---- helpers ---------------------------------------------------------------

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Normalise a version to canonical MAJOR.MINOR.PATCH (numeric dot-parts padded
/// / truncated to three). None if any part is non-numeric.
pub fn canonicalize_version(s: &str) -> Option<String> {
    let mut comps: Vec<&str> = Vec::new();
    for part in s.split('.') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        comps.push(part);
    }
    if comps.is_empty() {
        return None;
    }
    while comps.len() < 3 {
        comps.push("0");
    }
    Some(comps.iter().take(3).copied().collect::<Vec<_>>().join("."))
}

/// Extract a canonical semver from an arbitrary git tag.
///
/// Handles `v1.0.0`, `boost-1.90.0`, `boost-version-1.90` (any prefix is fine
/// provided a MAJOR.MINOR[.PATCH] numeric run exists). Requires at least
/// MAJOR.MINOR so numbers embedded in a project name aren't mistaken for it.
/// Returns None for dash-separated tags like `VER-2-14-1` — the caller must
/// then pass `--version` explicitly.
pub fn extract_semver(tag: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut j = i;
            let mut comps = 0usize;
            loop {
                let b = j;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j == b {
                    break;
                }
                comps += 1;
                if j < bytes.len() && bytes[j] == b'.' {
                    j += 1;
                    continue;
                }
                break;
            }
            if comps >= 2 {
                return canonicalize_version(tag[start..j].trim_end_matches('.'));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    None
}

/// Resolve the package version: explicit `--version` wins (and is canonicalised),
/// otherwise derive from the tag. Errors when neither yields a semver.
pub fn resolve_version(version_override: Option<&str>, tag: &str) -> Result<String> {
    if let Some(v) = version_override {
        return canonicalize_version(v).ok_or_else(|| {
            anyhow::anyhow!(
                "`--version {v}` is not canonical MAJOR.MINOR.PATCH (numeric dot-parts only)"
            )
        });
    }
    extract_semver(tag).ok_or_else(|| {
        anyhow::anyhow!(
            "could not derive a semver (MAJOR.MINOR.PATCH) from tag `{tag}`\n\
             this happens with non-standard tag formats (e.g. `VER-2-14-1`).\n\
             pass the version explicitly: --version X.Y.Z"
        )
    })
}

/// `<lowercase(name)>-<canonical-version>.zip`
fn default_archive_name(name: &str, version: &str) -> String {
    format!("{}-{}.zip", name.to_ascii_lowercase(), version)
}

/// Parse `<name>-<version>.zip` into (lowercase name, verbatim version).
/// Returns None unless a `-<digit>` boundary exists. Version is kept verbatim
/// (no canonicalisation) so distinct packings (e.g. `3.4` vs `3.4.0.20251217`)
/// don't collide.
fn parse_archive_name_version(filename: &str) -> Option<(String, String)> {
    let stem = filename.strip_suffix(".zip")?;
    let bytes = stem.as_bytes();
    let mut split = None;
    for i in 0..bytes.len() {
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            split = Some(i);
        }
    }
    let i = split?;
    Some((stem[..i].to_ascii_lowercase(), stem[i + 1..].to_string()))
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
    force: bool,
) -> Result<()> {
    config::ensure_dirs()?;
    let preload = config::preload_dir()?;
    let mut reg = deps::load()?;

    let version = resolve_version(version_override, tag)?;
    let archive = archive_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| default_archive_name(name, &version));
    let archive_path = preload.join(&archive);

    if archive_path.exists() && !force {
        // Tarball already present: trust it, just attach the source (url+tag) to
        // the registry entry — no re-download, no repack.
        let sha = archive::sha256_file(&archive_path)?;
        let size = fs::metadata(&archive_path)?.len();
        let submodules = reg.pick(name, Some(&version)).and_then(|d| d.submodules);
        reg.upsert(Dep {
            name: name.to_ascii_lowercase(),
            url: Some(url.to_string()),
            tag: Some(tag.to_string()),
            archive: archive.clone(),
            version: Some(version.clone()),
            sha256: Some(sha),
            added_at: Some(now_rfc3339()),
            submodules,
        });
        deps::save(&reg)?;
        println!(
            "attached source to existing {} ({})",
            archive,
            human_bytes(size)
        );
        println!("  {} @ {}", url, tag);
        println!("  (pass --force to re-fetch and repack)");
        return Ok(());
    }
    if archive_path.exists() {
        let _ = fs::remove_file(&archive_path);
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

    println!("dereferencing symlinks ...");
    git::dereference_symlinks(&clone_dir)?;

    println!("packing {} ...", archive);
    archive::zip_dir(&clone_dir, &archive_path)?;
    let size = fs::metadata(&archive_path)?.len();
    let sha = archive::sha256_file(&archive_path)?;

    // remove temp clone
    let _ = fs::remove_dir_all(&clone_dir);

    reg.upsert(Dep {
        name: name.to_ascii_lowercase(),
        url: Some(url.to_string()),
        tag: Some(tag.to_string()),
        archive,
        version: Some(version),
        sha256: Some(sha),
        added_at: Some(now_rfc3339()),
        submodules: Some(has_submodules),
    });
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
    for dep in &reg.deps {
        let (url, tag) = match (dep.url.as_deref(), dep.tag.as_deref()) {
            (Some(u), Some(t)) => (u.to_string(), t.to_string()),
            _ => {
                println!("skip {} ({}) — no source (loc-only)", dep.name, dep.archive);
                continue;
            }
        };
        let path = config::preload_dir()?.join(&dep.archive);
        if path.exists() && !force {
            println!("skip {} ({})", dep.name, dep.archive);
            continue;
        }
        println!("==> {} @ {}", dep.name, tag);
        add(
            &dep.name,
            &url,
            &tag,
            Some(&dep.archive),
            dep.version.as_deref(),
            git::looks_like_commit(&tag),
            true,
        )?;
    }
    Ok(())
}

// ---- import ----------------------------------------------------------------

/// Register archives already present in `$CPM_PRELOAD` into the pantry.
///
/// Default: trust the content — parse `<name>-<version>.zip`, compute sha256,
/// register as loc-tier only (no source). `-f`: re-fetch from source; requires
/// a pantry entry with url+tag, otherwise errors.
pub fn import(force: bool) -> Result<()> {
    config::ensure_dirs()?;
    let preload = config::preload_dir()?;

    let mut files: Vec<String> = fs::read_dir(&preload)?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("zip") {
                p.file_name()?.to_str().map(String::from)
            } else {
                None
            }
        })
        .collect();
    files.sort();

    if force {
        import_force(&files)
    } else {
        import_trust(&preload, &files)
    }
}

fn import_trust(preload: &Path, files: &[String]) -> Result<()> {
    let mut reg = deps::load()?;
    let mut imported = 0u32;
    let mut skipped = 0u32;
    for fname in files {
        let (name, version) = match parse_archive_name_version(fname) {
            Some(nv) => nv,
            None => {
                println!("skip   {} (not <name>-<version>.zip)", fname);
                skipped += 1;
                continue;
            }
        };
        let sha = archive::sha256_file(&preload.join(fname))?;
        reg.upsert(Dep {
            name,
            url: None,
            tag: None,
            archive: fname.clone(),
            version: Some(version),
            sha256: Some(sha),
            added_at: Some(now_rfc3339()),
            submodules: None,
        });
        println!("added  {}", fname);
        imported += 1;
    }
    deps::save(&reg)?;
    println!("imported {} archive(s), skipped {}", imported, skipped);
    Ok(())
}

fn import_force(files: &[String]) -> Result<()> {
    let reg = deps::load()?;
    let mut re_fetched = 0u32;
    let mut unsourced: Vec<String> = Vec::new();
    for fname in files {
        let (name, version) = match parse_archive_name_version(fname) {
            Some(nv) => nv,
            None => continue,
        };
        match reg.pick(&name, Some(&version)) {
            Some(d) if d.url.is_some() && d.tag.is_some() => {
                let url = d.url.clone().unwrap();
                let tag = d.tag.clone().unwrap();
                println!("==> re-fetch {} {} from {}", name, version, url);
                add(
                    &name,
                    &url,
                    &tag,
                    Some(fname),
                    Some(&version),
                    git::looks_like_commit(&tag),
                    true,
                )?;
                re_fetched += 1;
            }
            _ => unsourced.push(fname.clone()),
        }
    }
    if !unsourced.is_empty() {
        bail!(
            "no source to re-fetch for {} archive(s):\n  {}\n\
             run `cpm import` without -f to trust them, or `cpm add <name> <url> <tag>` to add a source.",
            unsourced.len(),
            unsourced.join("\n  ")
        );
    }
    println!("re-fetched {} archive(s)", re_fetched);
    Ok(())
}

// ---- list ------------------------------------------------------------------

pub fn list() -> Result<()> {
    let reg = deps::load()?;
    if reg.deps.is_empty() {
        println!("(registry is empty; use `cpm add` or `cpm import`)");
        return Ok(());
    }
    let preload = config::preload_dir_opt();

    let mut entries: Vec<&Dep> = reg.deps.iter().collect();
    entries.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| {
                deps::version_key(b.version.as_deref().unwrap_or("0"))
                    .cmp(&deps::version_key(a.version.as_deref().unwrap_or("0")))
            })
    });

    println!(
        "{:<20} {:<14} {:<24} {:<10} {:<32} STATUS",
        "NAME", "VERSION", "TAG", "SIZE", "ARCHIVE"
    );
    println!("{}", "-".repeat(114));
    for dep in &entries {
        let version = dep.version.clone().unwrap_or_else(|| "-".into());
        let tag = dep.tag.clone().unwrap_or_else(|| "-".into());
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
            "{:<20} {:<14} {:<24} {:<10} {:<32} {}",
            dep.name, version, tag, size_str, dep.archive, status
        );
    }
    Ok(())
}

// ---- show ------------------------------------------------------------------

pub fn show(name: &str, with_hash: bool) -> Result<()> {
    let reg = deps::load()?;
    let all = reg.versions(name);
    if all.is_empty() {
        bail!("unknown dep '{name}'");
    }
    let dep = &all[0];
    let preload = config::preload_dir()?;
    let path = preload.join(&dep.archive);
    let size = fs::metadata(&path).map(|m| human_bytes(m.len())).ok();
    let version = dep.version.clone().unwrap_or_else(|| "?".into());
    let tag = dep.tag.clone().unwrap_or_else(|| "-".into());

    if all.len() > 1 {
        let vers: Vec<_> = all.iter().map(|d| d.version.clone().unwrap_or_else(|| "?".into())).collect();
        println!("# {} versions: {} (showing {})", dep.name, vers.join(", "), version);
    }
    println!(
        "# {}   version {}   tag {}   ({}, {})",
        dep.name,
        version,
        tag,
        dep.archive,
        size.unwrap_or_else(|| "missing".into())
    );

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
        for dep in &reg.deps {
            if dep.archive == stripped {
                match &dep.sha256 {
                    Some(s) if s == &sha => println!("registry: {} — hash matches", dep.name),
                    Some(_) => println!("registry: {} — HASH MISMATCH", dep.name),
                    None => println!("registry: {} (no stored hash)", dep.name),
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
    // try as a dep name in the registry (freshest version)
    let reg = deps::load()?;
    if let Some(dep) = reg.pick(target, None) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_handles_padding_and_truncation() {
        assert_eq!(canonicalize_version("1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(canonicalize_version("1.2").as_deref(), Some("1.2.0"));
        assert_eq!(canonicalize_version("1").as_deref(), Some("1.0.0"));
        assert_eq!(canonicalize_version("1.2.3.4").as_deref(), Some("1.2.3"));
        assert_eq!(canonicalize_version("1.92.801").as_deref(), Some("1.92.801"));
        // non-numeric / prefixed => rejected
        assert_eq!(canonicalize_version("1.0.0-rc1"), None);
        assert_eq!(canonicalize_version("v1.0"), None);
        assert_eq!(canonicalize_version(""), None);
    }

    #[test]
    fn extract_semver_from_common_tag_shapes() {
        // v-prefix
        assert_eq!(extract_semver("v1.0.0").as_deref(), Some("1.0.0"));
        assert_eq!(extract_semver("v6.2").as_deref(), Some("6.2.0"));
        // name-prefix, with or without an inner "version"
        assert_eq!(extract_semver("boost-1.90.0").as_deref(), Some("1.90.0"));
        assert_eq!(extract_semver("boost-version-1.90").as_deref(), Some("1.90.0"));
        // bare
        assert_eq!(extract_semver("1.92.801").as_deref(), Some("1.92.801"));
        assert_eq!(extract_semver("10.2.1").as_deref(), Some("10.2.1"));
        // number embedded in name must not win
        assert_eq!(extract_semver("libfoo2-1.0.0").as_deref(), Some("1.0.0"));
    }

    #[test]
    fn extract_semver_rejects_non_semver_tags() {
        // dash-separated / non-standard => no derivation
        assert_eq!(extract_semver("VER-2-14-1"), None);
        assert_eq!(extract_semver("2024-01-15"), None);
        assert_eq!(extract_semver("main"), None);
        assert_eq!(extract_semver("v5"), None);
    }

    #[test]
    fn resolve_version_requires_explicit_when_tag_opaque() {
        // explicit override canonicalises
        assert_eq!(
            resolve_version(Some("2.14.1"), "VER-2-14-1").unwrap(),
            "2.14.1"
        );
        assert_eq!(resolve_version(Some("6.2"), "6").unwrap(), "6.2.0");
        // opaque tag without override => error
        assert!(resolve_version(None, "VER-2-14-1").is_err());
        // bad override => error
        assert!(resolve_version(Some("v1"), "v1").is_err());
    }

    #[test]
    fn archive_name_is_lowercase_name_plus_semver() {
        assert_eq!(default_archive_name("imgui_bundle", "1.92.801"), "imgui_bundle-1.92.801.zip");
        assert_eq!(default_archive_name("ImGuiBundle", "1.0.0"), "imguibundle-1.0.0.zip");
    }
}

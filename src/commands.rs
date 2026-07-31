//! Command implementations.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::archive;
use crate::config;
use crate::deps::{self, Dep};
use crate::gen;
use crate::git;
use crate::source;
use crate::spec;

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

/// Resolve the package version for a fetch source: explicit `--version` wins,
/// otherwise derive a semver from the archive's basename. Errors if neither.
fn resolve_fetch_version(version_override: Option<&str>, url: &str) -> Result<String> {
    if let Some(v) = version_override {
        return canonicalize_version(v).ok_or_else(|| {
            anyhow::anyhow!("`--version {v}` is not canonical MAJOR.MINOR.PATCH (numeric dot-parts only)")
        });
    }
    let stem = source::url_stem(url);
    if let Some(v) = extract_semver(stem) {
        return Ok(v);
    }
    anyhow::bail!(
        "could not derive a version (MAJOR.MINOR.PATCH) from `{stem}`.\n\
         pass it explicitly: --version X.Y.Z"
    )
}

#[allow(clippy::too_many_arguments)]
pub fn add(
    name: &str,
    url: &str,
    tag: Option<&str>,
    kind: source::Kind,
    archive_override: Option<&str>,
    version_override: Option<&str>,
    force_commit: bool,
    force: bool,
) -> Result<()> {
    config::ensure_dirs()?;
    let preload = config::preload_dir()?;
    let mut reg = deps::load()?;

    let git_tag: Option<&str> = tag;
    let version = match kind {
        source::Kind::Git => {
            let t = git_tag.ok_or_else(|| {
                anyhow::anyhow!("git source needs a tag/branch/commit (positional arg 3)")
            })?;
            resolve_version(version_override, t)?
        }
        source::Kind::Fetch => {
            if git_tag.is_some() {
                println!("(ignoring tag for --kind=fetch)");
            }
            resolve_fetch_version(version_override, url)?
        }
    };
    let existing = reg.pick(name, Some(&version)).cloned();
    let archive = archive_override
        .map(|s| s.to_string())
        .or_else(|| existing.as_ref().map(|d| d.archive.clone()))
        .unwrap_or_else(|| default_archive_name(name, &version));
    let archive_path = preload.join(&archive);

    if archive_path.exists() && !force {
        // Archive already present: by the source-equivalence invariant we trust
        // it. For git we record/refresh the url+tag provenance; for fetch there
        // is nothing to store (the archive is the snapshot). No re-download.
        let sha = archive::sha256_file(&archive_path)?;
        let size = fs::metadata(&archive_path)?.len();
        let key = deps::version_key(&version);
        let has_entry = slot_of(&reg, name, key).is_some();
        match kind {
            source::Kind::Git => {
                let t = git_tag.unwrap();
                if !attach_in_place(&mut reg, name, key, url, t, &sha) {
                    let submodules = existing.as_ref().and_then(|d| d.submodules);
                    reg.upsert(Dep {
                        name: name.to_ascii_lowercase(),
                        url: Some(url.to_string()),
                        tag: Some(t.to_string()),
                        archive: archive.clone(),
                        version: Some(version.clone()),
                        sha256: Some(sha),
                        added_at: Some(now_rfc3339()),
                        submodules,
                    });
                }
                deps::save(&reg)?;
                println!("{} already present ({}) — source recorded", archive, human_bytes(size));
                println!("  {} @ {}", url, t);
                println!("  (pass --force to re-fetch and repack)");
            }
            source::Kind::Fetch => {
                if !has_entry {
                    reg.upsert(Dep {
                        name: name.to_ascii_lowercase(),
                        url: None,
                        tag: None,
                        archive: archive.clone(),
                        version: Some(version.clone()),
                        sha256: Some(sha),
                        added_at: Some(now_rfc3339()),
                        submodules: None,
                    });
                    deps::save(&reg)?;
                    println!("registered {} ({})", archive, human_bytes(size));
                } else {
                    deps::save(&reg)?;
                    println!("{} already present ({}) — kept", archive, human_bytes(size));
                }
                println!("  (pass --force to re-fetch and repack)");
            }
        }
        return Ok(());
    }
    if archive_path.exists() {
        let _ = fs::remove_file(&archive_path);
    }

    let tmp_root = config::tmp_dir()?;
    let work = tmp_root.join(format!(
        "{}-{}",
        sanitize_fs(name),
        sanitize_fs(&version)
    ));

    println!("acquiring {} ({:?}) ...", url, kind);
    let src_root = source::acquire(url, kind, git_tag, force_commit, &work)?;

    let has_submodules = src_root.join(".gitmodules").exists();

    println!("cleaning VCS metadata ...");
    git::clean_vcs(&src_root)?;

    println!("dereferencing symlinks ...");
    git::dereference_symlinks(&src_root)?;

    println!("packing {} ...", archive);
    archive::zip_dir(&src_root, &archive_path)?;
    let size = fs::metadata(&archive_path)?.len();
    let sha = archive::sha256_file(&archive_path)?;

    // remove temp work tree
    let _ = fs::remove_dir_all(&work);

    // keep a pre-existing verbatim version (e.g. "3.4") if any, so repacking
    // doesn't rewrite it to the canonical form ("3.4.0").
    let store_version = existing
        .as_ref()
        .and_then(|d| d.version.clone())
        .unwrap_or_else(|| version.clone());
    let (surl, stag) = match kind {
        source::Kind::Git => (Some(url.to_string()), git_tag.map(|s| s.to_string())),
        source::Kind::Fetch => (None, None),
    };
    reg.upsert(Dep {
        name: name.to_ascii_lowercase(),
        url: surl,
        tag: stag,
        archive,
        version: Some(store_version),
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

/// Set url+tag and refresh sha/added_at on an existing pantry entry in place,
/// preserving its version and archive name. Returns false if no matching entry.
fn attach_in_place(
    reg: &mut deps::Registry,
    name: &str,
    key: (u64, u64, u64),
    url: &str,
    tag: &str,
    sha: &str,
) -> bool {
    for d in &mut reg.deps {
        if d.name.eq_ignore_ascii_case(name)
            && d.version.as_deref().map(deps::version_key).as_ref() == Some(&key)
        {
            d.url = Some(url.to_string());
            d.tag = Some(tag.to_string());
            d.sha256 = Some(sha.to_string());
            d.added_at = Some(now_rfc3339());
            return true;
        }
    }
    false
}

/// First pantry entry matching `name` + numeric version key (index), or None.
fn slot_of(reg: &deps::Registry, name: &str, key: (u64, u64, u64)) -> Option<usize> {
    reg.deps.iter().position(|d| {
        d.name.eq_ignore_ascii_case(name) && d.version.as_deref().map(deps::version_key).as_ref() == Some(&key)
    })
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
            Some(&tag),
            source::Kind::Git,
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
                    Some(&tag),
                    source::Kind::Git,
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

    let headers = ["NAME", "VERSION", "TAG", "SIZE", "ARCHIVE", "STATUS"];
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(entries.len());
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
        rows.push(vec![
            dep.name.clone(),
            version,
            tag,
            size_str,
            dep.archive.clone(),
            status,
        ]);
    }
    // flex columns (shrink to fit): NAME=0, TAG=2, ARCHIVE=4
    print_table(&headers, &rows, &[0, 2, 4]);
    Ok(())
}

// ---- table layout -----------------------------------------------------------

/// Column widths are derived from content; when stdout is a terminal whose
/// width cannot hold the natural layout, the listed `flex` columns are shaved
/// (the widest first) down to a readable floor, and over-long cells are
/// truncated with an ellipsis. Non-tty output (pipes/redirects) is left intact.
fn print_table(headers: &[&str], rows: &[Vec<String>], flex: &[usize]) {
    let ncols = headers.len();
    // per-column floor: header width (so the header always fits), min 6.
    let floors: Vec<usize> = headers.iter().map(|h| h.chars().count().max(6)).collect();
    let mut w = floors.clone();
    for r in rows {
        for (i, cell) in r.iter().enumerate() {
            if i < ncols {
                w[i] = w[i].max(cell.chars().count());
            }
        }
    }
    let seps = ncols.saturating_sub(1);
    let mut total: usize = w.iter().sum::<usize>() + seps;

    if let Some(tw) = term_width() {
        // shave the widest flex column by one until we fit (or hit all floors)
        while total > tw {
            let pick = flex
                .iter()
                .copied()
                .filter(|&i| i < ncols && w[i] > floors[i])
                .max_by_key(|&i| w[i]);
            match pick {
                Some(i) => {
                    w[i] -= 1;
                    total -= 1;
                }
                None => break,
            }
        }
    }

    let table_w: usize = w.iter().sum::<usize>() + seps;
    let wref = &w;

    let emit = |cells: &[String]| {
        let mut line = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i >= ncols {
                break;
            }
            if i > 0 {
                line.push(' ');
            }
            line.push_str(&fit(c, wref[i]));
        }
        println!("{}", line.trim_end());
    };

    let hrow: Vec<String> = headers.iter().map(|s| s.to_string()).collect();
    emit(&hrow);
    println!("{}", "-".repeat(table_w));
    for r in rows {
        emit(r);
    }
}

/// Truncate `s` to `width` display columns (appending `…`) or left-pad when shorter.
fn fit(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width {
        format!("{:<width$}", s)
    } else if width == 0 {
        String::new()
    } else {
        let kept: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

fn term_width() -> Option<usize> {
    if let Ok(v) = std::env::var("COLUMNS") {
        if let Ok(n) = v.parse::<usize>() {
            if n > 0 {
                return Some(n);
            }
        }
    }
    terminal_size::terminal_size().map(|(terminal_size::Width(w), _)| w as usize)
}

// ---- requires / bump -------------------------------------------------------

/// `cpm rm <name> [version]` — remove a dep from the global pantry.
///
/// The inverse of `cpm add`: drops the pantry entry (all versions, or one with
/// `--version`) and deletes its archive from `$CPM_PRELOAD`. Re-add anytime.
pub fn rm(name: &str, version: Option<&str>, dry_run: bool) -> Result<()> {
    let mut reg = deps::load()?;
    let key = version.map(deps::version_key);
    let matched: Vec<&Dep> = reg
        .deps
        .iter()
        .filter(|d| {
            d.name.eq_ignore_ascii_case(name)
                && key
                    .map(|k| d.version.as_deref().map(deps::version_key).as_ref() == Some(&k))
                    .unwrap_or(true)
        })
        .collect();
    if matched.is_empty() {
        bail!(
            "no pantry entry for '{name}'{}",
            version.map(|v| format!(" @ {v}")).unwrap_or_default()
        );
    }

    let verb = if dry_run { "[dry-run] would remove" } else { "removing" };
    for d in &matched {
        println!(
            "{verb} {} {} ({})",
            d.name,
            d.version.clone().unwrap_or_else(|| "-".into()),
            d.archive
        );
    }
    if dry_run {
        return Ok(());
    }

    let removed = reg.remove(name, version);
    deps::save(&reg)?;

    if let Some(p) = config::preload_dir_opt() {
        for d in &removed {
            let path = p.join(&d.archive);
            if path.exists() {
                if let Err(e) = fs::remove_file(&path) {
                    eprintln!("warn: could not delete {}: {e}", path.display());
                }
            }
        }
    }
    Ok(())
}

// ---- requires / bump (project deps.toml) -----------------------------------

/// Commented field-hint block rendered above a freshly created `[dep.X]` header.
/// Attached as table-header prefix decor (not trailing) so it moves atomically
/// with the table and never migrates when further stanzas are appended.
fn hint_for(name: &str) -> String {
    format!(
        "\n# {name} — optional overrides (uncomment a line inside the table to enable):\n\
         #   options       = [\"OPT=ON\", \"...\"]   # extra CPMAddPackage args\n\
         #   source_subdir = \"libs/foo\"           # nested CMakeLists in the archive\n\
         #   download_only = false                  # fetch source, skip add_subdirectory; declare targets in `post`\n\
         #   pre  = \"...\"                          # cmake snippet OR a `.cmake` file ref (before add_subdirectory)\n\
         #   post = \"...\"                          # (after)\n\
         #   patches = [\"name.patch\"]              # applied via CPM_PATCH_COMMAND\n\
         #   aliases = {{ \"pkg::x\" = \"X\" }}\n"
    )
}

/// Surgically set `version` in a table, preserving any existing decor (so the
/// commented hint block and user comments on the version line survive a bump).
fn table_set_version(t: &mut toml_edit::Table, value: String) {
    let prev_decor = t
        .get("version")
        .and_then(|i| i.as_value())
        .map(|v| v.decor().clone());
    t["version"] = toml_edit::value(value);
    if let Some(d) = prev_decor {
        if let Some(v) = t["version"].as_value_mut() {
            *v.decor_mut() = d;
        }
    }
}

fn ensure_dep_table<'a>(doc: &'a mut toml_edit::DocumentMut) -> &'a mut toml_edit::Table {
    let root = doc.as_table_mut();
    if !root.contains_key("dep") {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        root["dep"] = toml_edit::Item::Table(t);
    }
    root["dep"].as_table_mut().expect("dep is a table")
}

fn deps_toml_from_cwd() -> Result<(std::path::PathBuf, std::path::PathBuf, String)> {
    let (root, rel) = config::locate_module()?;
    Ok((root.clone(), root.join(&rel).join("deps.toml"), rel))
}

/// `cpm requires <name> [<spec>]` — add a dep to the project manifest.
pub fn requires_add(name: &str, spec_str: Option<&str>, package: Option<&str>) -> Result<()> {
    let pantry = deps::load()?;
    let parsed = spec_str.map(spec::Spec::parse).transpose()?;
    let base = pantry.resolve_dep(name, parsed.as_ref()).ok_or_else(|| {
        anyhow::anyhow!(
            "no version of '{name}' satisfies `{}`; {}",
            spec_str.unwrap_or("(freshest)"),
            avail(&pantry, name)
        )
    })?;
    // what to write into deps.toml: constraints stay as ranges (re-resolved at
    // generate); exact/any collapse to the resolved concrete version.
    let to_write = match &parsed {
        Some(spec::Spec::Constraint(_)) => spec_str.unwrap().to_string(),
        _ => base.version.clone().unwrap_or_else(|| "0".into()),
    };

    let (root, dpath, rel) = deps_toml_from_cwd()?;
    let txt = fs::read_to_string(&dpath)
        .with_context(|| format!("read {} (run `cpm init` first)", dpath.display()))?;
    let mut doc: toml_edit::DocumentMut =
        txt.parse().with_context(|| format!("parse {}", dpath.display()))?;

    let dep = ensure_dep_table(&mut doc);
    let existed = dep.contains_key(name);
    if !existed {
        // create the table first, then decorate its header prefix with hints.
        dep[name] = toml_edit::table();
        let newtbl = dep[name].as_table_mut().expect("dep entry is a table");
        newtbl.decor_mut().set_prefix(&hint_for(name));
        let pkg = package.unwrap_or(name).to_string();
        newtbl["package"] = toml_edit::value(pkg);
        newtbl["version"] = toml_edit::value(to_write.clone());
    } else {
        let t = dep[name].as_table_mut().expect("dep entry is a table");
        table_set_version(t, to_write.clone());
    }

    fs::write(&dpath, doc.to_string())?;
    gen::generate(&root.to_string_lossy(), &rel)?;

    println!(
        "{name}: required {} → resolved {}",
        spec_str.unwrap_or("(freshest)"),
        base.version.clone().unwrap_or_else(|| "?".into())
    );
    Ok(())
}

/// `cpm requires --rm <name>` — drop a dep from the project manifest.
pub fn requires_rm(name: &str) -> Result<()> {
    let (root, dpath, rel) = deps_toml_from_cwd()?;
    let txt = fs::read_to_string(&dpath).with_context(|| format!("read {}", dpath.display()))?;
    let mut doc: toml_edit::DocumentMut =
        txt.parse().with_context(|| format!("parse {}", dpath.display()))?;

    let mut removed = false;
    if let Some(dep) = doc.get_mut("dep").and_then(|i| i.as_table_mut()) {
        if dep.remove(name).is_some() {
            removed = true;
        }
    }
    if let Some(arr) = doc
        .get_mut("deps")
        .and_then(|i| i.as_value_mut())
        .and_then(|v| v.as_array_mut())
    {
        let mut i = 0;
        while i < arr.len() {
            if arr.get(i).and_then(|v| v.as_str()) == Some(name) {
                arr.remove(i);
                removed = true;
            } else {
                i += 1;
            }
        }
    }
    if !removed {
        bail!("'{name}' is not required in this project");
    }

    fs::write(&dpath, doc.to_string())?;
    gen::generate(&root.to_string_lossy(), &rel)?;
    println!("removed {name} (regenerated glue)");
    Ok(())
}

/// `cpm requires list [--outdated]` — show what the project requires.
pub fn requires_list(outdated: bool) -> Result<()> {
    let (_root, dpath, _rel) = deps_toml_from_cwd()?;
    let reqs = gen::read_required(&dpath)?;
    if reqs.is_empty() {
        println!("(no deps required yet — `cpm requires add <name> [<spec>]`)");
        return Ok(());
    }
    let pantry = deps::load()?;

    // name, spec, resolved, freshest, package, source, is_outdated
    let mut enriched: Vec<(String, String, String, Option<String>, String, String, bool)> =
        Vec::new();
    for r in &reqs {
        let parsed = r.version.as_deref().map(spec::Spec::parse).transpose()?;
        let base = pantry.resolve_dep(&r.key, parsed.as_ref());
        let resolved = base
            .and_then(|b| b.version.clone())
            .unwrap_or_else(|| "-".into());
        let freshest = pantry.version_strings(&r.key).into_iter().next();
        let is_outdated = resolved != "-"
            && freshest
                .as_deref()
                .map(|f| deps::version_key(&resolved) < deps::version_key(f))
                .unwrap_or(false);
        let pkg = r.package.clone().unwrap_or_else(|| r.key.clone());
        let src = match (&base, r.from_stanza) {
            (None, _) => "missing",
            (Some(_), true) => "stanza",
            (Some(_), false) => "shorthand",
        }
        .to_string();
        let spec_disp = r.version.clone().unwrap_or_else(|| "(freshest)".into());
        enriched.push((r.key.clone(), spec_disp, resolved, freshest, pkg, src, is_outdated));
    }

    if outdated {
        let picked: Vec<_> = enriched.into_iter().filter(|x| x.6).collect();
        if picked.is_empty() {
            println!("all dependencies are at their freshest available version");
            return Ok(());
        }
        let headers = ["NAME", "RESOLVED", "FRESHEST", "SPEC", "PACKAGE"];
        let rows: Vec<Vec<String>> = picked
            .iter()
            .map(|(n, spec, res, fresh, pkg, _, _)| {
                vec![
                    n.clone(),
                    res.clone(),
                    fresh.clone().unwrap_or_else(|| "-".into()),
                    spec.clone(),
                    pkg.clone(),
                ]
            })
            .collect();
        print_table(&headers, &rows, &[0, 3, 4]);
    } else {
        let headers = ["NAME", "SPEC", "RESOLVED", "PACKAGE", "SOURCE"];
        let rows: Vec<Vec<String>> = enriched
            .iter()
            .map(|(n, spec, res, _, pkg, src, _)| {
                vec![n.clone(), spec.clone(), res.clone(), pkg.clone(), src.clone()]
            })
            .collect();
        print_table(&headers, &rows, &[0, 2, 3]);
    }
    Ok(())
}

/// `cpm bump <name> [<spec>]` — change ONLY the version, preserving user settings.
pub fn bump(name: &str, spec_str: Option<&str>) -> Result<()> {
    let (root, dpath, rel) = deps_toml_from_cwd()?;
    let txt = fs::read_to_string(&dpath).with_context(|| format!("read {}", dpath.display()))?;
    let mut doc: toml_edit::DocumentMut =
        txt.parse().with_context(|| format!("parse {}", dpath.display()))?;

    let has = doc
        .get("dep")
        .and_then(|d| d.as_table())
        .map(|d| d.contains_key(name))
        .unwrap_or(false);
    if !has {
        bail!("'{name}' is not required in this project; use `cpm requires {name} [<spec>]`");
    }

    let current = doc["dep"][name]
        .as_table()
        .and_then(|t| t.get("version"))
        .and_then(|i| i.as_value())
        .and_then(|v| v.as_str())
        .map(String::from);
    let cur_spec = current.as_deref().map(spec::Spec::parse).transpose()?;

    let pantry = deps::load()?;

    // No new spec given:
    //   - if current is a constraint → it already auto-resolves at generate;
    //     just report the freshest match, change nothing.
    //   - else (exact/none) → pin to the freshest available.
    let (to_write, resolved): (Option<String>, String) = match (&cur_spec, spec_str) {
        (Some(spec::Spec::Constraint(_)), None) => {
            let base = pantry
                .resolve_dep(name, cur_spec.as_ref())
                .ok_or_else(|| anyhow::anyhow!("no version of '{name}' satisfies `{}`", current.as_deref().unwrap_or("")))?;
            (None, base.version.clone().unwrap_or_else(|| "?".into()))
        }
        _ => {
            let parsed = spec_str.map(spec::Spec::parse).transpose()?;
            let base = pantry.resolve_dep(name, parsed.as_ref()).ok_or_else(|| {
                anyhow::anyhow!(
                    "no version of '{name}' satisfies `{}`; {}",
                    spec_str.unwrap_or("(freshest)"),
                    avail(&pantry, name)
                )
            })?;
            let w = match &parsed {
                Some(spec::Spec::Constraint(_)) => spec_str.unwrap().to_string(),
                _ => base.version.clone().unwrap_or_else(|| "0".into()),
            };
            (Some(w), base.version.clone().unwrap_or_else(|| "?".into()))
        }
    };

    match to_write {
        None => {
            println!("{name}: constrained to `{}` → resolves to {resolved}", current.as_deref().unwrap_or(""));
            Ok(())
        }
        Some(w) => {
            if Some(&w) == current.as_ref() {
                println!("{name}: already at {w}");
                return Ok(());
            }
            let t = doc["dep"][name].as_table_mut().expect("dep entry is a table");
            table_set_version(t, w.clone());
            fs::write(&dpath, doc.to_string())?;
            gen::generate(&root.to_string_lossy(), &rel)?;
            println!(
                "{name}: {} → {resolved}",
                current.as_deref().unwrap_or("-")
            );
            Ok(())
        }
    }
}

fn avail(reg: &deps::Registry, name: &str) -> String {
    let v = reg.version_strings(name);
    if v.is_empty() {
        format!("not in pantry — `cpm add {name} <url> <tag>`")
    } else {
        format!("available: {}", v.join(", "))
    }
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
    println!("  URL \"${{CPM_PRELOAD}}/{}\"", dep.archive);
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

// ---- info ------------------------------------------------------------------

/// Short summary of a dependency: every archived version and, for each, its
/// load sources (the git upstream and the local archive). The `git remote -v`
/// equivalent.
pub fn info(name: &str) -> Result<()> {
    let reg = deps::load()?;
    let all = reg.versions(name);
    if all.is_empty() {
        bail!("unknown dep '{name}'");
    }
    let preload = config::preload_dir_opt();
    let canon = all[0].name.clone();
    let n = all.len();

    println!("{canon} ({n} version{})\n", if n == 1 { "" } else { "s" });

    for dep in &all {
        let version = dep.version.clone().unwrap_or_else(|| "?".into());
        println!("  {version}");

        match (dep.url.as_deref(), dep.tag.as_deref()) {
            (Some(u), Some(t)) => println!("    git  {u} @ {t}"),
            _ => println!(
                "    git  (none — `cpm source add {canon} <url> <tag> --version {version}`)"
            ),
        }

        let (size, status) = match &preload {
            Some(p) => {
                let path = p.join(&dep.archive);
                if path.exists() {
                    let sz = fs::metadata(&path).map(|m| human_bytes(m.len())).unwrap_or_default();
                    (sz, "present".to_string())
                } else {
                    ("-".into(), "MISSING".to_string())
                }
            }
            None => ("?".into(), "no CPM_PRELOAD".to_string()),
        };
        let sha_short: String = dep
            .sha256
            .as_deref()
            .map(|s| s.chars().take(12).collect())
            .unwrap_or_else(|| "no hash".into());
        println!(
            "    loc  {}  {}  sha {}  [{}]",
            dep.archive, size, sha_short, status
        );
        if dep.submodules == Some(true) {
            println!("         (source tree uses git submodules)");
        }
        println!();
    }
    Ok(())
}

// ---- source add / rm -------------------------------------------------------

/// Attach a git source (url+tag) to an existing pantry entry. Never fetches —
/// the archive must already be on disk. Errors if the entry or archive is
/// missing (use `cpm add` to fetch a new dep first).
pub fn source_add(
    name: &str,
    url: &str,
    tag: &str,
    version_override: Option<&str>,
) -> Result<()> {
    let preload = config::preload_dir()?;
    let mut reg = deps::load()?;

    let version = resolve_version(version_override, tag)?;
    let key = deps::version_key(&version);

    let archive = match reg.deps.iter().find(|d| {
        d.name.eq_ignore_ascii_case(name)
            && d.version.as_deref().map(deps::version_key).as_ref() == Some(&key)
    }) {
        Some(d) => d.archive.clone(),
        None => bail!(
            "no pantry entry for {name} {version}.\n\
             fetch it first: cpm add {name} {url} {tag}"
        ),
    };
    let archive_path = preload.join(&archive);
    if !archive_path.exists() {
        bail!(
            "archive `{archive}` missing in {} — re-fetch: cpm add {name} {url} {tag} --force",
            preload.display()
        );
    }
    let sha = archive::sha256_file(&archive_path)?;
    let size = fs::metadata(&archive_path)?.len();

    attach_in_place(&mut reg, name, key, url, tag, &sha);
    deps::save(&reg)?;

    println!("source added: {} ({})", archive, human_bytes(size));
    println!("  {} @ {}", url, tag);
    Ok(())
}

/// Remove the git source (url+tag) from an entry, leaving it loc-only.
pub fn source_rm(name: &str, version_override: Option<&str>) -> Result<()> {
    let mut reg = deps::load()?;
    let all = reg.versions(name);
    if all.is_empty() {
        bail!("unknown dep '{name}'");
    }

    let key = match version_override {
        Some(v) => {
            let c = canonicalize_version(v).ok_or_else(|| {
                anyhow::anyhow!("`--version {v}` is not canonical MAJOR.MINOR.PATCH")
            })?;
            deps::version_key(&c)
        }
        None => {
            if all.len() == 1 {
                deps::version_key(all[0].version.as_deref().unwrap_or("0"))
            } else {
                let vers: Vec<_> = all
                    .iter()
                    .map(|d| d.version.clone().unwrap_or_default())
                    .collect();
                bail!(
                    "{name} has {} versions ({}); pass --version",
                    all.len(),
                    vers.join(", ")
                );
            }
        }
    };

    let idx = slot_of(&reg, name, key).ok_or_else(|| anyhow::anyhow!("no entry for {name}"))?;
    let dep = &mut reg.deps[idx];
    if dep.url.is_none() && dep.tag.is_none() {
        println!(
            "{} {} already has no source (loc-only)",
            dep.name,
            dep.version.as_deref().unwrap_or("?")
        );
        return Ok(());
    }
    dep.url = None;
    dep.tag = None;
    deps::save(&reg)?;
    println!("source removed (now loc-only)");
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

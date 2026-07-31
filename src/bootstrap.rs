//! Download official CPM.cmake (+ generate get_cpm.cmake) into the tool's
//! own data dir (NOT into $CPM_PRELOAD).

use std::path::Path;

use anyhow::{Context, Result};

use crate::config;

const GET_CPM_TEMPLATE: &str = include_str!("get_cpm.cmake.tmpl");

pub fn latest_version() -> Result<String> {
    let resp = ureq::get("https://api.github.com/repos/cpm-cmake/CPM.cmake/releases/latest")
        .set("User-Agent", "cpm-cli")
        .call()?;
    let v: serde_json::Value = serde_json::from_str(&resp.into_string()?)?;
    let tag = v["tag_name"]
        .as_str()
        .context("missing tag_name in github response")?;
    Ok(tag.trim_start_matches('v').to_string())
}

pub fn resolve_version(version: Option<&str>, latest: bool) -> Result<String> {
    if latest {
        return latest_version();
    }
    if let Some(v) = version {
        return Ok(v.trim_start_matches('v').to_string());
    }
    let cfg = config::load_config();
    if let Some(v) = cfg.cpm.version {
        return Ok(v);
    }
    Ok(config::DEFAULT_CPM_VERSION.to_string())
}

pub fn run(version: Option<&str>, latest: bool) -> Result<()> {
    let ver = resolve_version(version, latest)?;
    let dir = config::cpm_cmake_dir()?.join(&ver);
    std::fs::create_dir_all(&dir)?;

    let cmake_path = dir.join("CPM.cmake");
    let url = format!(
        "https://github.com/cpm-cmake/CPM.cmake/releases/download/v{ver}/CPM.cmake"
    );
    println!("downloading CPM.cmake v{ver} ...");
    download(&url, &cmake_path)?;

    let hash = crate::archive::sha256_file(&cmake_path)?;
    let get = GET_CPM_TEMPLATE.replace("__VER__", &ver).replace("__HASH__", &hash);
    std::fs::write(dir.join("get_cpm.cmake"), get)?;

    // remember the active version
    let mut reg = crate::deps::load()?;
    reg.meta.cpm_version = Some(ver.clone());
    crate::deps::save(&reg)?;

    println!("CPM.cmake v{ver} installed at {}", dir.display());
    println!("  CPM.cmake     {}", cmake_path.display());
    println!("  get_cpm.cmake {}", dir.join("get_cpm.cmake").display());
    Ok(())
}

fn download(url: &str, dst: &Path) -> Result<()> {
    let resp = ureq::get(url).set("User-Agent", "cpm-cli").call()?;
    let mut file = std::fs::File::create(dst)?;
    let mut reader = resp.into_reader();
    std::io::copy(&mut reader, &mut file)?;
    Ok(())
}

/// `cpm update` — refresh the vendored CPM.cmake to the latest stable release.
///
/// Default (no flag): project-local. Requires a `.cpm` at the project root;
/// downloads CPM.cmake into `[paths] scripts` (as `CPM.cmake`).
///
/// `-g`/`--global`: bump the version pinned in this tool's own source tree
/// (`src/get_cpm_default.cmake` + `DEFAULT_CPM_VERSION` in `src/config.rs`).
pub fn update(global: bool, check: bool) -> Result<()> {
    let latest = latest_version()?;
    if global {
        update_self(&latest, check)
    } else {
        update_project(&latest, check)
    }
}

/// `-g`: bump the version baked into the tool's own source tree.
fn update_self(latest: &str, check: bool) -> Result<()> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let default_cmake = repo.join("src").join("get_cpm_default.cmake");
    let config_rs = repo.join("src").join("config.rs");

    let d_src = std::fs::read_to_string(&default_cmake).with_context(|| {
        format!(
            "cpm source not found under {} — run `cpm update -g` from a checkout",
            repo.display()
        )
    })?;
    let current = extract_marker(&d_src, "set(CPM_DOWNLOAD_VERSION ", ')')?;
    let old_hash = extract_marker(&d_src, "set(CPM_HASH_SUM \"", '"')?;

    if check {
        println!("bundled : v{current}");
        println!("latest  : v{latest}");
        if latest == current {
            println!("up to date");
        } else {
            println!("update available: {current} -> {latest}");
        }
        return Ok(());
    }

    if latest == current {
        println!("Already at the latest stable release: CPM.cmake v{current}");
        return Ok(());
    }

    let hash = download_cmake_to_tmp(latest)?;
    let d_new = d_src
        .replace(
            &format!("set(CPM_DOWNLOAD_VERSION {current})"),
            &format!("set(CPM_DOWNLOAD_VERSION {latest})"),
        )
        .replace(
            &format!("set(CPM_HASH_SUM \"{old_hash}\")"),
            &format!("set(CPM_HASH_SUM \"{hash}\")"),
        );
    std::fs::write(&default_cmake, d_new)?;

    let c = std::fs::read_to_string(&config_rs)?;
    let c_new = c.replace(
        &format!("pub const DEFAULT_CPM_VERSION: &str = \"{current}\";"),
        &format!("pub const DEFAULT_CPM_VERSION: &str = \"{latest}\";"),
    );
    std::fs::write(&config_rs, c_new)?;

    println!("CPM.cmake {current} -> {latest} (sha256 {hash})");
    println!("  updated {}", default_cmake.display());
    println!("  updated {}", config_rs.display());
    println!("rebuild (`cargo build --release`) and commit to finalize.");
    Ok(())
}

/// Default: refresh `<project>/[paths] scripts/CPM.cmake`.
fn update_project(latest: &str, check: bool) -> Result<()> {
    let root = config::find_project_root()?;
    let cfg = config::load_project_config(&root)?;
    let scripts_rel = cfg
        .paths
        .scripts
        .as_deref()
        .context("`.cpm` has no [paths] scripts")?;
    let scripts = root.join(scripts_rel);
    let cpm_cmake = scripts.join("CPM.cmake");

    let current = std::fs::read_to_string(&cpm_cmake)
        .ok()
        .and_then(|s| detect_cpm_version(&s))
        .unwrap_or_else(|| "unknown".to_string());

    println!("project : {}", root.display());

    if check {
        println!("scripts : {}", cpm_cmake.display());
        println!("current : v{current}");
        println!("latest  : v{latest}");
        if latest == current {
            println!("up to date");
        } else {
            println!("update available: {current} -> {latest}");
        }
        return Ok(());
    }

    if latest == current {
        println!("Already at the latest stable release: CPM.cmake v{current}");
        println!("  {}", cpm_cmake.display());
        return Ok(());
    }

    std::fs::create_dir_all(&scripts)?;
    let url = format!(
        "https://github.com/cpm-cmake/CPM.cmake/releases/download/v{latest}/CPM.cmake"
    );
    println!("downloading {url}");
    download(&url, &cpm_cmake)?;
    let hash = crate::archive::sha256_file(&cpm_cmake)?;

    println!("CPM.cmake {current} -> {latest} (sha256 {hash})");
    println!("  written {}", cpm_cmake.display());
    println!("commit to finalize.");
    Ok(())
}

/// Download CPM.cmake for `version` into a temp file, return its sha256.
fn download_cmake_to_tmp(version: &str) -> Result<String> {
    let tmp = config::tmp_dir()?;
    std::fs::create_dir_all(&tmp)?;
    let dst = tmp.join(format!("CPM_{version}.cmake"));
    let url = format!(
        "https://github.com/cpm-cmake/CPM.cmake/releases/download/v{version}/CPM.cmake"
    );
    println!("downloading CPM.cmake v{version} ...");
    download(&url, &dst)?;
    let hash = crate::archive::sha256_file(&dst)?;
    let _ = std::fs::remove_file(&dst);
    Ok(hash)
}

/// Detect the release version baked into a CPM.cmake
/// (`set(CURRENT_CPM_VERSION X.Y.Z)`), skipping the EXTRACTED_CPM_VERSION form.
fn detect_cpm_version(content: &str) -> Option<String> {
    let marker = "set(CURRENT_CPM_VERSION ";
    let mut start = 0;
    while let Some(rel) = content[start..].find(marker) {
        let i = start + rel + marker.len();
        let after = &content[i..];
        let end = match after.find(|c: char| c == ')' || c.is_whitespace()) {
            Some(e) => e,
            None => break,
        };
        let v = &after[..end];
        if v.chars().next().map_or(false, |c| c.is_ascii_digit()) {
            return Some(v.to_string());
        }
        start = i;
    }
    None
}

/// Text between `marker` and the next `terminator` in `content`.
fn extract_marker(content: &str, marker: &str, terminator: char) -> Result<String> {
    let i = content
        .find(marker)
        .with_context(|| format!("marker `{marker}` not found"))?
        + marker.len();
    let j = content[i..]
        .find(terminator)
        .with_context(|| format!("terminator `{terminator}` not found after `{marker}`"))?
        + i;
    Ok(content[i..j].trim().to_string())
}

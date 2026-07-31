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

/// Rewrite the CPM.cmake version pinned in the tool's own source tree
/// (`src/get_cpm_default.cmake` + `DEFAULT_CPM_VERSION` in `src/config.rs`)
/// to the latest stable release. Run from a checkout, then rebuild.
pub fn update(check: bool) -> Result<()> {
    let latest = latest_version()?;

    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let default_cmake = repo.join("src").join("get_cpm_default.cmake");
    let config_rs = repo.join("src").join("config.rs");

    let d_src = std::fs::read_to_string(&default_cmake).with_context(|| {
        format!(
            "cpm source not found under {} — run `cpm update` from a checkout",
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

    let url = format!(
        "https://github.com/cpm-cmake/CPM.cmake/releases/download/v{latest}/CPM.cmake"
    );
    let tmp = config::tmp_dir()?;
    std::fs::create_dir_all(&tmp)?;
    let dst = tmp.join(format!("CPM_{latest}.cmake"));
    println!("downloading CPM.cmake v{latest} ...");
    download(&url, &dst)?;
    let hash = crate::archive::sha256_file(&dst)?;
    let _ = std::fs::remove_file(&dst);

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

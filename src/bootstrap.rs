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

//! Path resolution: separates archive storage ($CPM_PRELOAD) from the
//! tool's own state ($CPM_HOME, defaults to XDG data dir).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const DEFAULT_CPM_VERSION: &str = "0.43.1";

/// The tool's own data dir: deps.toml, tmp clones, bootstrapped CPM.cmake.
/// Override with $CPM_HOME, else XDG data dir (`~/.local/share/cpm`).
pub fn cpm_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CPM_HOME") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let dir = dirs::data_dir()
        .context("cannot determine XDG data dir (set $CPM_HOME explicitly)")?;
    Ok(dir.join("cpm"))
}

/// User config dir (`~/.config/cpm`). Override with $CPM_CONFIG_HOME.
pub fn config_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CPM_CONFIG_HOME") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let dir = dirs::config_dir()
        .context("cannot determine XDG config dir (set $CPM_CONFIG_HOME)")?;
    Ok(dir.join("cpm"))
}

pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn deps_file() -> Result<PathBuf> {
    Ok(cpm_home()?.join("deps.toml"))
}

pub fn tmp_dir() -> Result<PathBuf> {
    Ok(cpm_home()?.join("tmp"))
}

/// Where bootstrapped CPM.cmake / get_cpm.cmake live, per version.
pub fn cpm_cmake_dir() -> Result<PathBuf> {
    Ok(cpm_home()?.join("cpm"))
}

/// Archive storage. Resolution order: `$CPM_PRELOAD` env, then `[paths] preload`
/// in config.toml. REQUIRED for add/fetch/list/show/verify.
pub fn preload_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CPM_PRELOAD") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    if let Some(p) = load_config().paths.preload {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    anyhow::bail!(
        "CPM_PRELOAD is not set. Point it at your archives directory, e.g.:\n  \
         export CPM_PRELOAD=$HOME/cmake/cpm_preload\n\
         or set [paths] preload in {}",
        config_file()?.display()
    )
}

pub fn preload_dir_opt() -> Option<PathBuf> {
    preload_dir().ok()
}

pub fn source_cache_opt() -> Option<PathBuf> {
    let p = std::env::var("CPM_SOURCE_CACHE").ok()?;
    if p.is_empty() {
        None
    } else {
        Some(PathBuf::from(p))
    }
}

pub fn ensure_dirs() -> Result<()> {
    for d in [
        cpm_home()?,
        tmp_dir()?,
        cpm_cmake_dir()?,
        config_dir()?,
        preload_dir()?,
    ] {
        std::fs::create_dir_all(&d)?;
    }
    Ok(())
}

// ---- config.toml (user settings) -------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
pub struct Config {
    #[serde(default)]
    pub cpm: CpmCfg,
    #[serde(default)]
    pub paths: PathsCfg,
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct PathsCfg {
    /// Archive storage used when $CPM_PRELOAD env is not set.
    #[serde(default)]
    pub preload: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct CpmCfg {
    #[serde(default)]
    pub version: Option<String>,
}

pub fn load_config() -> Config {
    let Ok(path) = config_file() else {
        return Config::default();
    };
    let Ok(s) = std::fs::read_to_string(&path) else {
        return Config::default();
    };
    toml::from_str(&s).unwrap_or_default()
}

// ---- .cpm (per-repository project config) ----------------------------------

#[derive(Debug, Default, serde::Deserialize)]
pub struct ProjectConfig {
    #[serde(default)]
    pub paths: ProjectPaths,
}

#[derive(Debug, Default, serde::Deserialize)]
#[allow(dead_code)] // schema for upcoming commands (init/generate/preload)
pub struct ProjectPaths {
    /// Where the vendored CPM.cmake lives (relative to project root).
    pub scripts: Option<String>,
    /// Where deps.toml + engine + Find<Name>.cmake live.
    pub module: Option<String>,
    /// Patch root (→ CPM_PATCH_PREFIX).
    pub patches: Option<String>,
    /// Optional repo-local archive store.
    pub preload: Option<String>,
}

/// Walk up from CWD to the nearest directory containing a `.cpm` file.
pub fn find_project_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("cannot determine current directory")?;
    for dir in cwd.ancestors() {
        if dir.join(".cpm").is_file() {
            return Ok(dir.to_path_buf());
        }
    }
    anyhow::bail!(
        "no `.cpm` found here or in any parent — run from the root of a cpm project\n\
         (create a `.cpm` with at least `[paths] scripts = \"...\"`)"
    )
}

pub fn load_project_config(root: &Path) -> Result<ProjectConfig> {
    let p = root.join(".cpm");
    let s = std::fs::read_to_string(&p)
        .with_context(|| format!("reading {}", p.display()))?;
    toml::from_str(&s).with_context(|| format!("parsing {}", p.display()))
}

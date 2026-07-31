//! deps.toml registry: the list of preloaded dependencies.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dep {
    pub url: String,
    pub tag: String,
    pub archive: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    /// Whether the upstream sources use git submodules (affects tarball-tier synthesis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submodules: Option<bool>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub meta: Meta,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub deps: BTreeMap<String, Dep>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Meta {
    #[serde(rename = "cpm_version", skip_serializing_if = "Option::is_none")]
    pub cpm_version: Option<String>,
}

pub fn load() -> Result<Registry> {
    let p = config::deps_file()?;
    match std::fs::read_to_string(&p) {
        Ok(s) if s.trim().is_empty() => Ok(Registry::default()),
        Ok(s) => toml::from_str(&s).with_context(|| format!("parse {}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(e) => Err(e.into()),
    }
}

pub fn save(reg: &Registry) -> Result<()> {
    let p = config::deps_file()?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut s = String::from("# managed by `cpm`; do not edit by hand unless you know what you do\n");
    s.push_str(&toml::to_string_pretty(reg)?);
    std::fs::write(&p, s)?;
    Ok(())
}

#[allow(dead_code)]
pub fn archive_path(dep: &Dep) -> Result<PathBuf> {
    Ok(config::preload_dir()?.join(&dep.archive))
}

//! deps.toml registry: the list of preloaded dependencies.
//!
//! Multi-version: a name may have several versions archived. Lookups pick the
//! requested version (exact match) or fall back to the freshest available.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dep {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    pub archive: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    /// Whether the upstream sources use git submodules (affects tarball-tier synthesis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submodules: Option<bool>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub meta: Meta,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<Dep>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Meta {
    #[serde(rename = "cpm_version", skip_serializing_if = "Option::is_none")]
    pub cpm_version: Option<String>,
}

impl Registry {
    /// All entries for `name`, sorted freshest-first.
    pub fn versions(&self, name: &str) -> Vec<&Dep> {
        let mut v: Vec<&Dep> = self
            .deps
            .iter()
            .filter(|d| d.name.eq_ignore_ascii_case(name))
            .collect();
        v.sort_by(|a, b| {
            let va = a.version.as_deref().unwrap_or("0");
            let vb = b.version.as_deref().unwrap_or("0");
            version_key(vb).cmp(&version_key(va))
        });
        v
    }

    /// Pick a single entry: exact `version` if given (no freshest fallback — a
    /// missing pinned version must NOT silently serve another), else freshest.
    pub fn pick(&self, name: &str, version: Option<&str>) -> Option<&Dep> {
        let all = self.versions(name);
        if all.is_empty() {
            return None;
        }
        match version {
            Some(want) => all.iter().find(|d| d.version.as_deref() == Some(want)).copied(),
            None => Some(all[0]),
        }
    }

    /// Insert or replace by (name, version).
    pub fn upsert(&mut self, dep: Dep) {
        let name = dep.name.clone();
        let ver = dep.version.clone();
        if let Some(slot) = self
            .deps
            .iter_mut()
            .find(|d| d.name.eq_ignore_ascii_case(&name) && d.version == ver)
        {
            *slot = dep;
        } else {
            self.deps.push(dep);
        }
    }
}

/// Numeric key for ordering versions (first three numeric components, 0-padded).
pub fn version_key(v: &str) -> (u64, u64, u64) {
    let mut parts = [0u64; 3];
    for (i, tok) in v.split(|c: char| !c.is_ascii_digit()).enumerate() {
        if i >= 3 {
            break;
        }
        if let Ok(n) = tok.parse::<u64>() {
            parts[i] = n;
        }
    }
    (parts[0], parts[1], parts[2])
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

//! Version requirement spec for `requires`/`bump`.
//!
//! A spec is one of:
//!   - `Any`        — `*`, `x`, `latest`, or empty → freshest in the pantry.
//!   - `Exact(v)`   — a bare version like `1.90.0` → pinned (matched by numeric
//!                    `version_key`, so `1.90` ≡ `1.90.0`).
//!   - `Constraint` — anything with a leading operator (`^`, `~`, `>=`, `<`,
//!                     `=`, …) → a `semver::VersionReq`, resolved at generate
//!                     time to the freshest pantry version that matches.
//!
//! Bare versions are intentionally treated as exact pins (not caret, unlike
//! cargo), so an explicit concrete version stays concrete and reproducible.

use anyhow::{anyhow, Result};
use semver::{Version, VersionReq};

use crate::deps::version_key;

#[derive(Debug, Clone)]
pub enum Spec {
    Any,
    Exact(String),
    Constraint(VersionReq),
}

impl Spec {
    pub fn parse(s: &str) -> Result<Self> {
        let t = s.trim();
        if t.is_empty() || t == "*" || t.eq_ignore_ascii_case("x") || t.eq_ignore_ascii_case("latest") {
            return Ok(Spec::Any);
        }
        match t.chars().next() {
            Some(c @ ('^' | '~' | '>' | '<' | '=')) => {
                let _ = c;
                // Accept both spaces and commas as separators (npm-style ">=1.2 <2.0"
                // and cargo-style ">=1.2,<2.0"); normalise to comma-separated.
                let norm = t
                    .replace(',', " ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(",");
                VersionReq::parse(&norm)
                    .map(Spec::Constraint)
                    .map_err(|e| anyhow!("invalid version spec `{t}`: {e}"))
            }
            _ => Ok(Spec::Exact(t.to_string())),
        }
    }

    /// Human-readable form for echoing back to the user.
    pub fn display(&self) -> String {
        match self {
            Spec::Any => "(freshest)".into(),
            Spec::Exact(v) => v.clone(),
            Spec::Constraint(r) => r.to_string(),
        }
    }
}

/// Resolve a spec against a list of available versions (freshest-first).
/// Returns the chosen version string.
pub fn resolve<'a, I>(spec: &Spec, versions: I) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    match spec {
        Spec::Any => versions.into_iter().next().map(String::from),
        Spec::Exact(want) => {
            let key = version_key(want);
            versions
                .into_iter()
                .find(|v| version_key(v) == key)
                .map(String::from)
        }
        Spec::Constraint(req) => versions
            .into_iter()
            .filter_map(|v| Version::parse(v).ok().map(|vv| (v, vv)))
            .find(|(_, vv)| req.matches(vv))
            .map(|(v, _)| String::from(v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vs() -> Vec<&'static str> {
        // freshest-first, as the pantry yields
        vec!["2.0.0", "1.91.0", "1.90.1", "1.90.0", "1.85.0", "0.9.0"]
    }

    #[test]
    fn any_picks_freshest() {
        assert_eq!(resolve(&Spec::parse("").unwrap(), vs()).as_deref(), Some("2.0.0"));
        assert_eq!(resolve(&Spec::parse("*").unwrap(), vs()).as_deref(), Some("2.0.0"));
        assert_eq!(resolve(&Spec::parse("latest").unwrap(), vs()).as_deref(), Some("2.0.0"));
    }

    #[test]
    fn bare_is_exact_pin() {
        assert_eq!(resolve(&Spec::parse("1.90.0").unwrap(), vs()).as_deref(), Some("1.90.0"));
        // numeric-key equivalence: "1.90" ≡ "1.90.0"
        assert_eq!(resolve(&Spec::parse("1.90").unwrap(), vs()).as_deref(), Some("1.90.0"));
        // exact miss
        assert_eq!(resolve(&Spec::parse("1.99.0").unwrap(), vs()), None);
    }

    #[test]
    fn constraints_resolve_freshest_matching() {
        assert_eq!(resolve(&Spec::parse("^1.85").unwrap(), vs()).as_deref(), Some("1.91.0"));
        assert_eq!(resolve(&Spec::parse("~1.90.0").unwrap(), vs()).as_deref(), Some("1.90.1"));
        assert_eq!(resolve(&Spec::parse(">=1.85 <2.0").unwrap(), vs()).as_deref(), Some("1.91.0"));
        // `<=1.90` (no patch) covers all 1.90.x — so 1.90.1 wins:
        assert_eq!(resolve(&Spec::parse(">1.85 <=1.90").unwrap(), vs()).as_deref(), Some("1.90.1"));
        // pin the patch to get a true (..,1.90.0] upper bound:
        assert_eq!(resolve(&Spec::parse(">1.85.0 <=1.90.0").unwrap(), vs()).as_deref(), Some("1.90.0"));
        assert_eq!(resolve(&Spec::parse("<1.0").unwrap(), vs()).as_deref(), Some("0.9.0"));
    }

    #[test]
    fn constraint_miss() {
        assert_eq!(resolve(&Spec::parse("^5.0").unwrap(), vs()), None);
        assert_eq!(resolve(&Spec::parse(">=3.0").unwrap(), vs()), None);
    }
}

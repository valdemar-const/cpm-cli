//! Project-side generation: `cpm init` + `cpm generate`.
//!
//! Layered config: the global pantry ($CPM_HOME/deps.toml) is a *base* layer
//! (where to fetch sources, archive names, hashes); the per-repo `deps.toml`
//! overlays on top and wins ("the more local, the higher priority").
//!
//! `cpm generate` merges the layers and emits, per dependency:
//!   - a `Find<Package>.cmake` that triggers resolution at find_package() time;
//! and rewrites `3rdparty.cmake` (engine + `cpm_declare_package`/`cpm_declare_fallback`
//! registrations synthesised from the merged data).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::deps;
use crate::spec;

const ENGINE_CMAKE: &str = include_str!("engine.cmake");
const GET_CPM_DEFAULT_CMAKE: &str = include_str!("get_cpm_default.cmake");

const DEFAULT_NETWORK_WHEN: &str = "CPM_FETCH";
const DEFAULT_PATCH_PREFIX: &str = "${PROJECT_SOURCE_DIR}/build/patches";

// =============================== manifest ==================================

#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct Manifest {
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    network_when: Option<String>,
    #[serde(default)]
    patch_prefix: Option<String>,
    /// shorthand: pantry keys to include as-is.
    #[serde(default)]
    deps: Vec<String>,
    /// per-dep overrides / declarations.
    #[serde(default)]
    dep: BTreeMap<String, DepSpec>,
}

#[derive(serde::Deserialize, Default, Clone)]
struct DepSpec {
    package: Option<String>,
    version: Option<String>,
    archive: Option<String>,
    /// false => no git tier; "url" => override git url.
    git: Option<GitField>,
    tag: Option<String>,
    /// explicit tarball tier URL (disables github auto-synthesis).
    tarball: Option<String>,
    source_subdir: Option<String>,
    /// extra CPMAddPackage args, e.g. ["EXCLUDE_FROM_ALL YES", "DOWNLOAD_ONLY"].
    options: Option<Vec<String>>,
    patches: Option<Vec<String>>,
    /// alias -> underlying target, e.g. { "glfw::glfw" = "glfw" }.
    aliases: Option<BTreeMap<String, String>>,
    pre: Option<String>,
    post: Option<String>,
    /// synthetic, target-only package (no source tiers).
    no_source: Option<bool>,
    /// download-only: fetch the source but don't build it; you declare the
    /// target(s) yourself in `post` (header-only, non-CMake, custom builds).
    download_only: Option<bool>,
    /// explicit ordered candidate list (replaces synthesised tiers).
    source: Option<Vec<Candidate>>,
}

#[derive(serde::Deserialize, Clone)]
#[serde(untagged)]
enum GitField {
    Bool(bool),
    Url(String),
}

#[derive(serde::Deserialize, Default, Clone)]
#[allow(dead_code)]
struct Candidate {
    tier: String,
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    git: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    archive: Option<String>,
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    options: Option<Vec<String>>,
}

// ============================== tokenisation ================================
//
// Option strings are tokenised shell-style (whitespace separation, double
// quotes respected) so values like  OPTIONS "BUILD_SHARED_LIBS NO"  survive as
// a single CPM argument.

fn tokenize(s: &str) -> Vec<String> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    for c in s.chars() {
        match c {
            '"' => in_q = !in_q,
            c if c.is_whitespace() && !in_q => {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        toks.push(cur);
    }
    toks
}

/// Render a token for a generated CMake call; quote if it has whitespace/specials.
fn emit_tok(t: &str) -> String {
    let special = t.is_empty()
        || t.chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '(' | ')' | '#' | ';'));
    if special {
        let escaped = t.replace('\\', r"\\").replace('"', r#"\""#);
        format!("\"{escaped}\"")
    } else {
        t.to_string()
    }
}

fn emit_tok_list(out: &mut String, toks: &[String]) {
    for t in toks {
        out.push(' ');
        out.push_str(&emit_tok(t));
    }
}

// =============================== commands ==================================

fn module_dir(project: &Path, rel: &str) -> std::path::PathBuf {
    project.join(rel)
}

/// A project's required dependency, read from deps.toml (for `requires --list`).
pub struct RequiredDep {
    pub key: String,
    /// version spec string as written (exact pin, constraint, or None=freshest).
    pub version: Option<String>,
    pub package: Option<String>,
    /// true if a `[dep.X]` stanza exists; false if the name is only in `deps[]`.
    pub from_stanza: bool,
}

/// Parse a project's deps.toml into the ordered union of required deps.
pub fn read_required(path: &Path) -> Result<Vec<RequiredDep>> {
    let txt = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let manifest: Manifest = toml::from_str(&txt).context("parse deps.toml")?;
    let mut out: Vec<RequiredDep> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for k in &manifest.deps {
        if seen.insert(k.clone()) {
            out.push(RequiredDep {
                key: k.clone(),
                version: None,
                package: None,
                from_stanza: false,
            });
        }
    }
    for (k, spec) in &manifest.dep {
        if seen.insert(k.clone()) {
            out.push(required_from_spec(k, spec));
        } else if let Some(slot) = out.iter_mut().find(|r| &r.key == k) {
            // a shorthand entry also has a stanza: enrich it.
            *slot = required_from_spec(k, spec);
        }
    }
    Ok(out)
}

fn required_from_spec(key: &str, spec: &DepSpec) -> RequiredDep {
    RequiredDep {
        key: key.to_string(),
        version: spec.version.clone(),
        package: spec.package.clone(),
        from_stanza: true,
    }
}

pub fn init(project: &str, rel: &str, scripts: &str, patches: &str, force: bool) -> Result<()> {
    let project = Path::new(project);
    let dir = module_dir(project, rel);
    std::fs::create_dir_all(&dir)?;

    let name = project
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project")
        .to_string();

    let cpm = format!(
        "# .cpm — generated by `cpm init`. Tool orientation; CMake does NOT read this.\n\
         # Regenerate by re-running `cpm init --force` with updated --dir/--scripts/--patches.\n\
         [paths]\n\
         scripts = \"{scripts}\"   # vendored CPM.cmake lives here (→ `cpm update`)\n\
         module  = \"{rel}\"      # deps.toml + engine + Find<Name>.cmake\n\
         patches = \"{patches}\"   # patch root (→ CPM_PATCH_PREFIX)\n",
    );
    let toml = format!(
        "# cpm project manifest. Layers: global pantry ($CPM_HOME/deps.toml) is the base,\n\
         # this file overlays it (more local wins). After editing: cpm generate {proj}\n\
         project      = \"{name}\"\n\
         network_when = \"{net}\"            # configure-time predicate gating git/tar tiers\n\
         # patch_prefix = \"{pp}\"   # where <archive-base>/<name>.patch files live\n\
         # To override the patch tool (e.g. on Windows), in configure.cmake:\n\
         #   set(CPM_PATCH_COMMAND \"${{Python3_EXECUTABLE}} -m patch -p1\" CACHE STRING \"\" FORCE)\n\
         \n\
         # simple: pantry keys included as-is\n\
         # deps = [\"fmt\", \"glm\"]\n\
         \n\
         # rich: per-dep override table. Examples:\n\
         # [dep.boost]\n\
         # package = \"Boost\"; git = false\n\
         # tarball = \"https://github.com/boostorg/boost/releases/download/boost-1.87.0/boost-1.87.0-cmake.tar.xz\"\n\
         # options = [\"EXCLUDE_FROM_ALL YES\", \"OVERRIDE_FIND_PACKAGE\"]\n\
         # pre = ''' ...emscripten-only cmake... '''\n\
         # [dep.stb]            # synthetic, target-only\n\
         # package = \"STB\"; no_source = true\n\
         # post = '''add_library(stb::headers ALIAS stb_headers)'''\n\
         deps = []\n",
        proj = project.display(),
        net = DEFAULT_NETWORK_WHEN,
        pp = DEFAULT_PATCH_PREFIX,
    );

    let mut wrote = 0u32;
    let mut kept = 0u32;
    write_if_new(&dir.join("get_cpm.cmake"), GET_CPM_DEFAULT_CMAKE, force, &mut wrote, &mut kept)?;
    write_if_new(&dir.join("3rdparty.cmake"), ENGINE_CMAKE, force, &mut wrote, &mut kept)?;
    write_if_new(&project.join(".cpm"), &cpm, force, &mut wrote, &mut kept)?;
    write_if_new(&dir.join("deps.toml"), &toml, force, &mut wrote, &mut kept)?;

    println!();
    if force {
        println!("initialized (--force): {wrote} written, {kept} kept");
    } else if kept == 0 {
        println!("initialized: {wrote} file(s) written");
    } else {
        println!("{wrote} written, {kept} kept — pass --force to overwrite");
    }
    println!();
    println!("in CMakeLists (before find_package):");
    println!("  list(APPEND CMAKE_MODULE_PATH \"${{CMAKE_SOURCE_DIR}}/{rel}\")");
    println!("  include(\"${{CMAKE_SOURCE_DIR}}/{rel}/3rdparty.cmake\")   # optional, preloads engine");
    println!();
    println!("  find_package(<dep> REQUIRED)   # one per dep");
    println!();
    println!("next: edit {}/deps.toml, then `cpm generate {}`", dir.display(), project.display());
    Ok(())
}

/// Write `content` to `path` unless it already exists (unless `force`).
fn write_if_new(path: &Path, content: &str, force: bool, wrote: &mut u32, kept: &mut u32) -> Result<()> {
    if path.exists() && !force {
        println!("kept    {}", path.display());
        *kept += 1;
    } else {
        std::fs::write(path, content)?;
        println!("wrote   {}", path.display());
        *wrote += 1;
    }
    Ok(())
}

pub fn generate(project: &str, rel: &str) -> Result<()> {
    let project = Path::new(project);
    let dir = module_dir(project, rel);
    let manifest_path = dir.join("deps.toml");
    let txt = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {} (run `cpm init {}` first)", manifest_path.display(), project.display()))?;
    let manifest: Manifest = toml::from_str(&txt).context("parse deps.toml")?;

    let pantry = deps::load()?;
    let net = manifest
        .network_when
        .clone()
        .unwrap_or_else(|| DEFAULT_NETWORK_WHEN.to_string());

    // ordered union of keys: shorthand list first, then [dep.*] extras.
    let mut keys: Vec<String> = manifest.deps.clone();
    for k in manifest.dep.keys() {
        if !keys.contains(k) {
            keys.push(k.clone());
        }
    }

    let mut body = String::new();
    body.push_str("\n# ===== registrations (generated by `cpm generate`) =====\n");
    let mut generated: Vec<String> = Vec::new();

    for key in &keys {
        let spec = manifest.dep.get(key).cloned().unwrap_or_default();
        // `version` may be an exact pin, a constraint (^/~/>=/...), or absent (=freshest).
        let parsed_spec = spec
            .version
            .as_deref()
            .map(spec::Spec::parse)
            .transpose()
            .with_context(|| format!("dep '{key}': bad version spec"))?;
        let base = pantry.resolve_dep(key, parsed_spec.as_ref());

        // valid if any source data is available: pantry entry, explicit source
        // list, no_source (synthetic), or a declared tarball/git url.
        let has_data = base.is_some()
            || spec.source.is_some()
            || spec.no_source == Some(true)
            || spec.tarball.is_some()
            || matches!(spec.git, Some(GitField::Url(_)));
        if !has_data {
            if let Some(s) = &parsed_spec {
                bail!(
                    "no version of '{key}' satisfies `{}` (available: {})",
                    s.display(),
                    if pantry.version_strings(key).is_empty() {
                        "none".into()
                    } else {
                        pantry.version_strings(key).join(", ")
                    }
                );
            }
            bail!(
                "dep '{key}' has no source data.\n\
                 add it via `cpm add {key} <url> <tag>`, or declare git/tarball explicitly in deps.toml."
            );
        }

        let pkg = spec.package.clone().unwrap_or_else(|| key.clone());
        // Emit the resolved concrete version (from pantry), not the constraint
        // string. Fall back to an exact spec/tag only when there is no pantry
        // entry (e.g. synthetic no_source deps).
        let version = base
            .and_then(|b| b.version.clone())
            .or_else(|| {
                spec.version
                    .clone()
                    .filter(|s| matches!(spec::Spec::parse(s), Ok(spec::Spec::Exact(_))))
            })
            .or_else(|| spec.tag.clone())
            .or_else(|| base.and_then(|b| b.tag.clone()));

        // package-level options (tokens)
        let mut pkg_opts: Vec<String> = Vec::new();
        pkg_opts.push("OVERRIDE_FIND_PACKAGE".into());
        if let Some(sub) = &spec.source_subdir {
            pkg_opts.push("SOURCE_SUBDIR".into());
            pkg_opts.push(sub.clone());
        }
        if let Some(opts) = &spec.options {
            // OVERRIDE_FIND_PACKAGE is added once at package level; GIT_SHALLOW is
            // git-tier only — strip both (and a trailing bool) from user tokens.
            const BOOLS: &[&str] = &["YES", "NO", "ON", "OFF", "TRUE", "FALSE"];
            let toks: Vec<String> = opts.iter().flat_map(|o| tokenize(o)).collect();
            let mut skip_bool = false;
            for t in toks {
                if skip_bool && BOOLS.contains(&t.as_str()) {
                    skip_bool = false;
                    continue;
                }
                skip_bool = false;
                if t == "OVERRIDE_FIND_PACKAGE" || t == "GIT_SHALLOW" {
                    skip_bool = true;
                    continue;
                }
                pkg_opts.push(t);
            }
        }
        if spec.download_only == Some(true)
            && !pkg_opts.iter().any(|o| o == "DOWNLOAD_ONLY")
        {
            pkg_opts.push("DOWNLOAD_ONLY".into());
        }
        // patches -> PATCH_COMMAND ${CPM_PATCH_COMMAND} <f1> && ... (package level)
        if let Some(patches) = &spec.patches {
            let archbase = archive_base(&spec, base, key, &version);
            let mut pcmd: Vec<String> = vec!["PATCH_COMMAND".into()];
            for (i, p) in patches.iter().enumerate() {
                if i > 0 {
                    pcmd.push("&&".into());
                }
                pcmd.push("${CPM_PATCH_COMMAND}".into());
                pcmd.push(format!("${{CPM_PATCH_PREFIX}}/{archbase}/{p}.patch"));
            }
            pkg_opts.extend(pcmd);
        }

        // aliases
        let alias_pairs: Vec<String> = spec
            .aliases
            .as_ref()
            .map(|m| m.iter().map(|(a, u)| format!("{a}={u}")).collect())
            .unwrap_or_default();

        // PRE/POST files
        let (pre_file, post_file) = write_hooks(&dir, key, &spec.pre, &spec.post)?;

        // package declaration
        body.push_str(&format!("cpm_declare_package(KEY {key} PACKAGE {pkg}"));
        if let Some(v) = &version {
            body.push_str(&format!(" VERSION {}", emit_tok(v)));
        }
        if !pkg_opts.is_empty() {
            body.push_str(" OPTIONS");
            emit_tok_list(&mut body, &pkg_opts);
        }
        if !alias_pairs.is_empty() {
            body.push_str(" ALIASES");
            for p in &alias_pairs {
                body.push(' ');
                body.push_str(&emit_tok(p));
            }
        }
        if let Some(pf) = &pre_file {
            body.push_str(&format!(" PRE {}", emit_tok(pf)));
        }
        if let Some(pf) = &post_file {
            body.push_str(&format!(" POST {}", emit_tok(pf)));
        }
        body.push_str(")\n");

        // candidates
        let cands = synthesize_candidates(key, &spec, base, &version, &net);
        for c in cands {
            body.push_str(&format!(
                "cpm_declare_fallback(KEY {key} TIER {}",
                emit_tok(&c.tier)
            ));
            if let Some(w) = &c.when {
                body.push_str(&format!(" WHEN {}", emit_tok(w)));
            }
            if c.tier == "git" {
                if let Some(g) = &c.git {
                    body.push_str(&format!(" GIT {}", emit_tok(g)));
                }
                if let Some(t) = &c.tag {
                    body.push_str(&format!(" TAG {}", emit_tok(t)));
                }
            } else if c.tier == "tar" {
                if let Some(u) = &c.url {
                    body.push_str(&format!(" URL {}", emit_tok(u)));
                }
            } else if c.tier == "loc" {
                if let Some(u) = &c.url {
                    body.push_str(&format!(" URL {}", emit_tok(u)));
                }
                if let Some(h) = &c.hash {
                    body.push_str(&format!(" HASH {}", emit_tok(h)));
                }
            }
            if let Some(opts) = &c.options {
                let toks: Vec<String> = opts.iter().flat_map(|o| tokenize(o)).collect();
                if !toks.is_empty() {
                    body.push_str(" OPTIONS");
                    emit_tok_list(&mut body, &toks);
                }
            }
            body.push_str(")\n");
        }

        generated.push(pkg.clone());
    }

    // write engine + body
    let mut engine = String::from(ENGINE_CMAKE);
    engine.push_str(&body);
    std::fs::write(dir.join("3rdparty.cmake"), engine)?;

    // Find<Package>.cmake per dep
    for pkg in &generated {
        let key_for_find = find_key_for_package(&manifest, pkg);
        let content = format!(
            "# Find{pkg}.cmake — generated by `cpm generate`. Do not edit by hand.\n\
             include(\"${{CMAKE_CURRENT_LIST_DIR}}/3rdparty.cmake\")\n\
             cpm_resolve_fallback(\"{key}\")\n\
             if(NOT {pkg}_FOUND AND {pkg}_FIND_REQUIRED)\n\
               message(FATAL_ERROR \"{pkg}: cpm could not resolve any source tier (git/tarball/local)\")\n\
             endif()\n",
            pkg = pkg,
            key = key_for_find
        );
        std::fs::write(dir.join(format!("Find{pkg}.cmake")), content)?;
    }

    println!(
        "generated {} dep(s) -> {}",
        generated.len(),
        dir.display()
    );
    for pkg in &generated {
        println!("  Find{pkg}.cmake");
    }
    Ok(())
}

/// Map a package (find_name) back to its toml key for the Find module's resolve call.
fn find_key_for_package(manifest: &Manifest, pkg: &str) -> String {
    for (k, spec) in &manifest.dep {
        if spec.package.as_deref() == Some(pkg) {
            return k.clone();
        }
    }
    // shorthand deps: key == package
    for k in &manifest.deps {
        if k == pkg {
            return k.clone();
        }
    }
    pkg.to_string()
}

/// A `pre`/`post` value is a file reference (passed through to the engine
/// verbatim) when it is a single line ending in `.cmake`; otherwise it is inline
/// CMake code, written to a generated `pre_<key>.cmake`/`post_<key>.cmake`.
fn is_hook_file_ref(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && !t.contains('\n') && t.to_ascii_lowercase().ends_with(".cmake")
}

fn write_hooks(
    dir: &Path,
    key: &str,
    pre: &Option<String>,
    post: &Option<String>,
) -> Result<(Option<String>, Option<String>)> {
    let pre_file = match pre {
        Some(s) if !s.trim().is_empty() => {
            if is_hook_file_ref(s) {
                Some(s.trim().to_string())
            } else {
                let f = format!("pre_{key}.cmake");
                std::fs::write(dir.join(&f), format!("# pre[{key}] — generated\n{s}\n"))?;
                Some(f)
            }
        }
        _ => None,
    };
    let post_file = match post {
        Some(s) if !s.trim().is_empty() => {
            if is_hook_file_ref(s) {
                Some(s.trim().to_string())
            } else {
                let f = format!("post_{key}.cmake");
                std::fs::write(dir.join(&f), format!("# post[{key}] — generated\n{s}\n"))?;
                Some(f)
            }
        }
        _ => None,
    };
    Ok((pre_file, post_file))
}

/// archive "base" name (without archive suffix) — used as the patch subdir.
fn archive_base(spec: &DepSpec, base: Option<&deps::Dep>, key: &str, version: &Option<String>) -> String {
    let arch = spec
        .archive
        .clone()
        .or_else(|| base.map(|b| b.archive.clone()))
        .unwrap_or_else(|| {
            format!("{key}-{}", version.clone().unwrap_or_default())
        });
    arch.trim_end_matches(".zip")
        .trim_end_matches(".tar.gz")
        .trim_end_matches(".tar.xz")
        .to_string()
}

/// Build the ordered candidate list. Explicit `[[dep.X.source]]` wins; otherwise
/// synthesise git → tar → loc from the merged layer data.
fn synthesize_candidates(
    _key: &str,
    spec: &DepSpec,
    base: Option<&deps::Dep>,
    version: &Option<String>,
    net: &str,
) -> Vec<Candidate> {
    if let Some(srcs) = &spec.source {
        return srcs.clone();
    }
    if spec.no_source == Some(true) {
        return Vec::new();
    }

    let mut out = Vec::new();
    let tag = spec
        .tag
        .as_ref()
        .map(|t| subst_version(t, version))
        .or_else(|| base.and_then(|b| b.tag.clone()));

    let git_disabled = matches!(spec.git, Some(GitField::Bool(false)));
    let git_url = match &spec.git {
        Some(GitField::Url(u)) => Some(u.clone()),
        Some(GitField::Bool(false)) => None,
        Some(GitField::Bool(true)) | None => base.and_then(|b| b.url.clone()),
    };

    // tier: git
    if !git_disabled {
        if let (Some(url), Some(tag)) = (git_url.as_ref(), tag.as_ref()) {
            out.push(Candidate {
                tier: "git".into(),
                when: Some(net.to_string()),
                git: Some(url.clone()),
                tag: Some(tag.clone()),
                options: Some(vec!["GIT_SHALLOW ON".into()]),
                ..Default::default()
            });
        }
    }

    // tier: tarball (explicit only — auto-synthesised github/gitlab "archive"
    // tarballs are NOT used because they omit submodules and are unreliable).
    if let Some(t) = spec.tarball.clone() {
        out.push(Candidate {
            tier: "tar".into(),
            when: Some(net.to_string()),
            url: Some(t),
            ..Default::default()
        });
    }

    // tier: local archive
    let archive = spec
        .archive
        .clone()
        .or_else(|| base.map(|b| b.archive.clone()))
        .unwrap_or_else(|| {
            format!(
                "{}-{}.zip",
                _key,
                version.clone().unwrap_or_default()
            )
        });
    let hash = base
        .and_then(|b| b.sha256.clone())
        .map(|h| format!("SHA256={h}"));
    out.push(Candidate {
        tier: "loc".into(),
        when: None,
        url: Some(format!("${{CPM_PRELOAD}}/{archive}")),
        hash,
        ..Default::default()
    });

    out
}

fn subst_version(tag: &str, version: &Option<String>) -> String {
    match version {
        Some(v) => tag.replace("${version}", v),
        None => tag.replace("${version}", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokeniser_handles_quotes() {
        assert_eq!(tokenize(r#"OPTIONS "A B" C"#), vec!["OPTIONS", "A B", "C"]);
        assert_eq!(tokenize("GIT_SHALLOW ON"), vec!["GIT_SHALLOW", "ON"]);
    }

    #[test]
    fn subst_version_works() {
        assert_eq!(subst_version("v.${version}", &Some("3.5.2".into())), "v.3.5.2");
    }

    #[test]
    fn hook_file_ref_detection() {
        // file references
        assert!(is_hook_file_ref("my_post.cmake"));
        assert!(is_hook_file_ref("${CMAKE_CURRENT_LIST_DIR}/sub/my.cmake"));
        assert!(is_hook_file_ref("/abs/path/my.cmake"));
        // inline code is not
        assert!(!is_hook_file_ref("add_library(foo INTERFACE)"));
        assert!(!is_hook_file_ref("if(NOT TARGET foo)\n  add_library(foo)\nendif()"));
        assert!(!is_hook_file_ref(""));
    }
}

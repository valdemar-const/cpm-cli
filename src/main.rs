mod archive;
mod bootstrap;
mod commands;
mod config;
mod deps;
mod gen;
mod git;
mod source;
mod spec;

use clap::{Parser, Subcommand};

#[derive(Clone, Copy, clap::ValueEnum)]
enum KindArg {
    Auto,
    Git,
    Fetch,
}

#[derive(Parser)]
#[command(
    name = "cpm",
    version,
    about = "Local preloader & tooling around CPM.cmake",
    long_about = "Manages a store of prebuilt source archives for CPMAddPackage.\n\
                  Archive storage: $CPM_PRELOAD. Tool state: $CPM_HOME (~/.local/share/cpm)."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Acquire a dependency into $CPM_PRELOAD
    ///
    /// Fetches the source (git clone, an archive download, or a local dir/file),
    /// strips VCS metadata, dereferences symlinks, and packs it under
    /// <name>-<version>.zip, then registers it in the pantry ($CPM_HOME/deps.toml).
    ///
    /// Source kinds (--kind, default auto):
    ///
    ///   git    - `git clone` of a remote or local repo. TAG selects the ref and
    ///            the version is derived from it (v1.2.3, boost-1.90.0, ...).
    ///
    ///   fetch  - download an archive (http(s)://), or extract a local .zip/.tar.*,
    ///            or copy a local directory. TAG is ignored; the version comes from
    ///            --version or the archive basename.
    ///
    ///   auto   - inferred: archive extension or a local file/dir => fetch; a local
    ///            git repo => git; otherwise git.
    ///
    /// If the target archive already exists and --force is not set, nothing is
    /// re-fetched: for git the url+tag provenance is (re)recorded, for fetch the
    /// entry is simply kept (source equivalence is assumed).
    ///
    /// Examples:
    ///
    ///   cpm add fmt git@github.com:fmtlib/fmt.git 10.2.1
    ///
    ///   cpm add boost git@github.com:boostorg/boost.git boost-1.90.0
    ///
    ///   cpm add foo https://example.com/foo-1.2.3.tar.gz
    ///
    ///   cpm add foo ./local-src --kind fetch --version 1.0.0
    ///
    ///   cpm add bar git@github.com:o/r.git abc123 --commit
    ///
    /// See also: `cpm source add` (attach a source without fetching), `cpm info`.
    Add {
        /// CPM/CPMAddPackage package name (also the find_package name).
        name: String,
        /// Git URL (git), or an archive URI / local path (fetch).
        url: String,
        /// Branch, tag, or (with --commit) commit hash. Required for --kind=git;
        /// ignored for --kind=fetch.
        tag: Option<String>,
        /// Source kind: `git` clones, `fetch` downloads/extracts/copies. `auto`
        /// infers from the URL/path.
        #[arg(long, value_enum, default_value = "auto")]
        kind: KindArg,
        /// Override archive filename (default: <name>-<version>.zip).
        #[arg(long)]
        archive: Option<String>,
        /// Override VERSION reported by `cpm show` (required for fetch when the
        /// archive basename has no semver).
        #[arg(long)]
        version: Option<String>,
        /// Treat `tag` as a commit hash (git only).
        #[arg(long)]
        commit: bool,
        /// Overwrite an existing archive (re-fetch and repack).
        #[arg(long)]
        force: bool,
    },
    /// List registered dependencies, their versions, sources and archive status
    ///
    /// One line per pantry entry: name, version, tag, on-disk size, archive
    /// filename, status (ok / MISSING / no CPM_PRELOAD). Sorted by name, then
    /// version freshest-first. For per-version sources use `cpm info <name>`.
    List,
    /// Build any missing archives from the pantry (re-clones sourced entries)
    ///
    /// Walks the pantry and (re)creates every archive that has a git source
    /// (url+tag) but no file on disk — or all of them with --force. Loc-only
    /// entries (no source) are skipped; attach one with `cpm source add` first.
    Fetch {
        /// Rebuild every archive even if it already exists.
        #[arg(long)]
        force: bool,
    },
    /// Register archives already in $CPM_PRELOAD into the pantry
    ///
    /// Scans $CPM_PRELOAD for *.zip, parses <name>-<version>.zip, computes sha256
    /// and registers each entry loc-only (no source); non-conforming filenames
    /// are skipped. With -f, re-fetches each archive from its pantry source
    /// instead (requires url+tag; errors listing the unsourced ones).
    Import {
        /// Re-fetch from source (requires url+tag in pantry; else errors).
        #[arg(short, long)]
        force: bool,
    },
    /// Remove a dependency from the global pantry (inverse of `cpm add`)
    ///
    /// Drops the pantry entry (all versions, or one with `--version`) and deletes
    /// its archive from `$CPM_PRELOAD`. Re-add anytime with `cpm add`.
    /// `--dry-run` previews without deleting.
    Rm {
        /// Dependency name.
        name: String,
        /// Remove a specific version (default: all versions of this name).
        #[arg(long)]
        version: Option<String>,
        /// Preview what would be removed; change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print a ready-to-paste CPMAddPackage(...) snippet for a dependency
    ///
    /// Uses the freshest version. With --hash, includes a URL_HASH (SHA256) line.
    /// For an overview of all versions and their sources, use `cpm info`.
    Show {
        /// Dependency name (freshest version is used).
        name: String,
        /// Include a URL_HASH (SHA256) line.
        #[arg(long)]
        hash: bool,
    },
    /// Show a dependency summary: every version and its load sources (git/loc)
    ///
    /// For each archived version prints the git upstream (url @ tag, or a hint to
    /// add one) and the local archive (filename, size, short sha, presence).
    /// The `git remote -v` equivalent for a dependency.
    Info {
        /// Dependency name.
        name: String,
    },
    /// Manage the git source of a pantry entry (cf. `git remote`)
    ///
    /// A "source" is a way to (re)acquire a version's snapshot. `source add`
    /// attaches a git url+tag to an existing entry without fetching; `source rm`
    /// detaches it, leaving the entry loc-only. Equivalence is assumed — the
    /// source is trusted to match the archive, never cross-checked.
    Source {
        #[command(subcommand)]
        cmd: SourceCmd,
    },
    /// Download official CPM.cmake + get_cpm.cmake into the cpm data dir
    ///
    /// Fetches CPM.cmake <version> (or the latest with --latest, or the configured
    /// default) into $CPM_HOME/cmake/<version>/ and records it as the active
    /// version for the tool. Per-project CPM.cmake is refreshed with `cpm update`.
    Bootstrap {
        /// Specific CPM.cmake version (e.g. 0.43.1).
        #[arg(long)]
        version: Option<String>,
        /// Pick the latest release from GitHub.
        #[arg(long)]
        latest: bool,
    },
    /// Refresh the vendored CPM.cmake to the latest stable release
    ///
    /// Default (project-local): downloads the latest CPM.cmake into [paths]
    /// scripts/ (located via the .cpm anchor at the project root). With -g,
    /// bumps the version pinned in this tool's own source tree
    /// (src/get_cpm_default.cmake + config.rs) — rebuild the tool afterwards.
    /// --check is read-only: prints current vs latest, changes nothing.
    Update {
        /// Target this tool's own bundle instead of the current project.
        #[arg(short, long)]
        global: bool,
        /// Show current vs latest without modifying anything.
        #[arg(long)]
        check: bool,
    },
    /// Show resolved paths and environment
    ///
    /// Prints CPM_HOME, CPM_PRELOAD, CPM_SOURCE_CACHE, the deps registry path
    /// and the active CPM.cmake. With --export, also prints `export` lines to
    /// append to your shell rc.
    Env {
        /// Also print shell `export` lines for your rc.
        #[arg(long)]
        export: bool,
    },
    /// Inspect an archive: sha256, size, entries, CMakeLists presence, .git check
    ///
    /// TARGET may be a dep name (its freshest archive), an archive filename in
    /// $CPM_PRELOAD, or a path to a .zip. Reports file/dir counts, top-level
    /// entries, whether a top-level CMakeLists.txt exists, any .git contamination,
    /// and cross-checks the sha256 against the pantry entry.
    Verify {
        /// Dep name, archive filename, or path to a .zip.
        target: String,
    },
    /// Initialize a cpm 3rdparty module in a project
    ///
    /// Generates, under <dir> (default build/cmake/modules/3rdparty):
    ///
    ///   3rdparty.cmake  - the static fallback engine (registrations appended later)
    ///
    ///   deps.toml       - the project manifest (edit this, then `cpm generate`)
    ///
    ///   get_cpm.cmake   - downloads CPM.cmake on demand
    ///
    /// and a .cpm anchor at the project root (paths for `cpm update`/`generate`).
    /// Non-destructive: existing files are kept unless --force. Then in CMakeLists:
    ///
    ///   list(APPEND CMAKE_MODULE_PATH "${CMAKE_SOURCE_DIR}/<dir>")
    ///
    ///   include("${CMAKE_SOURCE_DIR}/<dir>/3rdparty.cmake")
    ///
    ///   find_package(<dep> REQUIRED)
    Init {
        /// Project root (default: current dir).
        #[arg(default_value = ".")]
        project: String,
        /// Module dir, relative to project root.
        #[arg(long, default_value = "build/cmake/modules/3rdparty")]
        dir: String,
        /// Where the vendored CPM.cmake lives (relative to project root).
        #[arg(long, default_value = "build/cmake/scripts")]
        scripts: String,
        /// Patch root (relative to project root).
        #[arg(long, default_value = "build/patches")]
        patches: String,
        /// Overwrite existing files (default: keep them).
        #[arg(long)]
        force: bool,
    },
    /// (Re)generate Find<Name>.cmake + fallback registrations from deps.toml
    ///
    /// Merges the project deps.toml over the global pantry ($CPM_HOME/deps.toml),
    /// rewrites the engine body in <dir>/3rdparty.cmake, and emits a
    /// Find<Package>.cmake per dependency that triggers resolution at
    /// find_package() time. Re-run after every deps.toml change.
    Generate {
        /// Project root (default: current dir).
        #[arg(default_value = ".")]
        project: String,
        /// Module dir, relative to project root.
        #[arg(long, default_value = "build/cmake/modules/3rdparty")]
        dir: String,
    },
    /// Manage this project's required deps (edits deps.toml + regenerates glue)
    ///
    /// Run from anywhere inside a cpm project (located via `.cpm`). Every
    /// subcommand resolves against the global pantry and regenerates the CMake
    /// glue. Version spec grammar: a bare `1.90.0` is an EXACT pin; a leading
    /// operator (`^1.85`, `~1.90.0`, `>=1.85 <2.0`) is a constraint re-resolved
    /// at generate; `*`/omitted = freshest from the pantry.
    Requires {
        #[command(subcommand)]
        cmd: RequiresCmd,
    },
}

#[derive(Subcommand)]
enum RequiresCmd {
    /// Add a dependency: resolve a version, write `[dep.X]`, regenerate
    ///
    /// Writes a fully-defaulted stanza (package + version + a commented menu of
    /// optional overrides). Re-running with a different spec updates only
    /// `version`. Validates against the pantry; errors if no version satisfies.
    Add {
        /// Dependency name (pantry key).
        name: String,
        /// Version spec (see `cpm requires --help`). Omitted = freshest.
        spec: Option<String>,
        /// Override the CPM package NAME if it differs from the dependency name.
        #[arg(long)]
        package: Option<String>,
    },
    /// Remove a dependency from deps.toml (and regenerate)
    Rm {
        /// Dependency name.
        name: String,
    },
    /// List this project's required deps with their resolved versions
    ///
    /// With `--outdated`, show only deps whose resolved version is older than the
    /// freshest available in the pantry (no network — purely a pantry comparison).
    List {
        #[arg(long)]
        outdated: bool,
    },
    /// Bump a dependency's version, preserving all its other deps.toml settings
    ///
    /// Rewrites ONLY the `version` field (options, hooks, patches, comments are
    /// preserved). Omit the spec to pin the freshest available; if the dep is
    /// currently a constraint, reports its freshest match without changing the
    /// file.
    Bump {
        /// Dependency name.
        name: String,
        /// New version spec. Omit = freshest.
        spec: Option<String>,
    },
}

#[derive(Subcommand)]
enum SourceCmd {
    /// Attach a git source (url+tag) to an existing pantry entry (no fetch)
    ///
    /// The archive must already be on disk (use `cpm add` to fetch a new dep).
    /// Records the url+tag provenance so `cpm fetch`/`import -f` can re-acquire
    /// it and the engine's git tier can reach it. Equivalence is assumed: the
    /// source is trusted to match the archive, never cross-checked.
    Add {
        /// Dependency name.
        name: String,
        /// Git URL of the upstream (e.g. git@github.com:owner/repo.git).
        url: String,
        /// Tag/branch/commit that matches this version's snapshot.
        tag: String,
        /// Target a specific version (needed when the tag is not a semver, or
        /// the name has several versions).
        #[arg(long)]
        version: Option<String>,
    },
    /// Remove the git source from an entry (back to loc-only)
    ///
    /// Clears the url+tag; the archive stays. The entry is now loc-only (not
    /// re-fetchable). With several versions, --version selects which one.
    Rm {
        /// Dependency name.
        name: String,
        /// Which version (required when the name has several).
        #[arg(long)]
        version: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Add {
            name,
            url,
            tag,
            kind,
            archive,
            version,
            commit,
            force,
        } => {
            let kind = match kind {
                KindArg::Auto => source::detect_kind(&url),
                KindArg::Git => source::Kind::Git,
                KindArg::Fetch => source::Kind::Fetch,
            };
            commands::add(
                &name,
                &url,
                tag.as_deref(),
                kind,
                archive.as_deref(),
                version.as_deref(),
                commit,
                force,
            )
        }
        Cmd::List => commands::list(),
        Cmd::Fetch { force } => commands::fetch(force),
        Cmd::Import { force } => commands::import(force),
        Cmd::Rm { name, version, dry_run } => commands::rm(&name, version.as_deref(), dry_run),
        Cmd::Show { name, hash } => commands::show(&name, hash),
        Cmd::Info { name } => commands::info(&name),
        Cmd::Source { cmd } => match cmd {
            SourceCmd::Add { name, url, tag, version } => {
                commands::source_add(&name, &url, &tag, version.as_deref())
            }
            SourceCmd::Rm { name, version } => commands::source_rm(&name, version.as_deref()),
        },
        Cmd::Bootstrap { version, latest } => bootstrap::run(version.as_deref(), latest),
        Cmd::Update { global, check } => bootstrap::update(global, check),
        Cmd::Env { export } => commands::env(export),
        Cmd::Verify { target } => commands::verify(&target),
        Cmd::Init { project, dir, scripts, patches, force } => {
            gen::init(&project, &dir, &scripts, &patches, force)
        }
        Cmd::Generate { project, dir } => gen::generate(&project, &dir),
        Cmd::Requires { cmd } => match cmd {
            RequiresCmd::Add { name, spec, package } => {
                commands::requires_add(&name, spec.as_deref(), package.as_deref())
            }
            RequiresCmd::Rm { name } => commands::requires_rm(&name),
            RequiresCmd::List { outdated } => commands::requires_list(outdated),
            RequiresCmd::Bump { name, spec } => commands::bump(&name, spec.as_deref()),
        },
    }
}

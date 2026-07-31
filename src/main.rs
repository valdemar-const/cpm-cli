mod archive;
mod bootstrap;
mod commands;
mod config;
mod deps;
mod gen;
mod git;
mod source;

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
    /// Acquire a dependency into $CPM_PRELOAD: clone (git) or download/extract/copy
    /// (fetch), then clean, dereference symlinks, and pack under <name>-<ver>.zip.
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
        /// Overwrite an existing archive.
        #[arg(long)]
        force: bool,
    },
    /// List registered dependencies and archive status.
    List,
    /// Build any missing archives from the registry.
    Fetch {
        /// Rebuild every archive even if it already exists.
        #[arg(long)]
        force: bool,
    },
    /// Register archives already in $CPM_PRELOAD into the pantry.
    Import {
        /// Re-fetch from source (requires url+tag in pantry; else errors).
        #[arg(short, long)]
        force: bool,
    },
    /// Print a ready-to-paste CPMAddPackage(...) snippet.
    Show {
        name: String,
        /// Include a URL_HASH (SHA256) line.
        #[arg(long)]
        hash: bool,
    },
    /// Show a dependency summary: every version and its load sources (git/loc).
    Info {
        name: String,
    },
    /// Manage the git source of a pantry entry (cf. `git remote`).
    Source {
        #[command(subcommand)]
        cmd: SourceCmd,
    },
    /// Download official CPM.cmake + get_cpm.cmake into the cpm data dir.
    Bootstrap {
        /// Specific CPM.cmake version (e.g. 0.42.0).
        #[arg(long)]
        version: Option<String>,
        /// Pick the latest release from GitHub.
        #[arg(long)]
        latest: bool,
    },
    /// Refresh the vendored CPM.cmake to the latest stable release.
    Update {
        /// Target this tool's own bundle instead of the current project.
        #[arg(short, long)]
        global: bool,
        /// Show current vs latest without modifying anything.
        #[arg(long)]
        check: bool,
    },
    /// Show resolved paths and environment.
    Env {
        /// Also print shell `export` lines for your rc.
        #[arg(long)]
        export: bool,
    },
    /// Inspect an archive (by dep name, archive filename, or path).
    Verify { target: String },
    /// Initialize a cpm 3rdparty module in a project.
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
    /// (Re)generate Find<Name>.cmake + fallback registrations from deps.toml.
    Generate {
        /// Project root (default: current dir).
        #[arg(default_value = ".")]
        project: String,
        /// Module dir, relative to project root.
        #[arg(long, default_value = "build/cmake/modules/3rdparty")]
        dir: String,
    },
}

#[derive(Subcommand)]
enum SourceCmd {
    /// Attach a git source (url+tag) to an existing pantry entry (no fetch).
    Add {
        name: String,
        url: String,
        tag: String,
        #[arg(long)]
        version: Option<String>,
    },
    /// Remove the git source from an entry (back to loc-only).
    Rm {
        name: String,
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
    }
}

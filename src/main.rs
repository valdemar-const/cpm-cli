mod archive;
mod bootstrap;
mod commands;
mod config;
mod deps;
mod gen;
mod git;

use clap::{Parser, Subcommand};

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
    /// Shallow-clone a dependency, strip VCS metadata, pack it into $CPM_PRELOAD.
    Add {
        /// CPM/CPMAddPackage package name (also the find_package name).
        name: String,
        /// Git URL (https://github.com/...git).
        url: String,
        /// Branch, tag, or (with --commit) a commit hash.
        tag: String,
        /// Override archive filename (default: <name>-<version>.zip).
        #[arg(long)]
        archive: Option<String>,
        /// Override VERSION reported by `cpm show`.
        #[arg(long)]
        version: Option<String>,
        /// Treat `tag` as a commit hash.
        #[arg(long)]
        commit: bool,
        /// Dereference symlinks into real files/dirs (improves portability, bloats size).
        #[arg(long)]
        dereference: bool,
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
    /// Print a ready-to-paste CPMAddPackage(...) snippet.
    Show {
        name: String,
        /// Include a URL_HASH (SHA256) line.
        #[arg(long)]
        hash: bool,
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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Add {
            name,
            url,
            tag,
            archive,
            version,
            commit,
            dereference,
            force,
        } => commands::add(
            &name,
            &url,
            &tag,
            archive.as_deref(),
            version.as_deref(),
            commit,
            dereference,
            force,
        ),
        Cmd::List => commands::list(),
        Cmd::Fetch { force } => commands::fetch(force),
        Cmd::Show { name, hash } => commands::show(&name, hash),
        Cmd::Bootstrap { version, latest } => bootstrap::run(version.as_deref(), latest),
        Cmd::Env { export } => commands::env(export),
        Cmd::Verify { target } => commands::verify(&target),
        Cmd::Init { project, dir } => gen::init(&project, &dir),
        Cmd::Generate { project, dir } => gen::generate(&project, &dir),
    }
}

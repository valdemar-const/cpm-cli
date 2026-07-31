# cpm

`cpm` is a developer-time helper around [CPM.cmake](https://github.com/cpm-cmake/CPM.cmake)
that tames 3rdparty dependency management for C++ projects. It pre-builds source
archives and generates **pure CMake** glue, so `find_package()` resolves each
dependency offline-first through ordered fallback tiers — and your repository
keeps building for anyone who clones it, **even if they never install `cpm`**.

> The rule that shapes everything below: `cpm` only ever *generates* files. What
> ends up in your repo is static CMake that stands on its own. `cpm` is not a
> build-time dependency.

---

## The 3rdparty pain it removes

| Without `cpm`                                       | With `cpm`                                                   |
|-----------------------------------------------------|--------------------------------------------------------------|
| Git submodules that drift and break reproducibly    | Pinned source archives, hashed, stored once per machine      |
| `find_package(Foo)` fails — vendor it by hand       | Generated `FindFoo.cmake` that always resolves               |
| CPM needs network at *configure* time               | Local archives resolve instantly; network is opt-in per tier |
| CI/offline builds fail without connectivity         | Offline builds work from the preload cache                   |
| Patches scattered, applied differently everywhere   | One patch location + one command, declarative per dep        |
| Every dev re-clones and re-builds the same sources  | Shared archive store (`$CPM_PRELOAD`)                        |

## How it stays cmake-first

`cpm` writes two things into your project:

1. **`Find<Name>.cmake`** modules — one per dependency.
2. **`3rdparty.cmake`** — a static engine that registers each package and, at
   `find_package()` time, resolves it through fallback tiers.

Both are plain CMake, committed to your repo. `cpm generate` is the only step
that reads your manifest (`deps.toml`) and emits these files. After that, the
manifest and the `cpm` binary are no longer needed to build — they're only needed
by whoever *regenerates* the glue.

The resolution order, per dependency:

```
local archive  ──►  release tarball  ──►  git clone
  (offline)         (network gate)        (network gate)
```

- **local** — a pre-built archive from `$CPM_PRELOAD`. Always available,
  instant, offline. This is the default path.
- **tarball** / **git** — tried only when a configurable network gate is on
  (e.g. an `option(CPM_DOWNLOAD ...)` you define). Lets you pull fresh sources
  when you choose to.

The first tier whose source is reachable wins.

---

## Install & environment

```sh
cargo install --path .     # or: cargo build --release && install -m755 target/release/cpm ~/.local/bin/
```

`cpm` reads three environment variables (see `cpm env`):

| Variable         | Purpose                                              | Typical value                |
|------------------|------------------------------------------------------|------------------------------|
| `CPM_PRELOAD`    | Shared store of pre-built source archives            | `~/cmake/cpm_preload`        |
| `CPM_HOME`       | `cpm`'s own state (pantry registry, tmp, bootstrap)  | `~/.local/share/cpm`         |
| `CPM_SOURCE_CACHE` | Where CPM.cmake + cloned sources are cached        | `~/cmake/cpm_cache`          |

```sh
export CPM_PRELOAD="$HOME/cmake/cpm_preload"
export CPM_HOME="$HOME/.local/share/cpm"
export CPM_SOURCE_CACHE="$HOME/cmake/cpm_cache"
```

`cpm env --export` prints ready-to-append `export` lines for your shell rc.

---

## Integrating into an existing CMake project

### 1. Scaffold the module

From your project root:

```sh
cpm init
```

This creates:

- `build/cmake/modules/3rdparty/` — the module: `3rdparty.cmake` (engine),
  `get_cpm.cmake` (downloads CPM.cmake once), and a starter `deps.toml`.
- `.cpm` — a tiny root anchor recording where `scripts`, `module`, and `patches`
  live. **CMake never reads it**; it orients `cpm` (e.g. `cpm update`) to your
  layout.

`init` is **non-destructive**: it never overwrites an existing file, so your
edits to `deps.toml` / `.cpm` survive a re-run. Pass `--force` to regenerate
everything from scratch.

**Don't want `build/`?** If your project uses `build/` for build artifacts,
relocate cpm's files (the paths are recorded in `.cpm`):

```sh
cpm init --dir out/modules/3rdparty --scripts out/scripts --patches out/patches
```

### 2. Acquire dependencies into the pantry

The shared pantry (`$CPM_HOME/deps.toml` + `$CPM_PRELOAD/*.zip`) is a multi-version
store of pre-built source archives. Add a dep once and every project on the machine
reuses it. `cpm add` acquires sources and packs them into a hashed zip:

```sh
# git (default): shallow clone at a tag, strip VCS, repack as <name>-<version>.zip
cpm add fmt https://github.com/fmtlib/fmt.git 10.2.1
cpm add glm https://github.com/g-truc/glm.git 0.9.9.8

# --kind auto|git|fetch controls how the source is acquired:
#   fetch = web archive URL / local .zip / local directory (version via --version)
cpm add foo https://example.com/foo-1.2.3.tar.gz --kind fetch --version 1.2.3
```

Inspect and manage what's staged:

```sh
cpm list                  # registered deps + archive status (adapts to terminal width)
cpm info fmt              # per-version summary: git upstream + local archive
cpm verify fmt            # peek inside an archive (name / filename / path)
cpm source add fmt <url> <tag>   # attach/repair a git upstream to an existing entry
cpm source rm fmt                # detach it (entry stays loc-only)
cpm import [--force]             # register archives in $CPM_PRELOAD (-f: re-fetch from source)
```

`cpm source add/rm` is the `git remote` analogue: it records provenance so a dep
can be re-acquired and its git tier can reach upstream. The source is trusted to
match the archive — it is never cross-checked.

### 3. Declare dependencies in your project

The ergonomic path is `cpm requires` — run it from anywhere inside the project
(located via `.cpm`). It resolves a version against the pantry, writes a
fully-defaulted `[dep.X]` stanza into `deps.toml` (with a commented menu of every
optional override), and regenerates the glue in one shot:

```sh
cpm requires add fmt            # freshest available
cpm requires add boost "^1.85"  # constraint: resolve to the freshest matching version
cpm requires add glm 1.0.1      # exact pin (a bare version is always exact)
cpm requires list               # show every required dep + its resolved version
cpm requires rm glad            # drop one
```

The version spec grammar:

| Spec              | Meaning                                                     |
|-------------------|-------------------------------------------------------------|
| `1.90.0` (bare)   | **exact pin** (matched by numeric key, so `1.9` ≡ `1.9.0`)  |
| `^1.85`           | caret — compatible: `>=1.85.0 <2.0.0`                       |
| `~1.90.0`         | tilde — patch range: `>=1.90.0 <1.91.0`                     |
| `>=1.85 <2.0`     | comparators (space- or comma-separated)                     |
| `*` / omitted     | freshest in the pantry                                      |

Bump a dep's version without touching its other settings — `cpm requires bump`
rewrites **only** the `version` field (options, hooks, patches, comments are
preserved):

```sh
cpm requires bump boost 1.90.0   # explicit target
cpm requires bump boost          # no spec → pin the freshest available
```

You can also edit `build/cmake/modules/3rdparty/deps.toml` directly (then run
`cpm generate`). It **overlays** the global pantry — declare a dep by pantry key,
or fully describe one that only lives in this project:

```toml
project      = "myapp"
network_when = "CPM_DOWNLOAD"      # CMake predicate gating the network tiers

# pulled from the pantry as-is (local archive + git/tar tiers baked in)
deps = ["fmt", "glm"]

# rich per-dep overrides (these are the fields `cpm requires` fills for you):
[dep.freetype]
package = "Freetype"
version = "2.14.1"            # exact pin, or a constraint like "^2.14"
git     = "https://github.com/freetype/freetype.git"
tag     = "VER-2-14-1"

[dep.glad]
package       = "Glad"
git           = false                                    # no git tier
tarball       = "https://github.com/Dav1dde/glad/archive/refs/tags/v2.0.8.zip"
source_subdir = "cmake"
options       = ["GLAD_API gl=gl"]
download_only = true                                     # fetch source, skip add_subdirectory
post          = "${CMAKE_CURRENT_LIST_DIR}/glad_post.cmake"   # a .cmake file ref

[dep.stb]                                                   # synthetic, target-only
package   = "STB"
no_source = true
post      = '''add_library(stb::headers ALIAS stb_headers)'''
```

`network_when` is any CMake variable that is true when network is allowed.
Define a matching option in your `CMakeLists.txt`:

```cmake
option(CPM_DOWNLOAD "Allow network during configure (git/tar tiers)" OFF)
```

### 4. Generate the CMake glue

`cpm generate` (also run automatically by `requires add`/`rm`/`bump`) emits a
`Find<Package>.cmake` for each dependency and rewrites `3rdparty.cmake` with the
static registrations synthesised from `deps.toml` + pantry. **From this point the
project is self-sufficient.**

### 5. Wire it into CMakeLists.txt

```cmake
# before any find_package():
list(APPEND CMAKE_MODULE_PATH "${CMAKE_SOURCE_DIR}/build/cmake/modules/3rdparty")
include("${CMAKE_SOURCE_DIR}/build/cmake/modules/3rdparty/3rdparty.cmake")

find_package(fmt REQUIRED)
find_package(Freetype REQUIRED)
find_package(Glad REQUIRED)
```

### 6. Build — online, then offline

First build (network on, to populate the source cache):

```sh
cmake -B build -DCPM_DOWNLOAD=ON
cmake --build build
```

Subsequent builds are fully offline from the cache and the preload store:

```sh
cmake -B build            # CPM_DOWNLOAD not set → only local archives used
cmake --build build
```

That's the whole loop: **`cpm add` / `cpm requires` → (edit) → `cpm generate` → build.**

---

## Hooks (`pre` / `post`) and `download_only`

Run CMake around a dep's `add_subdirectory()`. A hook is either an **inline
snippet** (written to `pre_<key>.cmake` / `post_<key>.cmake` next to the engine)
or a **`.cmake` file reference** (a single line ending in `.cmake`, e.g.
`${CMAKE_CURRENT_LIST_DIR}/my_post.cmake`), which is passed through verbatim —
so you can keep complex glue under your own version-controlled file.

```toml
[dep.glad]
download_only = true                                      # fetch source, skip add_subdirectory
post = "${CMAKE_CURRENT_LIST_DIR}/glad_post.cmake"        # declare the targets yourself

[dep.foo]
post = '''if(NOT TARGET foo::foo)
  add_library(foo::foo ALIAS foo)
endif()'''                                                # inline snippet → post_foo.cmake
```

`download_only = true` makes CPM fetch the source but not build it — you declare
the target(s) in `post`. Inside a post hook the fetched tree is exposed as
`${<Package>_SOURCE_DIR}` (read from CPM's cache variable).

---

## Patches

Apply fixes to vendored sources declaratively. Patch files live under a per-dep
directory keyed by the archive base name:

```
build/patches/
  fmt-10.2.1/
    fix-abi.patch
```

In `deps.toml`:

```toml
[dep.fmt]
patches = ["fix-abi.patch"]
```

Two CMake variables control application (override them **before** including
`3rdparty.cmake`):

- `CPM_PATCH_PREFIX` — patch root, default `${PROJECT_SOURCE_DIR}/build/patches`.
- `CPM_PATCH_COMMAND` — the apply prefix (takes one `.patch` as its last arg),
  default `patch -p1 -i`. On Windows, override in `configure.cmake`:

```cmake
set(CPM_PATCH_COMMAND "${Python3_EXECUTABLE} -m patch -p1" CACHE STRING "" FORCE)
```

---

## Keeping CPM.cmake fresh

```sh
cpm update            # project: fetch the latest stable CPM.cmake into [paths] scripts
cpm update --check    # show current vs latest without changing anything
cpm update -g         # global: bump the version bundled in this tool's own source tree
```

`cpm update` reads `scripts` from `.cpm` and writes `CPM.cmake` there. Commit it,
and the project's CPM.cmake is vendored and version-controlled.

---

## Command reference

### Acquiring & inspecting deps (global pantry)

| Command                      | What it does                                                        |
|------------------------------|---------------------------------------------------------------------|
| `cpm add <name> <url> <tag>` | Acquire sources (`--kind auto\|git\|fetch`), pack into `$CPM_PRELOAD`, register |
| `cpm import [--force]`       | Register archives already in `$CPM_PRELOAD` (loc-only); `-f` re-fetches from source |
| `cpm fetch [--force]`        | Re-acquire every pantry dep from its git source (rebuild archives)  |
| `cpm list`                   | Registered deps + archive status (adapts to terminal width)         |
| `cpm info <name>`            | Per-version summary: git upstream + local archive (`git remote -v` analogue) |
| `cpm show <name> [--hash]`   | Print a ready-to-paste `CPMAddPackage(...)` snippet                 |
| `cpm verify <target>`        | Inspect an archive (name / filename / path)                         |
| `cpm source add <name> <url> <tag>` | Attach a git upstream to an entry (no fetch)                |
| `cpm source rm <name>`       | Detach the git source (entry stays loc-only)                        |

### Managing a project's deps (writes `deps.toml` + regenerates)

| Command                              | What it does                                                        |
|--------------------------------------|---------------------------------------------------------------------|
| `cpm requires add <name> [spec]`     | Add a dep: resolve vs pantry, write `[dep.X]`, regenerate           |
| `cpm requires rm <name>`             | Drop a dep from `deps.toml` (and regenerate)                        |
| `cpm requires list`                  | Show required deps with resolved versions                           |
| `cpm requires bump <name> [spec]`    | Change **only** the version, preserving all other settings          |
| `cpm generate [project]`             | Emit `Find<Name>.cmake` + engine registrations from `deps.toml`     |

### Project & environment

| Command                      | What it does                                                        |
|------------------------------|---------------------------------------------------------------------|
| `cpm init [project]`         | Scaffold the 3rdparty module + `.cpm` (`--dir/--scripts/--patches/--force`) |
| `cpm update [--check] [-g]`  | Refresh vendored CPM.cmake (project) or the tool's bundle (`-g`)    |
| `cpm bootstrap [--latest]`   | Download CPM.cmake + get_cpm.cmake into `$CPM_HOME`                 |
| `cpm env [--export]`         | Show resolved paths / print shell exports                           |

`<spec>` grammar (for `requires`/`bump`): bare `1.90.0` = exact pin; `^1.85`,
`~1.90.0`, `>=1.85 <2.0` = constraint (re-resolved at generate); `*`/omitted =
freshest.

---

## What gets committed vs. what's tool-side

**Commit these (the self-sufficient set):**

```
.cpm                                            # root anchor for cpm
build/cmake/modules/3rdparty/
  3rdparty.cmake        # engine + static registrations
  deps.toml             # manifest (source for generation)
  get_cpm.cmake         # fetches CPM.cmake once into the source cache
  Find<Name>.cmake      # one per dependency
  pre_<key>.cmake       # optional pre-hooks
  post_<key>.cmake      # optional post-hooks
build/cmake/scripts/CPM.cmake   # vendored CPM.cmake (via cpm update)
build/patches/                                  # patch files
```

**Tool-side only (never read at configure time):**

- the `cpm` binary
- `$CPM_HOME/deps.toml` — the global pantry
- `$CPM_PRELOAD/` — the archive store

Clone the repo, run CMake — it builds. `cpm` is only needed to *change* the
dependency set.

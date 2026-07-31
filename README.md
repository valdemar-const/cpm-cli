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

### 2. Pre-build archives for offline-first deps

For any dependency you want available offline, add it to the shared pantry once
(it's cloned shallowly, VCS metadata stripped, packed into a zip, hashed):

```sh
cpm add fmt https://github.com/fmtlib/fmt.git 10.2.1
cpm add glm https://github.com/g-truc/glm.git 0.9.9.8
```

`cpm list` shows what's registered; `cpm verify fmt` inspects an archive.

### 3. Declare dependencies in `deps.toml`

`build/cmake/modules/3rdparty/deps.toml` is your per-project manifest. It
**overlays** the global pantry — declare a dep by pantry key, or fully describe
one that only lives in this project:

```toml
project      = "myapp"
network_when = "CPM_DOWNLOAD"      # CMake predicate gating the network tiers

# pulled from the pantry as-is (local archive + git/tar tiers baked in)
deps = ["fmt", "glm"]

# declared only here: git tier, pinned tag
[dep.freetype]
package = "Freetype"
git     = "https://github.com/freetype/freetype.git"
tag     = "VER-2-14-1"
version = "2.14.1"

# tarball tier (no git)
[dep.glad]
package       = "Glad"
git           = false
tarball       = "https://github.com/Dav1dde/glad/archive/refs/tags/v2.0.8.zip"
source_subdir = "cmake"
options       = ["DOWNLOAD_ONLY"]
post = '''if(Glad_ADDED)
  list(APPEND CMAKE_MODULE_PATH ${Glad_SOURCE_DIR}/cmake)
endif()'''
```

`network_when` is any CMake variable that is true when network is allowed.
Define a matching option in your `CMakeLists.txt`:

```cmake
option(CPM_DOWNLOAD "Allow network during configure (git/tar tiers)" OFF)
```

### 4. Generate the CMake glue

```sh
cpm generate
```

This emits a `Find<Package>.cmake` for each dependency and rewrites
`3rdparty.cmake` with the static registrations synthesised from `deps.toml` +
pantry. **From this point the project is self-sufficient.**

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

That's the whole loop: **`cpm add` / edit `deps.toml` → `cpm generate` → build.**

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

| Command                      | What it does                                                        |
|------------------------------|---------------------------------------------------------------------|
| `cpm init [project]`         | Scaffold the 3rdparty module + `.cpm` (non-destructive; `--dir/--scripts/--patches/--force`) |
| `cpm add <name> <url> <tag>` | Clone, strip VCS, pack into `$CPM_PRELOAD`, register in pantry      |
| `cpm list`                   | Show registered deps and archive status                             |
| `cpm fetch [--force]`        | Build any missing archives from the pantry                          |
| `cpm show <name> [--hash]`   | Print a ready-to-paste `CPMAddPackage(...)` snippet                 |
| `cpm verify <target>`        | Inspect an archive (name / filename / path)                         |
| `cpm generate [project]`     | Emit `Find<Name>.cmake` + engine registrations from `deps.toml`     |
| `cpm update [--check] [-g]`  | Refresh vendored CPM.cmake (project) or the tool's bundle (`-g`)    |
| `cpm bootstrap [--latest]`   | Download CPM.cmake + get_cpm.cmake into `$CPM_HOME`                 |
| `cpm env [--export]`         | Show resolved paths / print shell exports                           |

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

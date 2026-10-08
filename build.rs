//! Build script: embeds the build's git description and keeps the embedded `assets/`
//! directory in sync in release builds.
//!
//! # Build id (`presenter --version`)
//!
//! Sets `PRESENTER_GIT_SHA` for the crate, which `cli::version()` appends to the version:
//! `presenter 0.1.0 (<build id>)`. The build id is, in this order:
//!
//! 1. the `PRESENTER_GIT_SHA` environment variable, when set and not blank (for builds from
//!    a source archive, or packagers who know the revision);
//! 2. what `git describe --always --dirty` prints for the crate directory: the abbreviated
//!    commit (`33a4607`), or the annotated tag at or before it (`v0.1.0`,
//!    `v0.1.0-3-g33a4607`), plus `-dirty` when tracked files differ from the commit;
//! 3. `unknown` when git is not installed, the crate directory is not the root of a git
//!    checkout, or git fails. The build never fails because of git, but a checkout that git
//!    cannot describe gets a Cargo warning.
//!
//! A build id contains only ASCII letters, digits, `.`, `+` and `-`. Other characters, as
//! in the tag `release/v1_0` or the override `0.1.0~rc1`, become `-`, with a Cargo warning.
//! `PRESENTER_BUILD_ID_SOURCE` (`override`, `git` or `unknown`) tells the crate's tests
//! which of the three applied.
//!
//! The dirty check runs `git --no-optional-locks status` instead of `describe --dirty`,
//! which rewrites `.git/index` as a side effect; the output is the same. Git discovery stops
//! at the crate directory, so a source archive unpacked inside some other repository is
//! `unknown`, not that repository's commit.
//!
//! # When the script reruns
//!
//! A rerun recompiles the crate, so the script watches what can change the build id and
//! nothing else. It reruns when
//!
//! - `src/`, `Cargo.toml`, `Cargo.lock` or `build.rs` change (and `assets/` in release
//!   builds, see below): the edits that recompile the crate anyway, so `-dirty` is checked
//!   again whenever the binary changes;
//! - `HEAD` or its reflog `logs/HEAD` change (commit, checkout, reset, ...);
//! - the checked-out branch changes: its loose ref file, or, while it is packed (`git gc`
//!   runs `git pack-refs`), `packed-refs` and the directory the next commit writes the
//!   loose ref to;
//! - the tags change (`refs/tags`, `packed-refs`);
//! - in a reftable repository, the worktree's or the shared reftable stack changes;
//! - `PRESENTER_GIT_SHA` changes.
//!
//! Git paths come from `git rev-parse --git-path`, which knows linked worktrees. Edits to
//! other tracked files (tests, docs) do not rerun the script; they do not change the
//! binary, and its `-dirty` reflects them as of the last rerun.
//!
//! # Embedded assets
//!
//! `src/vendor.rs` embeds `assets/` with rust-embed. With debug assertions off it expands
//! to one `include_bytes!` per file that existed at expansion time, so Cargo notices edits
//! to those files but not files that were added or removed. Watching the directory makes
//! Cargo rebuild the crate in that case. Debug builds read `assets/` from disk at runtime
//! and need no rebuild.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The variable `cli::version()` reads at compile time, and the environment override.
const BUILD_ID_VAR: &str = "PRESENTER_GIT_SHA";

/// Where the build id came from, for the crate's tests: [`Source::as_str`].
const SOURCE_VAR: &str = "PRESENTER_BUILD_ID_SOURCE";

/// Build id when nothing better is known.
const UNKNOWN: &str = "unknown";

/// Crate files and directories (relative to the crate directory) whose changes recompile
/// the crate in every profile.
const CRATE_INPUTS: [&str; 4] = ["build.rs", "Cargo.toml", "Cargo.lock", "src"];

/// Paths in the git directory watched when they exist; `git rev-parse --git-path` maps them
/// to the worktree's own (`HEAD`, `logs/HEAD`, `reftable`) or the shared directory.
const GIT_PATHS: [&str; 5] = ["HEAD", "logs/HEAD", "packed-refs", "refs/tags", "reftable"];

fn main() {
    // Cargo always sets CARGO_MANIFEST_DIR for build scripts, and runs them in that
    // directory, so "." is only a formality.
    let crate_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_else(|| ".".into()));
    // Set by Cargo for the target configuration: present exactly when rust-embed reads
    // from disk instead of embedding.
    let embeds_assets = std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_none();
    for input in crate_inputs(&crate_dir, embeds_assets) {
        println!("cargo::rerun-if-changed={input}");
    }

    let build = build_id(
        std::env::var(BUILD_ID_VAR).ok().as_deref(),
        "git",
        &crate_dir,
    );
    println!("cargo::rerun-if-env-changed={BUILD_ID_VAR}");
    for path in &build.watch {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    for warning in &build.warnings {
        println!("cargo::warning={warning}");
    }
    println!("cargo::rustc-env={BUILD_ID_VAR}={}", build.id);
    println!("cargo::rustc-env={SOURCE_VAR}={}", build.source.as_str());
}

/// The crate's own inputs to watch, relative to `crate_dir`: [`CRATE_INPUTS`], and
/// `assets` when the build embeds it, as far as they exist. A directory is scanned
/// recursively, so new and deleted files count as changes; a path that does not exist
/// would make Cargo rerun the script on every build.
pub(crate) fn crate_inputs(crate_dir: &Path, embeds_assets: bool) -> Vec<&'static str> {
    CRATE_INPUTS
        .into_iter()
        .chain(embeds_assets.then_some("assets"))
        .filter(|input| crate_dir.join(input).exists())
        .collect()
}

/// Where a build id came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The `PRESENTER_GIT_SHA` environment variable.
    Override,
    /// `git describe`.
    Git,
    /// Neither: the id is `unknown`.
    Unknown,
}

impl Source {
    /// The value of `PRESENTER_BUILD_ID_SOURCE`: `override`, `git` or `unknown`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Source::Override => "override",
            Source::Git => "git",
            Source::Unknown => UNKNOWN,
        }
    }
}

/// The build id, where it came from, the git files whose changes make it stale, and
/// warnings for Cargo to show.
#[derive(Debug)]
pub(crate) struct BuildId {
    /// What `presenter --version` shows in parentheses.
    pub(crate) id: String,
    /// Where `id` came from.
    pub(crate) source: Source,
    /// Existing files and directories to watch with `rerun-if-changed`.
    pub(crate) watch: Vec<PathBuf>,
    /// One-line messages for `cargo::warning`.
    pub(crate) warnings: Vec<String>,
}

impl BuildId {
    /// A build id from `raw`, with the characters a build id may not contain replaced.
    fn new(raw: &str, source: Source, watch: Vec<PathBuf>) -> Self {
        let id = sanitize(raw);
        let mut warnings = Vec::new();
        if id != raw {
            warnings.push(format!(
                "build id {raw:?} may contain only ASCII letters, digits, '.', '+' and '-'; \
                 using {id:?}"
            ));
        }
        BuildId {
            id,
            source,
            watch,
            warnings,
        }
    }
}

/// The build id for the crate in `crate_dir`: `env_override` (the first line, trimmed) when
/// it is not blank, else `git describe` run with the program `git`, else `unknown`.
pub(crate) fn build_id(env_override: Option<&str>, git: &str, crate_dir: &Path) -> BuildId {
    if let Some(id) = env_override.and_then(first_line) {
        return BuildId::new(id, Source::Override, Vec::new());
    }
    if let Some(id) = describe(git, crate_dir) {
        return BuildId::new(&id, Source::Git, watched_git_paths(git, crate_dir));
    }
    let mut build = BuildId::new(UNKNOWN, Source::Unknown, Vec::new());
    if crate_dir.join(".git").exists() {
        build.warnings.push(format!(
            "{} is a git checkout, but `{git} describe` failed (is git installed, is there a \
             commit, and does git trust the directory's owner?); the build id is `{UNKNOWN}`. \
             Set {BUILD_ID_VAR} to name the build.",
            crate_dir.display()
        ));
    }
    build
}

/// `id` with every character other than an ASCII letter, digit, `.`, `+` or `-` replaced by
/// `-` (the charset `tests/cli_version.rs` accepts).
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The output of `git describe --always --dirty` for the checkout rooted at `crate_dir`,
/// computed without writing to the repository.
fn describe(git: &str, crate_dir: &Path) -> Option<String> {
    let described = run_git(git, crate_dir, &["describe", "--always"])?;
    let mut id = first_line(&described)?.to_string();
    // Tracked files only (staged or not), like `describe --dirty`.
    let status = run_git(
        git,
        crate_dir,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=no",
        ],
    )?;
    if !status.trim().is_empty() {
        id.push_str("-dirty");
    }
    Some(id)
}

/// The existing git files and directories whose changes can change the build id: see the
/// module docs. A path that does not exist would make Cargo rerun the script every time.
fn watched_git_paths(git: &str, crate_dir: &Path) -> Vec<PathBuf> {
    // Fails (exit 1) on a detached HEAD, which has no branch to watch.
    let branch = run_git(git, crate_dir, &["symbolic-ref", "-q", "HEAD"]);
    let branch = branch.as_deref().and_then(first_line);
    let mut args = vec!["rev-parse", "--git-common-dir"];
    for name in GIT_PATHS.into_iter().chain(branch) {
        args.extend(["--git-path", name]);
    }
    let Some(output) = run_git(git, crate_dir, &args) else {
        return Vec::new();
    };
    // Relative paths are relative to the working directory, `crate_dir`.
    let mut lines = output.lines().map(|line| crate_dir.join(line));
    let Some(common_dir) = lines.next() else {
        return Vec::new();
    };
    let mut candidates: Vec<PathBuf> = lines.by_ref().take(GIT_PATHS.len()).collect();
    // The shared reftable stack holds the branches; in a linked worktree `--git-path
    // reftable` is the worktree's own stack, which holds its HEAD.
    let shared_reftable = common_dir.join("reftable");
    let uses_reftable = shared_reftable.is_dir();
    candidates.push(shared_reftable);
    if let Some(branch_ref) = lines.next().filter(|_| !uses_reftable) {
        candidates.extend(loose_ref_watch(branch_ref, &common_dir));
    }
    let mut watch = Vec::new();
    for path in candidates {
        if path.exists() && !watch.contains(&path) {
            watch.push(path);
        }
    }
    watch
}

/// What to watch for a branch's loose ref file `path` in the files backend: the file, or,
/// when the branch is packed (it is then only in `packed-refs`), the nearest existing
/// directory below `common_dir` on its path, where the next commit creates the file.
fn loose_ref_watch(path: PathBuf, common_dir: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path);
    }
    path.ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(common_dir) && *dir != common_dir)
        .find(|dir| dir.is_dir())
        .map(Path::to_path_buf)
}

/// Runs `git args` in `crate_dir` and returns its stdout, or `None` when git cannot be
/// started, fails, or prints something that is not UTF-8. Git looks for a repository in
/// `crate_dir` only, and ignores `GIT_DIR`-style variables of the caller (set, e.g., while
/// a git hook runs `cargo`).
fn run_git(git: &str, crate_dir: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new(git);
    command
        .args(args)
        .current_dir(crate_dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR");
    if let Some(parent) = crate_dir.parent() {
        command.env("GIT_CEILING_DIRECTORIES", parent);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// The first line of `text`, trimmed, unless it is empty.
fn first_line(text: &str) -> Option<&str> {
    Some(text.lines().next()?.trim()).filter(|line| !line.is_empty())
}

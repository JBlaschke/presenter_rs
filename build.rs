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
//!    checkout, or git fails. The build never fails because of git.
//!
//! The dirty check runs `git --no-optional-locks status` instead of `describe --dirty`,
//! which rewrites `.git/index` as a side effect; the output is the same. Git discovery stops
//! at the crate directory, so a source archive unpacked inside some other repository is
//! `unknown`, not that repository's commit.
//!
//! The script reruns when `HEAD`, the checked-out branch, `packed-refs` or the tags change
//! (and when `PRESENTER_GIT_SHA` changes), not on every edit of a tracked file: `-dirty`
//! reflects the tree when the script last ran. Fresh checkouts (CI, releases) are exact.
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

/// Build id when nothing better is known.
const UNKNOWN: &str = "unknown";

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // Set by Cargo for the target configuration: present exactly when rust-embed reads
    // from disk instead of embedding.
    if std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_none() {
        // A directory is scanned recursively, so new and deleted files count as changes.
        println!("cargo::rerun-if-changed=assets");
    }

    // Cargo always sets CARGO_MANIFEST_DIR for build scripts, and runs them in that
    // directory, so "." is only a formality.
    let crate_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_else(|| ".".into()));
    let build = build_id(
        std::env::var(BUILD_ID_VAR).ok().as_deref(),
        "git",
        &crate_dir,
    );
    println!("cargo::rerun-if-env-changed={BUILD_ID_VAR}");
    for path in &build.watch {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    println!("cargo::rustc-env={BUILD_ID_VAR}={}", build.id);
}

/// The build id and the git files whose changes make it stale.
#[derive(Debug)]
pub(crate) struct BuildId {
    /// What `presenter --version` shows in parentheses.
    pub(crate) id: String,
    /// Existing files and directories to watch with `rerun-if-changed`.
    pub(crate) watch: Vec<PathBuf>,
}

/// The build id for the crate in `crate_dir`: `env_override` (the first line, trimmed) when
/// it is not blank, else `git describe` run with the program `git`, else `unknown`.
pub(crate) fn build_id(env_override: Option<&str>, git: &str, crate_dir: &Path) -> BuildId {
    if let Some(id) = env_override.and_then(first_line) {
        return BuildId {
            id: id.to_string(),
            watch: Vec::new(),
        };
    }
    match describe(git, crate_dir) {
        Some(id) => BuildId {
            id,
            watch: watched_git_paths(git, crate_dir),
        },
        None => BuildId {
            id: UNKNOWN.to_string(),
            watch: Vec::new(),
        },
    }
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

/// `HEAD`, the checked-out branch's ref, `packed-refs` and the tags directory, as far as
/// they exist (a path that does not exist would make Cargo rerun the script every time).
/// Paths come from `git rev-parse --git-path`, which knows linked worktrees.
fn watched_git_paths(git: &str, crate_dir: &Path) -> Vec<PathBuf> {
    let mut names = vec![
        "HEAD".to_string(),
        "packed-refs".to_string(),
        "refs/tags".to_string(),
    ];
    // Fails (exit 1) on a detached HEAD, which has no branch to watch.
    if let Some(branch) = run_git(git, crate_dir, &["symbolic-ref", "-q", "HEAD"])
        .as_deref()
        .and_then(first_line)
    {
        names.push(branch.to_string());
    }
    let mut args = vec!["rev-parse"];
    for name in &names {
        args.extend(["--git-path", name.as_str()]);
    }
    let Some(paths) = run_git(git, crate_dir, &args) else {
        return Vec::new();
    };
    paths
        .lines()
        .map(|path| crate_dir.join(path))
        .filter(|path| path.exists())
        .collect()
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
        .env_remove("GIT_INDEX_FILE");
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

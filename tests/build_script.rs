//! T47: the build id logic of `build.rs` (compiled here as a module): the
//! `PRESENTER_GIT_SHA` override, the `unknown` fallback without git or outside a checkout,
//! `git describe --always --dirty` semantics on a scratch repository, the build id charset,
//! and the git paths watched so that the id cannot go stale (loose and packed refs,
//! reftable, linked worktrees).
//!
//! Tests that need git skip themselves (with a note on stderr) when it is not installed.

use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)] // `main` and the constants are the build script's own.
#[path = "../build.rs"]
mod build_script;

use build_script::{Source, build_id, crate_inputs};

/// A program name that cannot exist on `PATH`.
const NO_GIT: &str = "presenter-test-no-such-git";

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn git_installed() -> bool {
    let found = Command::new("git").arg("--version").output().is_ok();
    if !found {
        eprintln!("git not installed; skipping");
    }
    found
}

/// A git command in `dir` without the caller's `GIT_DIR`-style variables.
fn git_command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR");
    command
}

/// Runs git in `dir` to set up a scratch repository, isolated from the user's and the
/// system's git configuration (signing, hooks, default branch); returns its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    try_git(dir, args).unwrap_or_else(|| panic!("git {args:?} failed in {}", dir.display()))
}

/// [`git`], or `None` when the command fails.
fn try_git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut command = git_command(dir, &[]);
    let output = command
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).unwrap().trim().to_string())
}

/// The abbreviated commit, computed with the configuration `build_id` sees (the user's
/// `core.abbrev`, for example), not the isolated one of [`git`].
fn short_sha(dir: &Path) -> String {
    let output = git_command(dir, &["rev-parse", "--short", "HEAD"])
        .output()
        .expect("git runs");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// A scratch repository (`git init` with `init_args`) with one commit of `a.txt`.
fn scratch_repo_with(init_args: &[&str]) -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    let mut args = vec!["init", "-q", "-b", "main"];
    args.extend(init_args);
    try_git(dir.path(), &args)?;
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    git(dir.path(), &["add", "a.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "first"]);
    Some(dir)
}

/// A scratch repository with one commit of `a.txt`; returns it and the abbreviated sha.
fn scratch_repo() -> (tempfile::TempDir, String) {
    let dir = scratch_repo_with(&[]).expect("git init works");
    let sha = short_sha(dir.path());
    (dir, sha)
}

/// The watched paths, canonicalized (git prints some paths through symlinks such as
/// macOS's `/var`, others not).
fn canonical_watch(watch: &[PathBuf]) -> Vec<PathBuf> {
    watch
        .iter()
        .map(|path| path.canonicalize().unwrap())
        .collect()
}

/// Whether `path` is watched itself or lies in a watched directory (Cargo scans watched
/// directories recursively).
fn covered(watch: &[PathBuf], path: &Path) -> bool {
    let path = path.canonicalize().unwrap();
    canonical_watch(watch)
        .iter()
        .any(|watched| path.starts_with(watched))
}

/// Asserts that every watched path exists: a missing one would make Cargo rerun the script
/// (and recompile the crate) on every build.
fn assert_all_exist(watch: &[PathBuf]) {
    assert!(watch.iter().all(|path| path.exists()), "{watch:?}");
}

/// The script reruns when the crate's sources change, so a build from edited sources is
/// `-dirty` (and one from reverted sources is not); in release builds also when `assets/`
/// changes. Missing inputs are not watched.
#[test]
fn crate_sources_are_watched() {
    let inputs = crate_inputs(crate_dir(), false);
    assert_eq!(inputs, ["build.rs", "Cargo.toml", "Cargo.lock", "src"]);
    let inputs = crate_inputs(crate_dir(), true);
    assert_eq!(
        inputs,
        ["build.rs", "Cargo.toml", "Cargo.lock", "src", "assets"]
    );

    let dir = tempfile::tempdir().unwrap();
    assert!(crate_inputs(dir.path(), true).is_empty());
    std::fs::create_dir(dir.path().join("src")).unwrap();
    assert_eq!(crate_inputs(dir.path(), true), ["src"]);
}

#[test]
fn override_wins_and_is_trimmed() {
    let build = build_id(Some("  v1.2.3-custom \nignored\n"), NO_GIT, crate_dir());
    assert_eq!(build.id, "v1.2.3-custom");
    assert_eq!(build.source, Source::Override);
    assert!(build.watch.is_empty(), "{:?}", build.watch);
    assert!(build.warnings.is_empty(), "{:?}", build.warnings);
}

/// One charset for every build id, the one `tests/cli_version.rs` accepts: a Debian-style
/// revision or a space in the override, or `_` and `/` in a tag, become `-`, with a warning.
#[test]
fn other_characters_become_dashes() {
    let build = build_id(Some("0.1.0-1~deb12u1 build_7/x+ä"), NO_GIT, crate_dir());
    assert_eq!(build.id, "0.1.0-1-deb12u1-build-7-x+-");
    assert_eq!(build.source, Source::Override);
    assert_eq!(build.warnings.len(), 1, "{:?}", build.warnings);
    assert!(
        build.warnings[0].contains("\"0.1.0-1-deb12u1-build-7-x+-\""),
        "{:?}",
        build.warnings
    );
    assert!(!build.warnings[0].contains('\n'));
}

#[test]
fn blank_override_is_ignored() {
    for blank in ["", "  ", "\n"] {
        let build = build_id(Some(blank), NO_GIT, crate_dir());
        assert_eq!(build.id, "unknown", "{blank:?}");
        assert_eq!(build.source, Source::Unknown, "{blank:?}");
    }
}

/// Without git the id is `unknown`; in a checkout (a `.git` entry) Cargo also shows a
/// warning, because the fallback is then a surprise.
#[test]
fn without_git_the_id_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let build = build_id(None, NO_GIT, dir.path());
    assert_eq!(build.id, "unknown");
    assert_eq!(build.source, Source::Unknown);
    assert!(build.watch.is_empty(), "{:?}", build.watch);
    assert!(build.warnings.is_empty(), "{:?}", build.warnings);

    std::fs::create_dir(dir.path().join(".git")).unwrap();
    let build = build_id(None, NO_GIT, dir.path());
    assert_eq!(build.id, "unknown");
    assert_eq!(build.warnings.len(), 1, "{:?}", build.warnings);
    assert!(
        build.warnings[0].contains("PRESENTER_GIT_SHA"),
        "{:?}",
        build.warnings
    );
    assert!(!build.warnings[0].contains('\n'));
}

#[test]
fn outside_a_checkout_the_id_is_unknown() {
    if !git_installed() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let build = build_id(None, "git", dir.path());
    assert_eq!(build.id, "unknown");
    assert_eq!(build.source, Source::Unknown);
    assert!(build.watch.is_empty(), "{:?}", build.watch);
    assert!(build.warnings.is_empty(), "{:?}", build.warnings);
}

/// A crate directory below the repository root (a source archive unpacked inside some
/// other repository) is not described with that repository's commit.
#[test]
fn a_parent_repository_is_not_used() {
    if !git_installed() {
        return;
    }
    let (repo, _) = scratch_repo();
    let nested = repo.path().join("vendor/presenter");
    std::fs::create_dir_all(&nested).unwrap();
    assert_eq!(build_id(None, "git", &nested).id, "unknown");
}

#[test]
fn commit_dirty_and_tag_forms() {
    if !git_installed() {
        return;
    }
    let (repo, sha) = scratch_repo();
    let dir = repo.path();
    let build = build_id(None, "git", dir);
    assert_eq!(build.id, sha);
    assert_eq!(build.source, Source::Git);
    assert!(build.warnings.is_empty(), "{:?}", build.warnings);

    // Untracked files do not count; modified tracked files do, staged or not.
    std::fs::write(dir.join("new.txt"), "untracked\n").unwrap();
    assert_eq!(build_id(None, "git", dir).id, sha);
    std::fs::write(dir.join("a.txt"), "two\n").unwrap();
    assert_eq!(build_id(None, "git", dir).id, format!("{sha}-dirty"));
    git(dir, &["add", "a.txt"]);
    assert_eq!(build_id(None, "git", dir).id, format!("{sha}-dirty"));
    git(dir, &["commit", "-q", "-m", "second"]);
    let second = short_sha(dir);
    assert_eq!(build_id(None, "git", dir).id, second);

    // An annotated tag at HEAD (what docs/dev/release.md asks for) names the build.
    git(dir, &["tag", "-a", "v9.9.9", "-m", "release"]);
    assert_eq!(build_id(None, "git", dir).id, "v9.9.9");
    std::fs::write(dir.join("a.txt"), "three\n").unwrap();
    assert_eq!(build_id(None, "git", dir).id, "v9.9.9-dirty");

    // A tag name with characters outside the build id charset.
    git(dir, &["checkout", "-q", "--", "a.txt"]);
    git(dir, &["tag", "-a", "release/v9_9", "-m", "release", "HEAD"]);
    git(dir, &["tag", "-d", "v9.9.9"]);
    let build = build_id(None, "git", dir);
    assert_eq!(build.id, "release-v9-9");
    assert_eq!(build.warnings.len(), 1, "{:?}", build.warnings);
}

#[test]
fn describe_leaves_the_index_alone() {
    if !git_installed() {
        return;
    }
    let (repo, _) = scratch_repo();
    let index = repo.path().join(".git/index");
    let before = std::fs::read(&index).unwrap();
    // A changed mtime with unchanged content makes `git describe --dirty` (and a plain
    // `git status`) rewrite the index with the refreshed stat data.
    let an_hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(repo.path().join("a.txt"))
        .unwrap()
        .set_modified(an_hour_ago)
        .unwrap();
    let build = build_id(None, "git", repo.path());
    assert!(!build.id.ends_with("-dirty"), "{}", build.id);
    assert_eq!(std::fs::read(&index).unwrap(), before);
}

#[test]
fn head_branch_and_tags_are_watched() {
    if !git_installed() {
        return;
    }
    let (repo, _) = scratch_repo();
    let git_dir = repo.path().join(".git");
    let build = build_id(None, "git", repo.path());
    for expected in ["HEAD", "logs/HEAD", "refs/heads/main", "refs/tags"] {
        assert!(
            build.watch.contains(&git_dir.join(expected)),
            "{expected}: {:?}",
            build.watch
        );
    }
    // Not the whole branch directory: commits on other branches (in other worktrees) do
    // not change this build's id.
    assert!(!build.watch.contains(&git_dir.join("refs/heads")));
    assert_all_exist(&build.watch);

    // A detached HEAD has no branch to watch.
    git(repo.path(), &["checkout", "-q", "--detach"]);
    let build = build_id(None, "git", repo.path());
    assert!(build.watch.contains(&git_dir.join("HEAD")));
    assert!(!build.watch.contains(&git_dir.join("refs/heads/main")));
}

/// After `git pack-refs` (which `git gc` runs) the branch is only in `packed-refs`; the
/// next commit writes a new loose ref and appends to the HEAD reflog, but rewrites neither
/// `HEAD` nor `packed-refs`. Something watched must still change.
#[test]
fn a_commit_on_a_packed_branch_changes_a_watched_path() {
    if !git_installed() {
        return;
    }
    let (repo, _) = scratch_repo();
    let dir = repo.path();
    let git_dir = dir.join(".git");
    let loose = git_dir.join("refs/heads/main");
    let reflog = git_dir.join("logs/HEAD");
    git(dir, &["pack-refs", "--all"]);
    assert!(!loose.exists());

    let build = build_id(None, "git", dir);
    assert_all_exist(&build.watch);
    assert!(build.watch.contains(&git_dir.join("packed-refs")));
    assert!(build.watch.contains(&reflog), "{:?}", build.watch);
    let reflog_before = std::fs::read(&reflog).unwrap();

    git(dir, &["commit", "-q", "--allow-empty", "-m", "second"]);
    // The new loose ref lies in a watched directory, and the watched reflog grew.
    assert!(covered(&build.watch, &loose), "{:?}", build.watch);
    assert!(std::fs::read(&reflog).unwrap().len() > reflog_before.len());
    // With reflogs off, the directory alone still catches it.
    git(dir, &["config", "core.logAllRefUpdates", "false"]);
    git(dir, &["pack-refs", "--all"]);
    std::fs::remove_file(&reflog).unwrap();
    let build = build_id(None, "git", dir);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "third"]);
    assert!(covered(&build.watch, &loose), "{:?}", build.watch);

    // Once the loose ref exists again, the script watches the file, not the directory.
    let build = build_id(None, "git", dir);
    assert!(build.watch.contains(&loose), "{:?}", build.watch);
    assert!(!build.watch.contains(&git_dir.join("refs/heads")));
    assert_eq!(build.id, short_sha(dir));
}

/// In a reftable repository (`git init --ref-format=reftable`, Git 2.45+) `HEAD` is a stub
/// and the branches live in the reftable stack, which every commit rewrites.
#[test]
fn a_reftable_repository_watches_the_stack() {
    if !git_installed() {
        return;
    }
    let Some(repo) = scratch_repo_with(&["--ref-format=reftable"]) else {
        eprintln!("git cannot create reftable repositories; skipping");
        return;
    };
    let dir = repo.path();
    let tables = dir.join(".git/reftable/tables.list");
    let build = build_id(None, "git", dir);
    assert_eq!(build.id, short_sha(dir));
    assert_all_exist(&build.watch);
    assert!(covered(&build.watch, &tables), "{:?}", build.watch);
    // Not the stub `refs/heads` file or its directory.
    assert!(
        !covered(&build.watch, &dir.join(".git/refs/heads")),
        "{:?}",
        build.watch
    );

    let before = std::fs::read(&tables).unwrap();
    git(dir, &["commit", "-q", "--allow-empty", "-m", "second"]);
    assert_ne!(std::fs::read(&tables).unwrap(), before);
}

/// A linked worktree of a reftable repository has its own stack, which holds its `HEAD`;
/// its branch is in the shared stack, which `--git-path reftable` does not name there.
#[test]
fn a_reftable_worktree_watches_the_shared_stack() {
    if !git_installed() {
        return;
    }
    let Some(repo) = scratch_repo_with(&["--ref-format=reftable"]) else {
        eprintln!("git cannot create reftable repositories; skipping");
        return;
    };
    git(
        repo.path(),
        &["worktree", "add", "-q", "-b", "feature", "linked"],
    );
    let worktree = repo.path().join("linked");
    let shared = repo.path().join(".git/reftable/tables.list");
    let own = repo
        .path()
        .join(".git/worktrees/linked/reftable/tables.list");
    let build = build_id(None, "git", &worktree);
    assert_eq!(build.id, short_sha(&worktree));
    assert_all_exist(&build.watch);
    assert!(covered(&build.watch, &own), "{:?}", build.watch);
    assert!(covered(&build.watch, &shared), "{:?}", build.watch);

    // A commit in the worktree moves `feature`, in the shared stack.
    let before = std::fs::read(&shared).unwrap();
    git(
        &worktree,
        &["commit", "-q", "--allow-empty", "-m", "second"],
    );
    assert_ne!(std::fs::read(&shared).unwrap(), before);
}

/// A linked worktree (as `git worktree add` makes) has its own `HEAD` and reflog; its
/// branch ref is in the shared git directory.
#[test]
fn a_linked_worktree_watches_its_own_head() {
    if !git_installed() {
        return;
    }
    let (repo, _) = scratch_repo();
    let worktree = repo.path().join("linked");
    git(
        repo.path(),
        &["worktree", "add", "-q", "-b", "feature", "linked"],
    );
    let build = build_id(None, "git", &worktree);
    assert_eq!(build.id, short_sha(&worktree));
    assert_all_exist(&build.watch);
    let git_dir = repo.path().join(".git");
    let watch = canonical_watch(&build.watch);
    for expected in [
        "worktrees/linked/HEAD",
        "worktrees/linked/logs/HEAD",
        "refs/heads/feature",
        "refs/tags",
    ] {
        let expected = git_dir.join(expected).canonicalize().unwrap();
        assert!(watch.contains(&expected), "{expected:?}: {watch:?}");
    }
    // Not the main worktree's HEAD or branch.
    for other in ["HEAD", "refs/heads/main"] {
        let other = git_dir.join(other).canonicalize().unwrap();
        assert!(!watch.contains(&other), "{other:?}: {watch:?}");
    }
}

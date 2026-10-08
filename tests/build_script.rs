//! T47: the build id logic of `build.rs` (compiled here as a module): the
//! `PRESENTER_GIT_SHA` override, the `unknown` fallback without git or outside a checkout,
//! and `git describe --always --dirty` semantics on a scratch repository.
//!
//! Tests that need git skip themselves (with a note on stderr) when it is not installed.

use std::path::Path;
use std::process::Command;

#[allow(dead_code)] // `main` and the constants are the build script's own.
#[path = "../build.rs"]
mod build_script;

use build_script::build_id;

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

/// Runs git in `dir`, isolated from the user's and the system's git configuration.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// A scratch repository with one commit of `a.txt`; returns it and the abbreviated sha.
fn scratch_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    git(dir.path(), &["add", "a.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "first"]);
    let sha = git(dir.path(), &["rev-parse", "--short", "HEAD"]);
    (dir, sha)
}

#[test]
fn override_wins_and_is_trimmed() {
    let build = build_id(Some("  v1.2.3-custom \nignored\n"), NO_GIT, crate_dir());
    assert_eq!(build.id, "v1.2.3-custom");
    assert!(build.watch.is_empty(), "{:?}", build.watch);
}

#[test]
fn blank_override_is_ignored() {
    for blank in ["", "  ", "\n"] {
        let build = build_id(Some(blank), NO_GIT, crate_dir());
        assert_eq!(build.id, "unknown", "{blank:?}");
    }
}

#[test]
fn without_git_the_id_is_unknown() {
    let build = build_id(None, NO_GIT, crate_dir());
    assert_eq!(build.id, "unknown");
    assert!(build.watch.is_empty(), "{:?}", build.watch);
}

#[test]
fn outside_a_checkout_the_id_is_unknown() {
    if !git_installed() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let build = build_id(None, "git", dir.path());
    assert_eq!(build.id, "unknown");
    assert!(build.watch.is_empty(), "{:?}", build.watch);
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
    assert_eq!(build_id(None, "git", dir).id, sha);

    // Untracked files do not count; modified tracked files do, staged or not.
    std::fs::write(dir.join("new.txt"), "untracked\n").unwrap();
    assert_eq!(build_id(None, "git", dir).id, sha);
    std::fs::write(dir.join("a.txt"), "two\n").unwrap();
    assert_eq!(build_id(None, "git", dir).id, format!("{sha}-dirty"));
    git(dir, &["add", "a.txt"]);
    assert_eq!(build_id(None, "git", dir).id, format!("{sha}-dirty"));
    git(dir, &["commit", "-q", "-m", "second"]);
    let second = git(dir, &["rev-parse", "--short", "HEAD"]);
    assert_eq!(build_id(None, "git", dir).id, second);

    // An annotated tag at HEAD (what docs/dev/release.md asks for) names the build.
    git(dir, &["tag", "-a", "v9.9.9", "-m", "release"]);
    assert_eq!(build_id(None, "git", dir).id, "v9.9.9");
    std::fs::write(dir.join("a.txt"), "three\n").unwrap();
    assert_eq!(build_id(None, "git", dir).id, "v9.9.9-dirty");
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
    for expected in ["HEAD", "refs/heads/main", "refs/tags"] {
        assert!(
            build.watch.contains(&git_dir.join(expected)),
            "{expected}: {:?}",
            build.watch
        );
    }
    // Watched paths exist: a missing one would make Cargo rerun the script on every build.
    assert!(build.watch.iter().all(|path| path.exists()));

    // A detached HEAD has no branch to watch.
    git(repo.path(), &["checkout", "-q", "--detach"]);
    let build = build_id(None, "git", repo.path());
    assert!(build.watch.contains(&git_dir.join("HEAD")));
    assert!(!build.watch.contains(&git_dir.join("refs/heads/main")));
}

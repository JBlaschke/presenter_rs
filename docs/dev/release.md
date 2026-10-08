# Releasing presenter

A release is an annotated git tag `v<version>` on `main`. Pushing it runs
[`.github/workflows/release.yml`](../../.github/workflows/release.yml). The workflow tests
and builds `presenter` on four native runners and publishes a GitHub Release. The release
holds one archive per platform and a `SHA256SUMS` file.

| target | runner | archive |
|---|---|---|
| `aarch64-apple-darwin` (macOS arm64, macOS 11+) | `macos-14` | `presenter-v0.1.0-aarch64-apple-darwin.tar.gz` |
| `x86_64-apple-darwin` (macOS Intel, macOS 10.12+) | `macos-15-intel` | `presenter-v0.1.0-x86_64-apple-darwin.tar.gz` |
| `x86_64-unknown-linux-gnu` (glibc, built on Ubuntu 22.04) | `ubuntu-22.04` | `presenter-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` |
| `aarch64-unknown-linux-gnu` (glibc, built on Ubuntu 22.04) | `ubuntu-22.04-arm` | `presenter-v0.1.0-aarch64-unknown-linux-gnu.tar.gz` |

Each archive holds one directory with the archive's name. That directory contains the
`presenter` binary, `LICENSE`, `README.md` and `THIRD-PARTY-LICENSES/` (see
[Third-party licenses](#third-party-licenses)). The binary is self-contained: the themes,
reveal.js and KaTeX are embedded. It needs only the system's own libraries. On macOS those
are `libSystem` and `libiconv` (checked with `otool -L`). On Linux they are glibc and
`libgcc_s`, as for any Rust binary.

## Checklist

1. Check that `main` is green in CI (`.github/workflows/ci.yml`).
2. Set the new version in `Cargo.toml`, then run `cargo check`. The release builds with
   `--locked`, so `Cargo.lock` must already have the new version. Also update the version
   pinned by two tests: the `VERSION_LINE` regex in `tests/cli_version.rs`, and the
   `version_string` test in `src/cli/mod.rs`. `tests/cli_build.rs` reads the version from
   Cargo and needs no change.
3. In `CHANGELOG.md`, rename `## [Unreleased]` to `## [<version>] - <YYYY-MM-DD>`, and add
   a new empty `## [Unreleased]` section above it. The workflow publishes the version's
   section as the release notes: everything from its heading to the next `## ` heading.
4. Run `cargo test`, commit (`Release v<version>`) and merge to `main`.
5. Tag the merged commit with an **annotated** tag and push it:

   ```sh
   git tag -a v0.1.0 -m "presenter 0.1.0"
   git push origin v0.1.0
   ```

   The tag must be annotated. `git describe` ignores lightweight tags, so the binary would
   show a commit hash instead of `v0.1.0`. The workflow's first job rejects a lightweight
   tag before anything is built.
   A suffix such as `v0.2.0-rc.1` makes a pre-release. `Cargo.toml` must then say
   `0.2.0-rc.1`.
6. Watch the "Release" run on the Actions tab. When it succeeds, check the release page:
   four archives, `SHA256SUMS`, and the notes from the changelog. Then try one binary:

   ```sh
   curl -LO https://github.com/<owner>/<repo>/releases/download/v0.1.0/presenter-v0.1.0-aarch64-apple-darwin.tar.gz
   curl -LO https://github.com/<owner>/<repo>/releases/download/v0.1.0/SHA256SUMS
   shasum -a 256 -c --ignore-missing SHA256SUMS   # Linux: sha256sum -c --ignore-missing SHA256SUMS
   tar -xzf presenter-v0.1.0-aarch64-apple-darwin.tar.gz
   presenter-v0.1.0-aarch64-apple-darwin/presenter --version   # presenter 0.1.0 (v0.1.0)
   ```

   The binaries are not signed or notarized. On macOS, a binary downloaded with a browser
   is quarantined and Gatekeeper blocks it. Remove the attribute with
   `xattr -d com.apple.quarantine presenter`. Downloads with `curl` are not quarantined.

## What the workflow checks

| job | checks |
|---|---|
| `label` | On a tag push, the tag is exactly `v` + the `Cargo.toml` version, and it is annotated. |
| `build` (×4) | The runner builds for the matrix target natively (`rustc -vV` host). `cargo test --release --locked` passes; this tests the embedded assets, as shipped. `cargo build --release --locked` succeeds. `--version` is exactly `presenter <version> (<tag>)`. `presenter build examples/basic/slides.md` works. Every crate linked into the binary is in `THIRD-PARTY-LICENSES/crates.txt`. |
| `release` | All four archives exist. `gh release create --verify-tag` publishes them with `SHA256SUMS`. |

The release job runs only after all four builds succeed, so a release is never partial.
The `label` job checks the tag first, so a wrong or lightweight tag fails in seconds
instead of after four release builds.
The build jobs use no cache: release binaries start from a clean slate. A cache restored
from another branch would also be a supply-chain risk.

## Third-party licenses

The binary contains other people's work, and their licenses (MIT, Apache-2.0, OFL, MPL-2.0
and others) ask that their notices travel with it. The workflow's "third-party licenses"
step fills `THIRD-PARTY-LICENSES/` in each archive:

- `assets/`: the license files of the web assets embedded from `assets/vendor`
  (reveal.js, KaTeX, and the Source Sans Pro and League Gothic fonts), at their paths
  there, and `VERSIONS.toml` with their versions and sources.
- `crates.txt`: every crate linked into the binary for that target, with its version,
  license expression and crates.io page (where the source of, for example, the MPL-2.0
  `selectors` crate is available).
- `crates/<name>-<version>/`: those crates' own license files. Two crates ship none
  (`ansi-to-html`, `selectors`); `crates.txt` names their licenses.

"Linked" means the normal dependencies for the target (`cargo tree -e normal,no-proc-macro`):
build scripts, proc-macros and dev-dependencies run only at build time and are not
distributed. The step fails when a linked crate is missing from `cargo metadata`.

## Dry runs

There are two ways to try the workflow without publishing a real release.

- **Without a release:** on the Actions tab, choose "Release" → "Run workflow" on any
  branch. The `label` and `build` jobs run, and the archives become artifacts of the run.
  Each archive is named `presenter-v<version>-dev.<sha>-<target>`. The `release` job is
  skipped.
- **The full path:** push a branch to a fork and tag it there (`v<version>`, annotated).
  The fork gets the GitHub Release. Delete the release and the tag afterwards.

## Troubleshooting

- **The tag does not match `Cargo.toml`, or the tag is lightweight.** The `label` job
  fails. Delete the tag locally and on GitHub with
  `git tag -d v0.1.0 && git push --delete origin v0.1.0`. Fix the problem, then tag and
  push again.
- **One target failed.** Fix the cause, or use "Re-run failed jobs" for a flaky failure.
  Nothing has been published yet.
- **A release with that tag exists already.** This happens when the release job is re-run
  after it published. `gh release create` then fails. Delete the release on GitHub
  (`gh release delete v0.1.0`) before re-running the job, or upload the files by hand with
  `gh release upload`.
- **A runner label is retired.** GitHub retires runner images. For example, the task plan
  named `macos-13`, which no longer exists, so x86_64 macOS builds on `macos-15-intel`.
  `actionlint` reports unknown labels. When a label moves to another architecture, the
  native target check fails with a clear message.

## `presenter --version`

`presenter --version` prints `presenter <version> (<build id>)`. For example,
`presenter 0.1.0 (v0.1.0)` comes from a tagged release, and
`presenter 0.1.0 (33a4607-dirty)` from a development build. `build.rs` sets the build id
as the compile-time variable `PRESENTER_GIT_SHA`, and `cli::version()` reads it. The id is
the first of these that applies:

1. The `PRESENTER_GIT_SHA` environment variable at build time, when it is set and not
   blank. Packagers who build from a source archive without `.git` can use it, for example
   `PRESENTER_GIT_SHA=v0.1.0 cargo build --release`.
2. The output of `git describe --always --dirty` for the crate directory. That is the
   abbreviated commit (`33a4607`), or the annotated tag at or before it (`v0.1.0` or
   `v0.1.0-3-g33a4607`), plus `-dirty` when tracked files differ from the commit.
   `build.rs` computes the same string with `git describe --always` and
   `git --no-optional-locks status`, because `describe --dirty` would rewrite
   `.git/index` during the build.
3. `unknown`, when git is not installed or fails, or when the crate directory is not the
   root of a git checkout. The build never fails because of git. When the crate
   directory is a checkout and git still fails (git missing, no commit yet, or git
   refusing a directory owned by another user), Cargo prints a warning. Git does not
   search above the crate directory, so a source archive unpacked inside another
   repository gets `unknown`, not that repository's commit.

A build id contains only ASCII letters, digits, `.`, `+` and `-`. `build.rs` replaces any
other character with `-` and prints a Cargo warning: the tag `release/v1_0` becomes
`release-v1-0`, and the override `0.1.0~rc1` becomes `0.1.0-rc1`. `tests/cli_version.rs`
and `tests/cli_build.rs` check the same character set, so `cargo test` passes with any
override.

The build script reruns, and the crate is recompiled, when one of these changes:

- `src/`, `Cargo.toml`, `Cargo.lock` or `build.rs`, and in release builds `assets/`. These
  are the edits that recompile the crate anyway, so `-dirty` is checked again whenever
  the binary changes.
- `HEAD` or the HEAD reflog (`logs/HEAD`), which change on every commit, checkout and
  reset.
- The checked-out branch's ref file. After `git gc` or `git pack-refs` the branch lives in
  `packed-refs`, and the script watches `packed-refs` and the directory where the next
  commit writes the ref file.
- The tags (`refs/tags`, `packed-refs`).
- In a reftable repository (`git init --ref-format=reftable`), the reftable stacks.
- The `PRESENTER_GIT_SHA` environment variable.

All git paths come from `git rev-parse --git-path`, so linked worktrees watch their own
`HEAD`. Edits to other files, such as tests and docs, do not rerun the script. They do not
change the binary, but they count for `-dirty`: a development build may show `-dirty` for
a docs edit made before its last rerun, or miss one made after it. Release builds come
from a fresh checkout and are exact.

`tests/build_script.rs` tests this logic on scratch repositories, including packed refs,
reftable and linked worktrees. It compiles `build.rs` as a module. `build.rs` also sets
`PRESENTER_BUILD_ID_SOURCE` (`override`, `git` or `unknown`). With it,
`tests/cli_build.rs` checks that a build from a git checkout names `HEAD` and did not
fall back to `unknown`. Cargo passes build-script variables to test processes too, so the
test cannot use the runtime environment to tell an override apart.

## Release profile and measurements

`Cargo.toml` sets `[profile.release]`: `opt-level = 3`, `lto = "thin"`,
`codegen-units = 1`, `strip = true`. `cargo test --release` uses the same settings.

The table below was measured on 2026-10-08 on an Apple Silicon Mac (macOS 26.7, rustc 1.99.0,
aarch64-apple-darwin). Each time is the median of 30 runs for `--version` and 15 runs for
the builds.

| | Cargo's default release profile | T47 profile |
|---|---|---|
| binary size | 14,243,728 B (13.6 MiB) | 10,963,392 B (10.5 MiB) |
| `presenter --version` | 3.5 ms | 3.3 ms |
| `presenter build examples/basic/slides.md` (15 slides, 2.4 MB output) | 31.9 ms | 31.6 ms |
| the same with `--site` | 34.0 ms | 35.5 ms |
| clean `cargo build --release` (all dependencies) | — | 33 s wall, 118 s CPU |
| `cargo test --release` (after the release build above) | — | 71 s wall |

About 4.8 MB of the binary is embedded assets from `assets/`: reveal.js, KaTeX and the
themes. The task plan expected 15–25 MB; the binary is smaller. The profile makes no
measurable difference to run time; it is there for size.

## Offline builds

The build needs no network after the crates are fetched. No build script, ours or a
dependency's, downloads anything:

```sh
cargo fetch --locked
cargo build --release --offline --locked
```

This was verified on 2026-10-08. A clean release build ran with a fresh target directory,
`--offline`, and network access denied by
`sandbox-exec -p '(version 1) (allow default) (deny network*)'`. It succeeded and produced
the same 10,963,392-byte binary.

## Design choices

- **A hand-written workflow, not `cargo-dist`.** The workflow is under 200 lines, and it
  uses only GitHub's own actions, `dtolnay/rust-toolchain` and the `gh` CLI that is already
  on the runners. `cargo-dist` generates a longer workflow, adds its own config and
  installer scripts, and must be re-generated with a pinned `dist` binary whenever it is
  upgraded. It would not reduce the YAML for four plain tarballs. Reconsider it if
  installers (shell, Homebrew, MSI) or Windows builds are wanted.
- **Native runners, not `cross`.** Every target has a GitHub-hosted runner, so each binary
  is tested on its own platform before it is packaged. `cross` would build Linux arm64 under
  emulation and could not run the macOS tests at all.
- **Ubuntu 22.04 for Linux.** The binaries link the glibc of the build machine, so building
  on the older LTS lets them run on more distributions. Systems with a glibc older than
  Ubuntu 22.04's 2.35 may lack required symbol versions. A static musl build is the next
  step if that becomes a problem.
- **`dtolnay/rust-toolchain@stable`**, as in CI. A release is built with the stable Rust
  of the day; `--locked` pins every dependency.

## Validation status

This repository had no GitHub remote when the workflow was written, so the workflow has not
yet run on GitHub. The "four artifacts from a tag" check is still open; do it on a fork with
the first release. Checks done so far, on 2026-10-08:

- `actionlint` 1.7.12 with `shellcheck`: no findings. It also checks runner labels and
  action versions, and rejects `macos-13`.
- The run steps were extracted from the YAML and run locally against a clone with an
  annotated `v0.1.0` tag:
  - The label passes for `v0.1.0` and fails for `v0.2.0`, and for a lightweight
    `v0.1.0`.
  - The native target check passes for this host and fails with a clear message for a
    wrong target.
  - The smoke test passes with an annotated tag (`presenter 0.1.0 (v0.1.0)`). It fails
    with a lightweight tag (`presenter 0.1.0 (419e02d)`).
  - Packaging creates the expected files and no `._*` entries. The third-party licenses
    step, run for `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu`, accounted for
    all 142 and 147 linked crates, and copied the four license files of `assets/vendor`.
    A macOS archive is 5.9 MB.
  - The checksum step requires four archives, and `sha256sum -c` verifies its output.
  - The notes step extracts only the version's changelog section, or warns when the
    section is missing.
  - A stub `gh` showed the `gh release create` arguments, including `--prerelease` for
    `v0.2.0-rc.1`.
- Action versions were checked against the actions' release pages:
  `actions/checkout@v7`, `actions/upload-artifact@v7` and `actions/download-artifact@v8`
  (both artifact actions run on Node 24 and have the inputs used here). Runner labels were
  checked against GitHub's runner documentation.
- A release binary built `examples/basic/slides.md`, and the deck opened in Chrome over
  CDP: `Reveal.isReady()` was true, with 15 slides, 4 KaTeX formulas and no console
  errors.
- The build id followed the checkout in a clone (debug builds, `--version` after each
  step): after `git pack-refs --all` and two commits; after a `src/` edit (`-dirty`) and
  its revert; on the annotated tag (`v0.1.0`, then `v0.1.0-dirty` after an edit); after
  `git gc` and a commit; and in a reftable clone after a commit. A `tests/` edit left the
  crate fresh.

# Releasing presenter

A release is an annotated git tag `v<version>` on `main`. Pushing it runs
[`.github/workflows/release.yml`](../../.github/workflows/release.yml). The workflow tests
and builds `presenter` on six native runners and publishes a GitHub Release. The release
holds one archive per platform and a `SHA256SUMS` file.

| target | runner | archive |
|---|---|---|
| `aarch64-apple-darwin` (macOS arm64, macOS 11+) | `macos-15` | `presenter-v0.1.0-aarch64-apple-darwin.tar.gz` |
| `x86_64-apple-darwin` (macOS Intel, macOS 10.12+) | `macos-15-intel` | `presenter-v0.1.0-x86_64-apple-darwin.tar.gz` |
| `x86_64-unknown-linux-gnu` (glibc, built on Ubuntu 22.04) | `ubuntu-22.04` | `presenter-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` |
| `aarch64-unknown-linux-gnu` (glibc, built on Ubuntu 22.04) | `ubuntu-22.04-arm` | `presenter-v0.1.0-aarch64-unknown-linux-gnu.tar.gz` |
| `x86_64-pc-windows-msvc` (Windows x64, Windows 10+) | `windows-2025` | `presenter-v0.1.0-x86_64-pc-windows-msvc.zip` |
| `aarch64-pc-windows-msvc` (Windows arm64, Windows 10+) | `windows-11-arm` | `presenter-v0.1.0-aarch64-pc-windows-msvc.zip` |

Each archive holds one directory with the archive's name. That directory contains the
`presenter` binary (`presenter.exe` on Windows), `LICENSE`, `README.md` and
`THIRD-PARTY-LICENSES/` (see [Third-party licenses](#third-party-licenses)). In the
tarballs every entry is owned by `root`, and macOS archives carry no extended attributes;
in the zips every entry name uses `/`; the workflow fails otherwise (see
[What the workflow checks](#what-the-workflow-checks)).
The binary is self-contained: the themes, reveal.js and KaTeX are embedded. It needs only
the system's own libraries. On macOS those are `libSystem`, `libiconv` and the
CoreFoundation and CoreServices frameworks, which the file watcher uses for FSEvents
(checked with `otool -L`). On Linux they are glibc and `libgcc_s`, as for any Rust binary.
On Windows the C runtime is linked statically ([`.cargo/config.toml`](../../.cargo/config.toml)
sets `-C target-feature=+crt-static` for the two MSVC targets), so the Visual C++
Redistributable is not needed. `presenter export pdf` and `export pptx` are not available
on Windows: `src/pdf.rs` starts Chrome through `/bin/sh`, and says so.

## Checklist

1. Check that `main` is green in CI (`.github/workflows/ci.yml`).
2. Set the new version in `Cargo.toml`, then run `cargo check`. The release builds with
   `--locked`, so `Cargo.lock` must already have the new version. Also update the version
   pinned by two tests: the `VERSION_LINE` regex in `tests/cli_version.rs`, and the
   `version_string` test in `src/cli/mod.rs`. `tests/cli_build.rs` reads the version from
   Cargo and needs no change.
3. In `CHANGELOG.md`, check that `## [Unreleased]` covers every user-visible change since
   the last tag (`git log <last tag>..main`; new commands, options and behaviour, fixes
   users would notice). Then rename it to `## [<version>] - <YYYY-MM-DD>`, and add a new
   empty `## [Unreleased]` section above it. The workflow publishes the version's section
   as the release notes: everything from its heading to the next `## ` heading.
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
   six archives, `SHA256SUMS`, and the notes from the changelog. Then try one binary:

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
| `build` (×6) | The runner builds for the matrix target natively (`rustc -vV` host). `cargo test --release --locked` passes; this tests the embedded assets, as shipped. `cargo build --release --locked` succeeds. `--version` is exactly `presenter <version> (<tag>)`. `presenter build examples/basic/slides.md` works. Every crate linked into the binary is in `THIRD-PARTY-LICENSES/crates.txt`. The "package" step reads the archive back (Python's `tarfile`) and fails unless every entry belongs to `root:root` (uid and gid 0, so not to the builder's user), no entry is an AppleDouble `._*` file, and no entry has a pax header keyword that POSIX does not define. macOS `tar` would store extended attributes such as `com.apple.provenance` as `LIBARCHIVE.xattr.*` and `SCHILY.xattr.*` headers, and GNU `tar` warns about each of them when it extracts the archive. On Windows the step writes a zip with Python's `zipfile` instead and reads it back: the data checks out (`testzip`), every entry is under the archive's directory with `/` separators, and `presenter.exe` is there. |
| `release` | All six archives exist. `gh release create --verify-tag` publishes them with `SHA256SUMS`. |

The release job runs only after all six builds succeed, so a release is never partial.
The `label` job checks the tag first, so a wrong or lightweight tag fails in seconds
instead of after six release builds.
The build jobs use no cache: release binaries start from a clean slate. A cache restored
from another branch would also be a supply-chain risk.

## Third-party licenses

The binary contains other people's work, and their licenses (MIT, Apache-2.0, OFL, MPL-2.0
and others) ask that their notices travel with it. The workflow's "third-party licenses"
step fills `THIRD-PARTY-LICENSES/` in each archive:

- `assets/`: the license files of the web assets embedded from `assets/vendor`, at their
  paths there, and `VERSIONS.toml` with their versions and sources. These are reveal.js and
  KaTeX (MIT); the libraries reveal.js bundles into its files: highlight.js (BSD-3-Clause)
  and highlightjs-line-numbers.js (MIT) in the highlight plugin, marked (MIT) in the notes
  plugin and fitty (MIT) in `reveal.js` itself; and the Source Sans Pro and League Gothic
  fonts of reveal's themes (SIL OFL 1.1). `scripts/vendor.sh` fetches the bundled
  libraries' licenses from their own npm packages, because reveal's package does not
  contain them, and completes League Gothic's notice (`docs/spec/vendored-assets.md` §1).
- `crates.txt`: every crate linked into the binary for that target, with its version,
  license expression and crates.io page (where the source of, for example, the MPL-2.0
  `selectors` crate is available).
- `crates/<name>-<version>/`: those crates' own license files. Four crates ship none:
  `ansi-to-html` (MIT OR Apache-2.0), `grass` and `grass_compiler` (MIT), and `selectors`
  (MPL-2.0). MIT asks for its copyright and permission notice in every copy, so for a
  crate whose licenses include MIT the step writes `LICENSE-MIT`: the MIT text with the
  authors from the crate's `Cargo.toml`, under a line saying the workflow wrote it. For
  `selectors`, `crates.txt` names the license and where the source is.

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
  named `macos-13`, which no longer exists, so x86_64 macOS builds on `macos-15-intel`; and
  `macos-14` retires in November 2026 (with brownouts through October), so arm64 macOS
  builds on `macos-15` since 2026-10-10.
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

The table below was measured on 2026-10-08 on the T47 branch (commits 256aa8e and 26c4ff8,
based on 33a4607), on an Apple Silicon Mac (macOS 26.7, rustc 1.99.0, aarch64-apple-darwin).
Each time is the median of 30 runs for `--version` and 15 runs for the builds. On that
branch the example deck had 15 slides; T17, merged next to it, grew it to 17, and the binary
has grown with the commands merged after it (see below the table).

| | Cargo's default release profile | T47 profile |
|---|---|---|
| binary size | 14,243,728 B (13.6 MiB) | 10,963,392 B (10.5 MiB) |
| `presenter --version` | 3.5 ms | 3.3 ms |
| `presenter build examples/basic/slides.md` (then 15 slides, 2.4 MB output) | 31.9 ms | 31.6 ms |
| the same with `--site` | 34.0 ms | 35.5 ms |
| clean `cargo build --release` (all dependencies) | — | 33 s wall, 118 s CPU |
| `cargo test --release` (after the release build above) | — | 71 s wall |

About 4.8 MB of the binary is embedded assets from `assets/`: reveal.js, KaTeX and the
themes. The task plan expected 15–25 MB; the binary is smaller. The profile makes no
measurable difference to run time; it is there for size.

Since then the binary has gained `serve`, `check`, `init`, the `theme` commands, SCSS and
`export pdf`. On 2026-10-09, on top of commit b69ace7, the release binary was 15,018,112 B
(14.3 MiB) on the same Mac, and `presenter build examples/basic/slides.md` wrote 17 slides,
2.5 MB (2.4 MB with `--site`). The times were not measured again.

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

- **A hand-written workflow, not `cargo-dist`.** The workflow is about 300 lines, most of
  them comments and the license step, and it uses only GitHub's own actions,
  `dtolnay/rust-toolchain` and the `gh` CLI that is already on the runners. `cargo-dist`
  generates a longer workflow, adds its own config and installer scripts, and must be
  re-generated with a pinned `dist` binary whenever it is upgraded. It would not reduce the
  YAML for six plain archives. Reconsider it if installers (shell, Homebrew, MSI) are wanted.
- **Native runners, not `cross`.** Every target has a GitHub-hosted runner, so each binary
  is tested on its own platform before it is packaged. `cross` would build Linux arm64 under
  emulation and could not run the macOS tests at all.
- **Ubuntu 22.04 for Linux.** The binaries link the glibc of the build machine, so building
  on the older LTS lets them run on more distributions. Systems with a glibc older than
  Ubuntu 22.04's 2.35 may lack required symbol versions. A static musl build is the next
  step if that becomes a problem.
- **Windows: native runners, zips, a static C runtime.** `windows-2025` (x64) and
  `windows-11-arm` (arm64) build natively, so the tests run there too, as on the other
  platforms. The archives are zips because that is what Windows opens by itself.
  `.cargo/config.toml` links the C runtime statically for the two MSVC targets, so the
  binary needs no Visual C++ Redistributable, the usual choice for Rust tools shipped as
  one file. The workflow's steps run in the runner's Git Bash (`defaults.run.shell`), with
  two Windows adjustments: `cargo metadata` prints `\`-separated paths, so the jq filter of
  the license step strips `\Cargo.toml` as well as `/Cargo.toml` and `cygpath` converts
  the crate directories for `find`; and the zip is written and checked by Python, which is
  on every runner. `.gitattributes` (`* text=auto eol=lf`) keeps every text file LF in a
  Windows checkout, so the tests compare the same bytes as on macOS and Linux. `export pdf`
  and `export pptx` are not available on Windows (above).
- **`dtolnay/rust-toolchain@stable`**, as in CI. A release is built with the stable Rust
  of the day; `--locked` pins every dependency.

## Validation status

This repository had no GitHub remote when the workflow was written, so the workflow has not
yet run on GitHub. The "four artifacts from a tag" check is still open; do it on a fork with
the first release. Checks done by T47 on 2026-10-08, on its branch (15 example slides,
before the commands merged later):

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

Checks done on 2026-10-09 after the license and packaging changes of the audit fixes (A5),
on top of commit b69ace7:

- `actionlint` 1.7.12 with `shellcheck`: no findings.
- The "third-party licenses" step, run locally for `aarch64-apple-darwin` and
  `x86_64-unknown-linux-gnu`, accounted for all 153 and 158 linked crates, copied the nine
  license files of `assets/vendor` (reveal.js, KaTeX, highlight.js,
  highlightjs-line-numbers.js, marked, fitty, Source Sans Pro, League Gothic and its
  `LICENSE-OFL.txt`), and wrote `LICENSE-MIT` for `ansi-to-html`, `grass` and
  `grass_compiler`; `selectors` got its note.
- The "package" step, run with macOS `tar` (bsdtar 3.5.3) on files that carry
  `com.apple.provenance`: GNU tar 1.35 extracts the archive without a warning, where the
  previous command gave 442 "Ignoring unknown extended header keyword" warnings, and
  lists every entry as `root/root`. Run with GNU tar in place of `tar`, the entries are
  `root/root` too. The step's archive check passed both archives, and failed (exit 1) on
  an archive made without the ownership and `--no-xattrs` flags, with two errors for each of
  its 103 entries: owned by the builder, and `LIBARCHIVE.xattr.*` and `SCHILY.xattr.*`
  headers.

Checks done on 2026-10-10 for the two Windows targets and the runner change (on top of
commit 93e6e89), on a Mac, without a Windows machine:

- `actionlint` with `shellcheck`: no findings for `release.yml`, `ci.yml` and `site.yml`.
- The jq filter of the license step strips both `/Cargo.toml` and `\Cargo.toml` (run on a
  `C:\…` sample path). The zip packaging code, extracted from the YAML and run on a sample
  directory, wrote the archive, read it back and listed its entries.
- `cargo clippy --target aarch64-pc-windows-msvc --all-targets --all-features -- -D warnings`
  passes, with the target's standard library from rustup and stand-in C tools for the build
  scripts of `blake3` and `ring` (a check links nothing). It had found an `unused_mut` in
  `create_private_dir` and seven test helpers in `src/pdf.rs` and `tests/cli_export.rs`
  that only Unix tests use; they are gated with `#[cfg(unix)]` now. The tests themselves
  have not run on Windows yet: the first CI run on `windows-latest`, or a dry run of this
  workflow, shows whether they pass there.
- The labels `windows-2025`, `windows-11-arm` and `macos-15` were checked against
  actions/runner-images. Both Windows images ship Rustup, Python, jq, 7-Zip and Git Bash,
  and the arm64 image has Visual Studio's ARM64 build tools.
- The first CI run on `windows-latest` (pull request #5): `cargo fmt` and `cargo clippy`
  passed; `cargo test` failed 16 unit tests, none a build problem. Twelve asserted POSIX
  paths (`/abs/…` spellings, `/` separators, canonical paths without `\\?\`) and are gated
  with `#[cfg(unix)]` or compare `Path::join` output now. Two expected the "not an
  executable" error, which the platform guard precedes on Windows. One joined `new/..` onto
  a canonical path, which `PathBuf::push` resolves itself on Windows; the test joins as
  text now. One found a real difference: Windows refuses to open a directory (permission
  denied) where Unix opens it, so `serve` answered a directory URL with an error other
  than 404; `open_range` maps that to not found now. Also, the `init` hint for a name
  starting with `-` is spelled `./name` on every platform (it was `'.\name'` on Windows).
  The integration tests had not run yet (the unit tests failed first); CI runs with
  `--no-fail-fast` now, so one run shows every test binary.
- The second run reached the integration tests: 47 failures in 14 test binaries, again
  none a build problem. Most showed relative paths with `\`, where the tests, the decks,
  the guide and the HTML spell `/`: `display_path` and the few other sites that show a
  relative path join it with `/` on every platform now, and an absolute path keeps its
  native spelling. The watcher took Windows' "directory modified" events (one for every
  entry written, renamed or removed inside a directory) as changes, which turned a
  stylesheet edit into a content rebuild and the build's own output into a change; those
  events are ignored now, while a created, removed or renamed directory still counts. The
  usage lines said `presenter.exe`, so clap's `bin_name` pins `presenter`. A `--config`
  error names the file as typed instead of its absolute path. `theme new a/..` lost its
  "(that is …)", because `std::path::absolute` resolves `..` on Windows; the message looks
  for the `..` itself. The remaining tests compared OS-native text (`\\?\` canonical paths,
  mixed separators, the compiler's `tests\…` panic location, a `\`-separated Sass URL, a
  percent-encoded `C:…` reference that reads as a URL scheme) and compare paths, or
  `/`-normalized text, now.
- The fourth run left one watcher test that had passed the run before: removing a
  directory above a watched one. Windows reports that only through the parent's watch
  (the watched directory's own handle dies with it), and the watcher ignored the event
  because the removed directory was neither a target nor on the way to a missing one;
  removing or renaming a directory above a target is a structural change now, on every
  platform. (The same run also saw the browser smoke test time out while Chrome started on
  the loaded runner: that test gives the start-up its own time now and keeps its short
  timeout for the navigation it measures.)
- The fifth run was the first to pass the debug tests on Windows; the release-mode tests
  then hit the same limitation one level up: removing the deck's own directory, the only
  watched one. Windows reports nothing when a watched directory itself is removed (the
  parent's watch would), so on Windows the watcher now also watches the parent of every
  watched directory, non-recursively (`guard_watches`); `watched_dirs()` leaves these out,
  so the tests' expectations hold on every platform.

//! Runs the built `presenter` binary: `--version` / `-V` (T00 acceptance criterion).

use assert_cmd::Command;
use predicates::str::is_match;

/// Exactly `presenter 0.1.0` on one line. The optional ` (<git describe>)` suffix is the
/// format T47 introduces, so that task does not have to edit this file.
const VERSION_LINE: &str = r"^presenter 0\.1\.0( \([0-9A-Za-z.+-]+\))?\n$";

#[test]
fn version_flag_prints_name_and_version() {
    for flag in ["--version", "-V"] {
        Command::new(env!("CARGO_BIN_EXE_presenter"))
            .arg(flag)
            .assert()
            .success()
            .stdout(is_match(VERSION_LINE).expect("valid regex"))
            .stderr("");
    }
}

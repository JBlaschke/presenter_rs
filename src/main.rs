//! `presenter` command-line entry point: everything happens in [`presenter::cli::run`].

use std::process::ExitCode;

fn main() -> ExitCode {
    presenter::cli::run()
}

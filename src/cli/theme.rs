//! `presenter theme list/eject/new/check`. Implemented by T41.
//!
//! Until then `run` is the T16 stub: it prints `theme: not implemented yet` on stderr and
//! exits 2. T41 replaces the body of `run`; the arguments are defined in `cli/mod.rs`
//! ([`ThemeArgs`]).

use std::process::ExitCode;

use super::{GlobalArgs, ThemeArgs, not_implemented};

/// Runs `presenter theme` (stub until T41).
pub fn run(args: &ThemeArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let _ = (args, global);
    not_implemented("theme")
}

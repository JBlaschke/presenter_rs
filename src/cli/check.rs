//! `presenter check`. Implemented by T34.
//!
//! Until then `run` is the T16 stub: it prints `check: not implemented yet` on stderr and
//! exits 2. T34 replaces the body of `run`; the arguments are defined in `cli/mod.rs`
//! ([`CheckArgs`]).

use std::process::ExitCode;

use super::{CheckArgs, GlobalArgs, not_implemented};

/// Runs `presenter check` (stub until T34).
pub fn run(args: &CheckArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let _ = (args, global);
    not_implemented("check")
}

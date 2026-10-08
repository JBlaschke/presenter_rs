//! `presenter serve`. Implemented by T23.
//!
//! Until then `run` is the T16 stub: it prints `serve: not implemented yet` on stderr and
//! exits 2. T23 replaces the body of `run`; the arguments are defined in `cli/mod.rs`
//! ([`ServeArgs`]).

use std::process::ExitCode;

use super::{GlobalArgs, ServeArgs, not_implemented};

/// Runs `presenter serve` (stub until T23).
pub fn run(args: &ServeArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let _ = (args, global);
    not_implemented("serve")
}

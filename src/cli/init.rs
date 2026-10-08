//! `presenter init`. Implemented by T40.
//!
//! Until then `run` is the T16 stub: it prints `init: not implemented yet` on stderr and
//! exits 2. T40 replaces the body of `run`; the arguments are defined in `cli/mod.rs`
//! ([`InitArgs`]).

use std::process::ExitCode;

use super::{GlobalArgs, InitArgs, not_implemented};

/// Runs `presenter init` (stub until T40).
pub fn run(args: &InitArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let _ = (args, global);
    not_implemented("init")
}

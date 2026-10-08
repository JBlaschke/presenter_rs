//! `presenter export pdf`. Implemented by T45.
//!
//! Until then `run` is the T16 stub: it prints `export: not implemented yet` on stderr and
//! exits 2. T45 replaces the body of `run`; the arguments are defined in `cli/mod.rs`
//! ([`ExportArgs`]).

use std::process::ExitCode;

use super::{ExportArgs, GlobalArgs, not_implemented};

/// Runs `presenter export` (stub until T45).
pub fn run(args: &ExportArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let _ = (args, global);
    not_implemented("export")
}

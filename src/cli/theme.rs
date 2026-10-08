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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{EXIT_USAGE, ThemeCommand};

    /// The stub's contract (`theme: not implemented yet` on stderr, exit 2); T41 deletes
    /// this test together with the stub.
    #[test]
    fn stub_exits_2() {
        let args = ThemeArgs {
            command: ThemeCommand::List,
        };
        let code = run(&args, &GlobalArgs::default()).unwrap();
        assert_eq!(code, ExitCode::from(EXIT_USAGE));
    }
}

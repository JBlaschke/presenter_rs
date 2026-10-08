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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::cli::{CheckFormat, EXIT_USAGE};

    /// The stub's contract (`check: not implemented yet` on stderr, exit 2); T34 deletes
    /// this test together with the stub.
    #[test]
    fn stub_exits_2() {
        let args = CheckArgs {
            inputs: vec![PathBuf::from("slides.md")],
            format: CheckFormat::Text,
            deny_warnings: false,
            theme: None,
        };
        let code = run(&args, &GlobalArgs::default()).unwrap();
        assert_eq!(code, ExitCode::from(EXIT_USAGE));
    }
}

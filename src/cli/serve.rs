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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::cli::EXIT_USAGE;

    /// The stub's contract (`serve: not implemented yet` on stderr, exit 2); T23 deletes
    /// this test together with the stub.
    #[test]
    fn stub_exits_2() {
        let args = ServeArgs {
            input: PathBuf::from("slides.md"),
            theme: None,
            port: Some(0),
            host: None,
            open: false,
            site_preview: false,
        };
        let code = run(&args, &GlobalArgs::default()).unwrap();
        assert_eq!(code, ExitCode::from(EXIT_USAGE));
    }
}

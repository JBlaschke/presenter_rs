//! Crate-wide error type and `Result` alias (contract: `docs/spec/ir.md` §5).
//!
//! T00 stub: only the variant the scaffold needs (`vendor::get_str`). T01 replaces this file
//! with the full enum from the spec; `Other(String)` keeps the same name and shape there.

/// Errors produced by the library.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Anything without a more specific variant.
    #[error("{0}")]
    Other(String),
}

/// Result alias used throughout the library.
pub type Result<T> = std::result::Result<T, Error>;

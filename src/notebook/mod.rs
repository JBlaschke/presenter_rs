//! Jupyter notebook frontend (nbformat 4).
//!
//! Contract: `docs/spec/notebook-mapping.md`. The entry point is implemented by T31.

pub mod convert;
pub mod nbformat;
pub mod outputs;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::ir::{Parsed, SourceInfo};

/// Parses a Jupyter notebook (`json` is the `.ipynb` file's contents) into a [`Parsed`] deck.
///
/// Contract: `docs/spec/ir.md` §4 and `docs/spec/notebook-mapping.md`. Coordinator stub so
/// the pipeline (T16) compiles against the final signature; T31 replaces the body.
pub fn parse(source: &SourceInfo, json: &str, config: &Config) -> Result<Parsed> {
    let _ = (source, json, config);
    Err(Error::Other("notebook support arrives in T31".to_string()))
}

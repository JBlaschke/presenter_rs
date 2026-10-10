//! `presenter` turns Markdown files and Jupyter notebooks into reveal.js presentations.
//!
//! The pipeline is: a frontend ([`markdown`], [`notebook`]) parses a source file into the
//! [`ir`] (`Deck`), the [`theme`] chain and [`config`] are resolved, [`render`] produces an
//! HTML document and [`assets`] emits it as a single self-contained file or a site
//! directory. [`serve`] wraps this in a watch/rebuild/live-reload loop. See `PLAN.md`.
//!
//! Every module of the planned crate is declared here so that later tasks never need to
//! edit this file.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod assets;
pub mod check;
pub mod cli;
pub mod config;
pub mod error;
pub mod ir;
pub mod markdown;
pub mod notebook;
pub mod pdf;
pub mod pipeline;
pub mod pptx;
pub mod render;
pub mod serve;
pub mod theme;
pub mod vendor;

pub use error::{Error, Result};

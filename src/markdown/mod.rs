//! Markdown frontend: front matter, slide splitting, attributes and block rendering.
//!
//! Contract: `docs/spec/markdown-dialect.md`. The entry point is implemented by T12.

pub mod attrs;
pub mod code;
pub mod compat;
pub mod frontmatter;
pub mod postprocess;
pub mod render;
pub mod split;

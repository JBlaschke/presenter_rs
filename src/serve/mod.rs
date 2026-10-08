//! Development server with file watching and live reload (`presenter serve`).
//!
//! Contract: `docs/spec/live-reload.md`. The pieces:
//!
//! - [`watch`] (T20): watches the deck's inputs, debounces changes, rebuilds and classifies
//!   each change batch as [`watch::ChangeKind::Css`] or [`watch::ChangeKind::Content`].
//! - [`server`] (T21): the axum server that serves the latest build from memory
//!   ([`server::ServeState`]) and pushes build events to the browsers over Server-Sent
//!   Events.
//! - [`events`] (T21): the JSON payloads of those events ([`events::BuildEvent`],
//!   [`events::Hello`]).
//!
//! The client side is `assets/presenter/live-reload.js` (T22); the CLI wiring is T23.

pub mod events;
pub mod server;
pub mod watch;

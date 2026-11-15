//! The `/_admin` surface: reset, inspect, advance an event, move the clock, inject a
//! fault, register a webhook endpoint.
//!
//! Mounted on the API listener under `/_admin`, which is safe because Resend's own
//! paths are a fixed set that will never include it. Nothing here is authenticated, and
//! nothing here is rate limited: a suite that has shut itself out still needs a working
//! way to reset. See `SECURITY.md`.
//!
//! The embedded console is a later phase. This is the API it will be built on.

#![forbid(unsafe_code)]

pub mod router;
pub mod routes;
pub mod wire;

use chrono::{DateTime, SecondsFormat, Utc};

pub use router::router;

/// Formats a timestamp for the admin API.
///
/// Milliseconds and a `Z`, which is the easier of the two formats Resend itself uses to
/// read in a terminal. The admin API is not Resend, so it does not have to match either.
#[must_use]
pub fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

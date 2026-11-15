//! The Resend REST surface.
//!
//! The only crate that knows Resend is JSON over HTTP. Handlers parse, call the engine
//! and map the error; the behaviour they expose lives in `mock-resend-core`, so a
//! message arriving over SMTP is held to the same rules.

#![forbid(unsafe_code)]

pub mod auth;
pub mod error;
pub mod ratelimit;
pub mod router;
pub mod routes;
pub mod wire;

pub use error::ApiError;
pub use router::router;

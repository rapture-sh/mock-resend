//! Outbound webhook delivery, Svix-compatible signing, and retries.
//!
//! This is a crate boundary rather than a module on purpose: it is the **only** crate
//! in the workspace that makes an outbound request, which is what makes rule 5
//! enforceable. There is exactly one place to audit, and everything that leaves this
//! machine goes through [`guard`] first.

#![forbid(unsafe_code)]

pub mod guard;
pub mod payload;
pub mod sender;
pub mod signature;

pub use guard::Policy;
pub use sender::{Config, Sender};

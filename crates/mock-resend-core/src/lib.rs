//! The mock-resend engine.
//!
//! Owns the mailbox, the delivery event lifecycle, scheduling and storage, and knows
//! nothing about HTTP or JSON. Input in, result out, with the filesystem behind the
//! [`Store`] trait.
//!
//! Everything is scoped to an [`ApiKey`]. There are no global switches, which is what
//! makes one instance safe for a test suite running in parallel: a key creates its
//! namespace on first use, so giving each test its own costs nothing.

#![forbid(unsafe_code)]

pub mod engine;
pub mod error;
pub mod events;
pub mod mailbox;
pub mod model;
pub mod schedule;
pub mod store;

pub use engine::Engine;
pub use error::{Error, Result};
pub use events::EmailEvent;
pub use mailbox::SendEmail;
pub use model::{ApiKey, Attachment, AttachmentSource, EmailId, Message, Tag};
pub use schedule::{Clock, SystemClock, TestClock};
pub use store::{Store, memory::MemoryStore};

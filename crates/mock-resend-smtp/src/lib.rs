//! The SMTP listener: a second way into the same engine, not a second mailbox.
//!
//! A message that arrives here is retrievable through `GET /emails/{id}` and produces
//! the same events as one sent through the API, because it goes through the same engine
//! method.
//!
//! Everything in this crate parses input a stranger controls, which is why the reads are
//! bounded, the buffers are capped, idle connections are dropped, and there is no
//! `unwrap` outside the tests. There is no `STARTTLS` and there will not be; see
//! `SECURITY.md`.

#![forbid(unsafe_code)]

pub mod command;
pub mod line;
pub mod listener;
pub mod session;

pub use listener::serve;
pub use session::{Config, USERNAME};

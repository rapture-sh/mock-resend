//! The engine's typed errors.
//!
//! Each variant maps to exactly one Resend error `name` at the HTTP boundary. The
//! mapping lives in `mock-resend-api`, because this crate does not know what a status
//! code is.
//!
//! The `Display` text becomes the `message` field of a Resend error body. It is written
//! for a human reading a test failure and is deliberately not contractual: callers
//! branch on the error `name`, never on this wording.

use thiserror::Error;

/// A [`Result`](std::result::Result) with the engine's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Anything the engine can refuse to do.
///
/// Deliberately **not** `#[non_exhaustive]`. The match in `mock-resend-api`'s error
/// module is exhaustive, so adding a variant here does not compile until it has been
/// given a Resend error `name` and a status. A wildcard arm would let the next one
/// quietly become `application_error`, which is the subtle wrongness this project exists
/// to avoid.
#[derive(Debug, Error)]
pub enum Error {
    /// One or more required body fields were absent.
    ///
    /// Resend names the missing fields in the message, so the engine carries them.
    #[error("The request body is missing one or more required fields: {}.", fields.join(", "))]
    MissingRequiredField {
        /// The absent fields, in the order the request declares them.
        fields: Vec<String>,
    },

    /// A field was present but unacceptable.
    #[error("{message}")]
    Validation {
        /// What is wrong with the request, in plain words.
        message: String,
    },

    /// An attachment carried neither `content` nor `path`.
    #[error("Attachment must have either a `content` or `path`.")]
    InvalidAttachment,

    /// A path parameter was not a UUID.
    #[error("The parameter must be a valid UUID.")]
    InvalidParameter,

    /// No message with that id exists under this API key.
    #[error("Email not found.")]
    NotFound,

    /// The `Authorization` header was absent, malformed, or not a `re_`-prefixed key.
    #[error("Missing API key in the authorization header.")]
    MissingApiKey,

    /// The `Idempotency-Key` header was outside the 1 to 256 character range.
    #[error("The key must be between 1-256 chars.")]
    InvalidIdempotencyKey,

    /// The same idempotency key is already in flight.
    #[error("Same idempotency key used while original request is still in progress.")]
    ConcurrentIdempotentRequests,

    /// The same idempotency key was used with a different payload.
    #[error("Same idempotency key used with a different request payload.")]
    InvalidIdempotentRequest,

    /// A fault rule fired, so this error was injected rather than earned.
    ///
    /// Carries the whole Resend error, because the rule chose it: the name and status a
    /// caller sees come from the test that asked for them, not from this enum.
    #[error("{}", .0.message)]
    Injected(crate::fault::ResendError),

    /// The storage backend failed.
    ///
    /// Carries a string rather than the source error so that the variant stays the same
    /// shape across backends.
    #[error("Storage failure: {0}")]
    Store(String),
}

impl Error {
    /// Builds a [`Error::Validation`] from anything string-like.
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
        }
    }
}

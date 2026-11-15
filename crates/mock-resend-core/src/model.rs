//! The engine's value types.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::events::EmailEvent;

/// The id Resend assigns to a captured message.
///
/// Resend's ids are UUIDs, and it rejects a non-UUID path parameter with
/// `invalid_parameter` rather than `not_found`, so this is a `Uuid` and not a `String`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EmailId(Uuid);

impl EmailId {
    /// Mints a fresh id.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }
}

impl FromStr for EmailId {
    type Err = Error;

    /// Returns [`Error::InvalidParameter`] for anything that is not a UUID, matching
    /// what Resend does with a malformed `{id}`.
    fn from_str(value: &str) -> Result<Self> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| Error::InvalidParameter)
    }
}

impl fmt::Display for EmailId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Hyphenated lowercase, which is the form Resend returns.
        write!(f, "{}", self.0.as_hyphenated())
    }
}

/// An API key, which is this server's whole isolation mechanism.
///
/// Nothing is authenticated: any `re_`-prefixed string is valid and creates its
/// namespace on first use. The prefix is checked only so that a caller who forgot to
/// configure a key gets Resend's `missing_api_key` instead of silent success.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ApiKey(String);

impl ApiKey {
    /// The prefix every Resend API key carries.
    pub const PREFIX: &'static str = "re_";

    /// Accepts any `re_`-prefixed string with at least one character after the prefix.
    ///
    /// # Errors
    ///
    /// [`Error::MissingApiKey`] if the prefix is absent or nothing follows it.
    pub fn parse(value: &str) -> Result<Self> {
        let rest = value
            .strip_prefix(Self::PREFIX)
            .ok_or(Error::MissingApiKey)?;

        if rest.is_empty() {
            return Err(Error::MissingApiKey);
        }

        Ok(Self(value.to_owned()))
    }

    /// The key as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ApiKey {
    /// Redacts everything after the prefix.
    ///
    /// A key is not a secret here, but it is frequently a test name, and a test name is
    /// one dereference away from the mail it captured. Logs get the prefix only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}***", Self::PREFIX)
    }
}

/// Where an attachment's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AttachmentSource {
    /// Inline bytes, decoded from the request.
    Content(Bytes),
    /// A URL the caller chose, which is **never** fetched.
    ///
    /// Recorded and reported, and nothing in the workspace requests it: fetching a URL
    /// the caller chose is a server-side request forgery on the developer's machine. If
    /// that ever changes it goes through the one guarded client in
    /// `mock-resend-webhooks` and nowhere else.
    Path(String),
}

/// A file attached to a message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Attachment {
    /// The caller-supplied filename. An arbitrary string, never treated as a path.
    pub filename: Option<String>,
    /// Declared content type. Resend derives it from the filename when absent.
    pub content_type: Option<String>,
    /// Marks the attachment inline, for reference as `cid:` in the HTML body.
    pub content_id: Option<String>,
    /// The bytes, or the URL they would come from.
    pub source: AttachmentSource,
}

/// A name and value pair attached to a message for reporting.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Tag {
    /// ASCII letters, digits, underscores and dashes, at most 256 characters.
    pub name: String,
    /// Same character set and limit as the name.
    pub value: String,
}

/// A captured message.
///
/// It is stored, reported and forgotten. Nothing in this repo delivers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The id `POST /emails` returned.
    pub id: EmailId,
    /// The key that owns it. Two keys never see each other's mail.
    pub key: ApiKey,
    /// Sender, in either `address` or `Name <address>` form, as given.
    pub from: String,
    /// Recipients. At most 50, which is Resend's limit.
    pub to: Vec<String>,
    /// Carbon copy recipients.
    pub cc: Vec<String>,
    /// Blind carbon copy recipients.
    pub bcc: Vec<String>,
    /// Addresses replies should go to.
    pub reply_to: Vec<String>,
    /// The subject line.
    pub subject: String,
    /// The HTML body.
    pub html: Option<String>,
    /// The plain text body.
    pub text: Option<String>,
    /// Caller-supplied headers, ordered so that the stored form is stable.
    pub headers: BTreeMap<String, String>,
    /// Attached files.
    pub attachments: Vec<Attachment>,
    /// Reporting tags.
    pub tags: Vec<Tag>,
    /// A template id, recorded but never rendered. Template rendering is out of scope.
    pub template: Option<String>,
    /// When the send is due, if the caller scheduled it.
    pub scheduled_at: Option<DateTime<Utc>>,
    /// When the engine captured it, from the engine's clock rather than the wall clock.
    pub created_at: DateTime<Utc>,
    /// The most recent delivery event.
    pub last_event: EmailEvent,
    /// The RFC 5322 `Message-ID`.
    ///
    /// Absent until the message is actually handed off, which is why Resend omits it on
    /// `email.scheduled`.
    pub message_id: Option<String>,
}

#[cfg(test)]
mod email_id {
    use super::EmailId;
    use crate::Error;

    #[test]
    fn should_round_trip_through_its_string_form() {
        let id = EmailId::generate();
        let parsed: EmailId = id.to_string().parse().expect("its own output parses");

        assert_eq!(id, parsed);
    }

    #[test]
    fn should_render_as_hyphenated_lowercase() {
        let rendered = EmailId::generate().to_string();

        assert_eq!(rendered.len(), 36);
        assert_eq!(rendered.matches('-').count(), 4);
        assert_eq!(rendered, rendered.to_lowercase());
    }

    #[test]
    fn should_reject_a_non_uuid_as_an_invalid_parameter() {
        let error = "not-a-uuid".parse::<EmailId>().expect_err("not a uuid");

        assert!(matches!(error, Error::InvalidParameter));
    }

    #[test]
    fn should_generate_a_different_id_each_time() {
        assert_ne!(EmailId::generate(), EmailId::generate());
    }
}

#[cfg(test)]
mod api_key {
    use super::ApiKey;
    use crate::Error;

    #[test]
    fn should_accept_any_re_prefixed_string() {
        let key = ApiKey::parse("re_should_accept_any_re_prefixed_string").expect("valid");

        assert_eq!(key.as_str(), "re_should_accept_any_re_prefixed_string");
    }

    #[test]
    fn should_reject_a_key_without_the_prefix() {
        let error = ApiKey::parse("sk_live_nope").expect_err("no re_ prefix");

        assert!(matches!(error, Error::MissingApiKey));
    }

    #[test]
    fn should_reject_the_bare_prefix() {
        assert!(matches!(
            ApiKey::parse("re_").expect_err("nothing follows the prefix"),
            Error::MissingApiKey
        ));
    }

    #[test]
    fn should_treat_keys_differing_only_in_case_as_different_namespaces() {
        let lower = ApiKey::parse("re_case").expect("valid");
        let upper = ApiKey::parse("re_CASE").expect("valid");

        assert_ne!(lower, upper);
    }

    #[test]
    fn should_redact_the_key_when_displayed() {
        let key = ApiKey::parse("re_secret_test_name").expect("valid");

        assert_eq!(key.to_string(), "re_***");
        assert!(!key.to_string().contains("secret"));
    }
}

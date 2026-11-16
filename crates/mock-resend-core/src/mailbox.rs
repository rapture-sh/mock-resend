//! Capture, retrieve, list and reset, all keyed by API key.
//!
//! Validation lives here rather than in a handler, because validation is most of what a
//! mock is for: a payload mock-resend accepts has to be a payload real Resend accepts,
//! and a handler is the wrong place to keep that promise.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::error::{Error, Result};
use crate::model::{Attachment, AttachmentSource};

/// Resend's recipient ceiling for a single send.
///
/// <https://resend.com/docs/api-reference/emails/send-email#body-parameters>
pub const MAX_RECIPIENTS: usize = 50;

/// The longest a tag name or value may be.
pub const MAX_TAG_LENGTH: usize = 256;

/// A send request, before the engine has made a [`Message`](crate::Message) of it.
///
/// The field names are Resend's. Nothing here is optional-by-convenience: `to`, `cc`,
/// `bcc` and `reply_to` arrive as a string or an array of strings on the wire, and the
/// HTTP layer has already collapsed both forms into a `Vec` by the time it gets here.
///
/// `Hash` is derived so that the idempotency register can fingerprint a request: two
/// bodies that differ only in field order normalise to the same struct, and therefore
/// to the same fingerprint, which is the comparison Resend's
/// `invalid_idempotent_request` is really about.
#[derive(Debug, Clone, Default, Hash)]
pub struct SendEmail {
    /// Sender, as `address` or `Name <address>`.
    pub from: String,
    /// Recipients.
    pub to: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// HTML body.
    pub html: Option<String>,
    /// Plain text body.
    pub text: Option<String>,
    /// Carbon copy recipients.
    pub cc: Vec<String>,
    /// Blind carbon copy recipients.
    pub bcc: Vec<String>,
    /// Addresses replies should go to.
    pub reply_to: Vec<String>,
    /// Caller-supplied headers.
    pub headers: BTreeMap<String, String>,
    /// Attached files.
    pub attachments: Vec<Attachment>,
    /// Reporting tags.
    pub tags: Vec<crate::model::Tag>,
    /// A template id. Recorded, never rendered.
    pub template: Option<String>,
    /// When the send is due.
    pub scheduled_at: Option<DateTime<Utc>>,
    /// Which way in the request arrived. The API is the default; SMTP says so.
    pub origin: crate::model::Origin,
}

impl SendEmail {
    /// A request with the three fields Resend always requires.
    #[must_use]
    pub fn new(from: impl Into<String>, to: Vec<String>, subject: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to,
            subject: subject.into(),
            ..Self::default()
        }
    }

    /// A stable digest of this request, for idempotency comparison.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        crate::idempotency::fingerprint_of(self)
    }

    /// Checks the request the way Resend does.
    ///
    /// Required fields are reported together, as one `missing_required_field` naming all
    /// of them, because that is what Resend's message does and callers log it.
    ///
    /// # Errors
    ///
    /// - [`Error::MissingRequiredField`] when `from`, `to` or `subject` is absent.
    /// - [`Error::Validation`] for a bad address, too many recipients, or a bad tag.
    /// - [`Error::InvalidAttachment`] for an attachment with neither `content` nor
    ///   `path`.
    pub fn validate(&self) -> Result<()> {
        let mut missing = Vec::new();

        if self.from.trim().is_empty() {
            missing.push("from".to_owned());
        }
        if self.to.is_empty() {
            missing.push("to".to_owned());
        }
        if self.subject.trim().is_empty() {
            missing.push("subject".to_owned());
        }

        if !missing.is_empty() {
            return Err(Error::MissingRequiredField { fields: missing });
        }

        if self.html.is_none() && self.text.is_none() && self.template.is_none() {
            return Err(Error::validation(
                "The request body must contain one of `html`, `text` or `template`.",
            ));
        }

        // A template replaces the body rather than supplementing it.
        if self.template.is_some() && (self.html.is_some() || self.text.is_some()) {
            return Err(Error::validation(
                "`template` cannot be combined with `html` or `text`.",
            ));
        }

        if self.to.len() > MAX_RECIPIENTS {
            return Err(Error::validation(format!(
                "The `to` field can contain at most {MAX_RECIPIENTS} recipients, and this request has {}.",
                self.to.len()
            )));
        }

        validate_address("from", &self.from)?;

        for (field, addresses) in [
            ("to", &self.to),
            ("cc", &self.cc),
            ("bcc", &self.bcc),
            ("reply_to", &self.reply_to),
        ] {
            for address in addresses {
                validate_address(field, address)?;
            }
        }

        for (name, value) in &self.headers {
            validate_header(name, value)?;
        }

        for tag in &self.tags {
            validate_tag_part("name", &tag.name)?;
            validate_tag_part("value", &tag.value)?;
        }

        for attachment in &self.attachments {
            validate_attachment(attachment)?;
        }

        Ok(())
    }
}

/// Accepts `address` and `Name <address>`, and nothing without an address part.
///
/// Deliberately permissive. Resend's own validation is not published, and a mock that
/// rejects what the real API accepts fails a test for a reason the code under test is
/// not responsible for — the worse of the two mistakes.
fn validate_address(field: &str, value: &str) -> Result<()> {
    let address = extract_address(value);

    let invalid = |reason: &str| {
        Err(Error::validation(format!(
            "`{field}` is not a valid email address: {reason}."
        )))
    };

    if address.is_empty() {
        return invalid("it is empty");
    }

    if address.chars().any(char::is_whitespace) {
        return invalid("it contains whitespace");
    }

    let Some((local, domain)) = address.split_once('@') else {
        return invalid("it has no `@`");
    };

    if local.is_empty() {
        return invalid("it has nothing before the `@`");
    }

    if domain.is_empty() {
        return invalid("it has nothing after the `@`");
    }

    if domain.contains('@') {
        return invalid("it has more than one `@`");
    }

    Ok(())
}

/// Returns the address inside `Name <address>`, or the whole string if there is no
/// display name.
fn extract_address(value: &str) -> &str {
    let trimmed = value.trim();

    match (trimmed.rfind('<'), trimmed.rfind('>')) {
        (Some(open), Some(close)) if close > open => trimmed[open + 1..close].trim(),
        _ => trimmed,
    }
}

/// Checks one caller-supplied header.
///
/// A header value containing a carriage return or newline is not a header value: it is
/// two headers. Writing one verbatim into a message lets a caller append any header
/// they like, which in this server means forging the `X-Mock-Resend-*` fields the disk
/// backend uses to record which API key owns a captured message — and so moving their
/// own mail, subject and recipients into somebody else's mailbox. Rejecting is also
/// what the real API has to do, because an injected header is not representable in
/// RFC 5322.
///
/// A tab is allowed in a value: it is how a long header is legally folded.
fn validate_header(name: &str, value: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(Error::validation("A header name cannot be empty."));
    }

    // RFC 5322 field names are printable US-ASCII without a colon.
    if let Some(offender) = name
        .chars()
        .find(|character| !character.is_ascii_graphic() || *character == ':')
    {
        return Err(Error::validation(format!(
            "The header name `{name}` contains {offender:?}, which a header name cannot contain."
        )));
    }

    if let Some(offender) = value
        .chars()
        .find(|character| character.is_control() && *character != '\t')
    {
        return Err(Error::validation(format!(
            "The value of header `{name}` contains {offender:?}. A header value cannot contain a control character, because a newline in one would start another header."
        )));
    }

    Ok(())
}

/// Tag names and values take ASCII letters, digits, underscores and dashes only.
///
/// <https://resend.com/docs/api-reference/emails/send-email#body-parameters>
fn validate_tag_part(part: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::validation(format!(
            "A tag `{part}` cannot be empty."
        )));
    }

    if value.len() > MAX_TAG_LENGTH {
        return Err(Error::validation(format!(
            "A tag `{part}` can contain at most {MAX_TAG_LENGTH} characters."
        )));
    }

    if let Some(offender) = value
        .chars()
        .find(|character| !matches!(character, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-'))
    {
        return Err(Error::validation(format!(
            "A tag `{part}` can only contain ASCII letters, numbers, underscores or dashes, and this one contains {offender:?}."
        )));
    }

    Ok(())
}

/// An attachment needs bytes or a URL to get them from.
fn validate_attachment(attachment: &Attachment) -> Result<()> {
    match &attachment.source {
        AttachmentSource::Content(content) if content.is_empty() => Err(Error::InvalidAttachment),
        AttachmentSource::Path(path) if path.trim().is_empty() => Err(Error::InvalidAttachment),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod send_email {
    use bytes::Bytes;

    use super::{MAX_RECIPIENTS, MAX_TAG_LENGTH, SendEmail};
    use crate::error::Error;
    use crate::model::{Attachment, AttachmentSource, Tag};

    fn valid() -> SendEmail {
        let mut request = SendEmail::new(
            "Acme <onboarding@example.com>",
            vec!["delivered@example.org".to_owned()],
            "Hello World",
        );
        request.text = Some("It works".to_owned());
        request
    }

    fn attachment(source: AttachmentSource) -> Attachment {
        Attachment {
            filename: Some("invoice.pdf".to_owned()),
            content_type: None,
            content_id: None,
            source,
        }
    }

    #[test]
    fn should_accept_a_minimal_request() {
        assert!(valid().validate().is_ok());
    }

    #[test]
    fn should_accept_a_bare_address_without_a_display_name() {
        let mut request = valid();
        request.from = "onboarding@example.com".to_owned();

        assert!(request.validate().is_ok());
    }

    #[test]
    fn should_accept_html_instead_of_text() {
        let mut request = valid();
        request.text = None;
        request.html = Some("<p>It works</p>".to_owned());

        assert!(request.validate().is_ok());
    }

    #[test]
    fn should_report_every_missing_required_field_at_once() {
        let request = SendEmail::default();

        let Err(Error::MissingRequiredField { fields }) = request.validate() else {
            panic!("an empty request is missing from, to and subject");
        };

        assert_eq!(fields, ["from", "to", "subject"]);
    }

    #[test]
    fn should_treat_a_whitespace_only_subject_as_missing() {
        let mut request = valid();
        request.subject = "   ".to_owned();

        let Err(Error::MissingRequiredField { fields }) = request.validate() else {
            panic!("a blank subject is a missing subject");
        };

        assert_eq!(fields, ["subject"]);
    }

    #[test]
    fn should_reject_a_request_with_no_body_at_all() {
        let mut request = valid();
        request.text = None;

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_accept_a_template_as_the_body() {
        let mut request = valid();
        request.text = None;
        request.template = Some("welcome".to_owned());

        assert!(request.validate().is_ok());
    }

    #[test]
    fn should_reject_a_template_combined_with_a_body() {
        let mut request = valid();
        request.template = Some("welcome".to_owned());

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_accept_exactly_fifty_recipients() {
        let mut request = valid();
        request.to = (0..MAX_RECIPIENTS)
            .map(|index| format!("recipient-{index}@example.org"))
            .collect();

        assert!(request.validate().is_ok());
    }

    #[test]
    fn should_reject_more_than_fifty_recipients() {
        let mut request = valid();
        request.to = (0..=MAX_RECIPIENTS)
            .map(|index| format!("recipient-{index}@example.org"))
            .collect();

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_reject_an_address_without_an_at_sign() {
        let mut request = valid();
        request.to = vec!["not-an-address".to_owned()];

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_reject_an_address_with_nothing_before_the_at_sign() {
        let mut request = valid();
        request.to = vec!["@example.org".to_owned()];

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_reject_an_address_with_nothing_after_the_at_sign() {
        let mut request = valid();
        request.to = vec!["nobody@".to_owned()];

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_reject_a_display_name_with_an_empty_angle_bracket_address() {
        let mut request = valid();
        request.from = "Acme <>".to_owned();

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_validate_cc_bcc_and_reply_to_as_well_as_to() {
        for field in ["cc", "bcc", "reply_to"] {
            let mut request = valid();
            let bad = vec!["broken".to_owned()];

            match field {
                "cc" => request.cc = bad,
                "bcc" => request.bcc = bad,
                _ => request.reply_to = bad,
            }

            assert!(
                matches!(request.validate(), Err(Error::Validation { .. })),
                "{field} should be validated like `to`"
            );
        }
    }

    #[test]
    fn should_accept_an_ordinary_custom_header() {
        let mut request = valid();
        let _inserted = request
            .headers
            .insert("X-Entity-Ref-ID".to_owned(), "abc-123".to_owned());

        assert!(request.validate().is_ok());
    }

    #[test]
    fn should_reject_a_header_value_containing_a_newline() {
        // The injection that let a caller forge `X-Mock-Resend-Api-Key` in the `.eml`
        // and move their own captured mail into another key's mailbox.
        for injected in [
            "ok\r\nX-Mock-Resend-Api-Key: re_victim",
            "ok\nX-Mock-Resend-Last-Event: delivered",
            "ok\rX-Mock-Resend-Id: 00000000-0000-4000-8000-000000000000",
        ] {
            let mut request = valid();
            let _inserted = request
                .headers
                .insert("X-Harmless".to_owned(), injected.to_owned());

            assert!(
                matches!(request.validate(), Err(Error::Validation { .. })),
                "{injected:?} should be refused"
            );
        }
    }

    #[test]
    fn should_reject_a_header_value_containing_a_null_byte() {
        let mut request = valid();
        let _inserted = request
            .headers
            .insert("X-Harmless".to_owned(), "ok\0more".to_owned());

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_allow_a_tab_in_a_header_value() {
        // A tab is how a long header is legally folded, so refusing it would reject
        // something real Resend accepts.
        let mut request = valid();
        let _inserted = request
            .headers
            .insert("X-Folded".to_owned(), "one\ttwo".to_owned());

        assert!(request.validate().is_ok());
    }

    #[test]
    fn should_reject_a_header_name_containing_a_newline_or_colon() {
        for name in ["X-Bad\r\nX-Worse", "X-Bad: value", "X-Bad\tTab"] {
            let mut request = valid();
            let _inserted = request.headers.insert(name.to_owned(), "value".to_owned());

            assert!(
                matches!(request.validate(), Err(Error::Validation { .. })),
                "{name:?} should be refused"
            );
        }
    }

    #[test]
    fn should_reject_an_empty_header_name() {
        let mut request = valid();
        let _inserted = request.headers.insert(String::new(), "value".to_owned());

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_accept_a_tag_of_letters_numbers_underscores_and_dashes() {
        let mut request = valid();
        request.tags = vec![Tag {
            name: "category_1".to_owned(),
            value: "confirm-email".to_owned(),
        }];

        assert!(request.validate().is_ok());
    }

    #[test]
    fn should_reject_a_tag_name_containing_a_space() {
        let mut request = valid();
        request.tags = vec![Tag {
            name: "two words".to_owned(),
            value: "fine".to_owned(),
        }];

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_reject_a_tag_value_containing_a_space() {
        let mut request = valid();
        request.tags = vec![Tag {
            name: "fine".to_owned(),
            value: "two words".to_owned(),
        }];

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_reject_a_non_ascii_tag_value() {
        let mut request = valid();
        request.tags = vec![Tag {
            name: "category".to_owned(),
            value: "confirmé".to_owned(),
        }];

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_reject_an_empty_tag_name() {
        let mut request = valid();
        request.tags = vec![Tag {
            name: String::new(),
            value: "value".to_owned(),
        }];

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_accept_a_tag_at_the_length_limit() {
        let mut request = valid();
        request.tags = vec![Tag {
            name: "a".repeat(MAX_TAG_LENGTH),
            value: "b".repeat(MAX_TAG_LENGTH),
        }];

        assert!(request.validate().is_ok());
    }

    #[test]
    fn should_reject_a_tag_over_the_length_limit() {
        let mut request = valid();
        request.tags = vec![Tag {
            name: "a".repeat(MAX_TAG_LENGTH + 1),
            value: "value".to_owned(),
        }];

        assert!(matches!(request.validate(), Err(Error::Validation { .. })));
    }

    #[test]
    fn should_accept_an_attachment_with_content() {
        let mut request = valid();
        request.attachments = vec![attachment(AttachmentSource::Content(Bytes::from_static(
            b"%PDF-1.4",
        )))];

        assert!(request.validate().is_ok());
    }

    #[test]
    fn should_accept_an_attachment_with_a_path() {
        let mut request = valid();
        request.attachments = vec![attachment(AttachmentSource::Path(
            "https://example.com/invoice.pdf".to_owned(),
        ))];

        assert!(request.validate().is_ok());
    }

    #[test]
    fn should_reject_an_attachment_with_empty_content() {
        let mut request = valid();
        request.attachments = vec![attachment(AttachmentSource::Content(Bytes::new()))];

        assert!(matches!(request.validate(), Err(Error::InvalidAttachment)));
    }

    #[test]
    fn should_reject_an_attachment_with_a_blank_path() {
        let mut request = valid();
        request.attachments = vec![attachment(AttachmentSource::Path("  ".to_owned()))];

        assert!(matches!(request.validate(), Err(Error::InvalidAttachment)));
    }

    #[test]
    fn should_not_reject_a_traversal_in_an_attachment_filename() {
        // The filename is an arbitrary string, and real Resend accepts it. It is made
        // safe where it becomes a path, in `store::resolve_in`, not by refusing the send.
        let mut request = valid();
        let mut hostile = attachment(AttachmentSource::Content(Bytes::from_static(b"x")));
        hostile.filename = Some("../../etc/passwd".to_owned());
        request.attachments = vec![hostile];

        assert!(request.validate().is_ok());
    }
}

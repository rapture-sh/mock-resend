//! The `/emails` request and response bodies.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use chrono::{DateTime, SecondsFormat, Utc};
use mock_resend_core::{
    Attachment, AttachmentSource, Error as EngineError, Message, SendEmail, Tag,
};
use serde::{Deserialize, Serialize};

use crate::wire::{OneOrMany, addresses};

/// The body of `POST /emails`.
///
/// Unknown fields are ignored rather than rejected: Resend accepts them, and a mock
/// that refuses a payload the real API takes fails a test for a reason the code under
/// test is not responsible for.
#[derive(Debug, Clone, Deserialize)]
pub struct SendEmailRequest {
    /// Sender, as `address` or `Name <address>`.
    #[serde(default)]
    pub from: Option<String>,
    /// Recipients. A string or an array of strings.
    #[serde(default)]
    pub to: Option<OneOrMany>,
    /// Subject line.
    #[serde(default)]
    pub subject: Option<String>,
    /// HTML body.
    #[serde(default)]
    pub html: Option<String>,
    /// Plain text body.
    #[serde(default)]
    pub text: Option<String>,
    /// Carbon copy recipients.
    #[serde(default)]
    pub cc: Option<OneOrMany>,
    /// Blind carbon copy recipients.
    #[serde(default)]
    pub bcc: Option<OneOrMany>,
    /// Addresses replies should go to.
    #[serde(default)]
    pub reply_to: Option<OneOrMany>,
    /// Custom headers.
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    /// Attached files.
    #[serde(default)]
    pub attachments: Option<Vec<AttachmentRequest>>,
    /// Reporting tags.
    #[serde(default)]
    pub tags: Option<Vec<TagRequest>>,
    /// The template to render.
    ///
    /// An object, not a bare id. Template rendering is out of scope, so the id is
    /// recorded and the variables are accepted and discarded.
    #[serde(default)]
    pub template: Option<TemplateRequest>,
    /// When to send, in ISO 8601.
    #[serde(default)]
    pub scheduled_at: Option<String>,
}

/// A `template` object on a send request.
#[derive(Debug, Clone, Deserialize)]
pub struct TemplateRequest {
    /// The template id.
    pub id: String,
    /// Values the template would interpolate. Accepted and discarded.
    #[serde(default)]
    pub variables: Option<serde_json::Value>,
}

/// A `tags` entry on a send request.
#[derive(Debug, Clone, Deserialize)]
pub struct TagRequest {
    /// The tag name.
    pub name: String,
    /// The tag value.
    pub value: String,
}

/// An `attachments` entry on a send request.
#[derive(Debug, Clone, Deserialize)]
pub struct AttachmentRequest {
    /// The bytes, as base64 or as an array of byte values.
    #[serde(default)]
    pub content: Option<AttachmentContent>,
    /// A URL to fetch the bytes from. **Never fetched here.**
    #[serde(default)]
    pub path: Option<String>,
    /// The caller-supplied filename. An arbitrary string, never a path.
    #[serde(default)]
    pub filename: Option<String>,
    /// Declared content type.
    #[serde(default)]
    pub content_type: Option<String>,
    /// Marks the attachment inline, for `cid:` reference from the HTML body.
    #[serde(default)]
    pub content_id: Option<String>,
}

/// Attachment bytes, in either form a Resend SDK sends them.
///
/// The Node SDK sends a base64 string; the Rust SDK serialises `Vec<u8>`, which lands
/// as a JSON array of numbers. Both have to work.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AttachmentContent {
    /// Base64, as the Node SDK sends it.
    Base64(String),
    /// Raw byte values, as `resend-rs` sends them.
    Bytes(Vec<u8>),
}

impl AttachmentContent {
    /// Decodes to bytes.
    ///
    /// # Errors
    ///
    /// [`EngineError::InvalidAttachment`] when a string is not valid base64.
    fn decode(self) -> Result<Bytes, EngineError> {
        match self {
            Self::Base64(encoded) => BASE64
                .decode(encoded.as_bytes())
                .map(Bytes::from)
                .map_err(|_| EngineError::InvalidAttachment),
            Self::Bytes(bytes) => Ok(Bytes::from(bytes)),
        }
    }
}

impl SendEmailRequest {
    /// Converts the parsed body into the engine's request.
    ///
    /// Only shape conversion happens here. Whether the result is acceptable is the
    /// engine's decision, so that the same rules apply to a message arriving over SMTP.
    ///
    /// # Errors
    ///
    /// [`EngineError::InvalidAttachment`] for undecodable attachment content, and
    /// [`EngineError::Validation`] for a `scheduled_at` that is not a timestamp.
    pub fn into_engine_request(self) -> Result<SendEmail, EngineError> {
        let scheduled_at = self
            .scheduled_at
            .as_deref()
            .map(parse_scheduled_at)
            .transpose()?;

        let mut attachments = Vec::new();
        for attachment in self.attachments.unwrap_or_default() {
            attachments.push(attachment.into_engine_attachment()?);
        }

        let template = self.template.map(|template| template.id);

        Ok(SendEmail {
            from: self.from.unwrap_or_default(),
            to: addresses(self.to),
            subject: self.subject.unwrap_or_default(),
            html: self.html,
            text: self.text,
            cc: addresses(self.cc),
            bcc: addresses(self.bcc),
            reply_to: addresses(self.reply_to),
            headers: self.headers.unwrap_or_default(),
            attachments,
            tags: self
                .tags
                .unwrap_or_default()
                .into_iter()
                .map(|tag| Tag {
                    name: tag.name,
                    value: tag.value,
                })
                .collect(),
            template,
            scheduled_at,
        })
    }
}

impl AttachmentRequest {
    /// Converts one attachment, deciding between `content` and `path`.
    fn into_engine_attachment(self) -> Result<Attachment, EngineError> {
        // `content` wins when both are given, because it needs no network at all.
        let source = match (self.content, self.path) {
            (Some(content), _) => AttachmentSource::Content(content.decode()?),
            (None, Some(path)) => AttachmentSource::Path(path),
            (None, None) => return Err(EngineError::InvalidAttachment),
        };

        Ok(Attachment {
            filename: self.filename,
            content_type: self.content_type,
            content_id: self.content_id,
            source,
        })
    }
}

/// Parses `scheduled_at`.
///
/// Resend also accepts natural language here, such as `in 1 min`. That is not
/// implemented: an unparseable value is refused with `validation_error` rather than
/// silently treated as "now", because a scheduled send that goes out immediately is the
/// kind of wrong answer a test would believe.
fn parse_scheduled_at(value: &str) -> Result<DateTime<Utc>, EngineError> {
    DateTime::parse_from_rfc3339(value.trim())
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| {
            EngineError::validation(
                "`scheduled_at` must be an ISO 8601 timestamp, for example `2026-08-05T11:52:01.858Z`. Natural language scheduling is not supported by mock-resend.",
            )
        })
}

/// The body of a successful `POST /emails`.
#[derive(Debug, Clone, Serialize)]
pub struct SendEmailResponse {
    /// The id of the captured message.
    pub id: String,
}

/// The body of `GET /emails/{id}`.
#[derive(Debug, Clone, Serialize)]
pub struct EmailResponse {
    /// Always `email`.
    pub object: &'static str,
    /// The message id.
    pub id: String,
    /// The RFC 5322 `Message-ID`, absent while a send is still scheduled.
    pub message_id: Option<String>,
    /// Sender, as given.
    pub from: String,
    /// Recipients.
    pub to: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// When the message was captured.
    pub created_at: String,
    /// HTML body.
    pub html: Option<String>,
    /// Plain text body.
    pub text: Option<String>,
    /// Blind carbon copy recipients, `null` when there were none.
    pub bcc: Option<Vec<String>>,
    /// Carbon copy recipients, `null` when there were none.
    pub cc: Option<Vec<String>>,
    /// Reply-to addresses, `null` when there were none.
    pub reply_to: Option<Vec<String>>,
    /// The most recent delivery event.
    pub last_event: String,
    /// When the send is due, present only for a scheduled message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,
}

impl From<Message> for EmailResponse {
    fn from(message: Message) -> Self {
        Self {
            object: "email",
            id: message.id.to_string(),
            message_id: message.message_id,
            from: message.from,
            to: message.to,
            subject: message.subject,
            created_at: timestamp(message.created_at),
            html: message.html,
            text: message.text,
            bcc: non_empty(message.bcc),
            cc: non_empty(message.cc),
            reply_to: non_empty(message.reply_to),
            last_event: message.last_event.as_str().to_owned(),
            scheduled_at: message.scheduled_at.map(timestamp),
        }
    }
}

/// Serves an empty address list as `null` rather than `[]`.
///
/// Resend's documented example for an empty `bcc` is `[null]`, which no SDK can
/// deserialise into a list of strings — `resend_rs::types::Email` would reject it. `null`
/// is the reading of that both the Rust and Node SDKs accept, so it is what goes out
/// here. This is the one place the response shape is a judgement call rather than a
/// copy of the documented one.
fn non_empty(addresses: Vec<String>) -> Option<Vec<String>> {
    if addresses.is_empty() {
        None
    } else {
        Some(addresses)
    }
}

/// Formats a timestamp the way Resend does.
///
/// Microsecond precision with a `+00:00` offset rather than a `Z`, matching
/// `2023-04-03T22:13:42.674981+00:00` from the retrieve-email reference.
fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, false)
}

#[cfg(test)]
mod send_email_request {
    use super::{AttachmentContent, SendEmailRequest};
    use mock_resend_core::{AttachmentSource, Error as EngineError};

    fn parse(json: &str) -> SendEmailRequest {
        serde_json::from_str(json).expect("the fixture is valid json")
    }

    #[test]
    fn should_parse_the_payload_a_node_sdk_sends() {
        let request = parse(
            r#"{
                "from": "Acme <onboarding@example.com>",
                "to": "delivered@example.org",
                "subject": "Hello World",
                "html": "<p>It works</p>"
            }"#,
        );

        let engine = request.into_engine_request().expect("it converts");

        assert_eq!(engine.from, "Acme <onboarding@example.com>");
        assert_eq!(engine.to, ["delivered@example.org"]);
        assert_eq!(engine.subject, "Hello World");
        assert_eq!(engine.html.as_deref(), Some("<p>It works</p>"));
    }

    #[test]
    fn should_ignore_a_field_it_does_not_know() {
        let request = parse(
            r#"{
                "from": "a@example.com",
                "to": ["b@example.org"],
                "subject": "s",
                "text": "t",
                "some_field_resend_added_last_week": {"nested": true}
            }"#,
        );

        assert!(request.into_engine_request().is_ok());
    }

    #[test]
    fn should_collapse_every_string_or_array_field() {
        let request = parse(
            r#"{
                "from": "a@example.com",
                "to": "b@example.org",
                "cc": ["c@example.org"],
                "bcc": "d@example.org",
                "reply_to": ["e@example.org", "f@example.org"],
                "subject": "s",
                "text": "t"
            }"#,
        );

        let engine = request.into_engine_request().expect("it converts");

        assert_eq!(engine.to, ["b@example.org"]);
        assert_eq!(engine.cc, ["c@example.org"]);
        assert_eq!(engine.bcc, ["d@example.org"]);
        assert_eq!(engine.reply_to, ["e@example.org", "f@example.org"]);
    }

    #[test]
    fn should_record_a_template_id_from_the_template_object() {
        let request = parse(
            r#"{
                "from": "a@example.com",
                "to": "b@example.org",
                "subject": "s",
                "template": {"id": "welcome", "variables": {"name": "Ada"}}
            }"#,
        );

        let engine = request.into_engine_request().expect("it converts");

        assert_eq!(engine.template.as_deref(), Some("welcome"));
    }

    #[test]
    fn should_decode_base64_attachment_content() {
        let content = AttachmentContent::Base64("SGVsbG8=".to_owned());

        assert_eq!(content.decode().expect("valid base64").as_ref(), b"Hello");
    }

    #[test]
    fn should_accept_attachment_content_as_an_array_of_bytes() {
        let request = parse(
            r#"{
                "from": "a@example.com",
                "to": "b@example.org",
                "subject": "s",
                "text": "t",
                "attachments": [{"content": [72, 105], "filename": "hi.txt"}]
            }"#,
        );

        let engine = request.into_engine_request().expect("it converts");

        assert_eq!(
            engine.attachments[0].source,
            AttachmentSource::Content(bytes::Bytes::from_static(b"Hi"))
        );
    }

    #[test]
    fn should_reject_attachment_content_that_is_not_base64() {
        let content = AttachmentContent::Base64("not valid base64!!".to_owned());

        assert!(matches!(
            content.decode(),
            Err(EngineError::InvalidAttachment)
        ));
    }

    #[test]
    fn should_keep_an_attachment_path_without_fetching_it() {
        let request = parse(
            r#"{
                "from": "a@example.com",
                "to": "b@example.org",
                "subject": "s",
                "text": "t",
                "attachments": [{"path": "http://169.254.169.254/latest/meta-data/"}]
            }"#,
        );

        let engine = request.into_engine_request().expect("it converts");

        assert_eq!(
            engine.attachments[0].source,
            AttachmentSource::Path("http://169.254.169.254/latest/meta-data/".to_owned()),
            "the url is recorded verbatim; nothing in this crate may request it"
        );
    }

    #[test]
    fn should_reject_an_attachment_with_neither_content_nor_path() {
        let request = parse(
            r#"{
                "from": "a@example.com",
                "to": "b@example.org",
                "subject": "s",
                "text": "t",
                "attachments": [{"filename": "empty.txt"}]
            }"#,
        );

        assert!(matches!(
            request.into_engine_request(),
            Err(EngineError::InvalidAttachment)
        ));
    }

    #[test]
    fn should_parse_scheduled_at_in_iso_8601() {
        let request = parse(
            r#"{
                "from": "a@example.com",
                "to": "b@example.org",
                "subject": "s",
                "text": "t",
                "scheduled_at": "2026-08-05T11:52:01.858Z"
            }"#,
        );

        let engine = request.into_engine_request().expect("it converts");

        assert_eq!(
            engine
                .scheduled_at
                .expect("it parsed")
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "2026-08-05T11:52:01.858Z"
        );
    }

    #[test]
    fn should_reject_a_scheduled_at_that_is_not_a_timestamp() {
        let request = parse(
            r#"{
                "from": "a@example.com",
                "to": "b@example.org",
                "subject": "s",
                "text": "t",
                "scheduled_at": "in 1 min"
            }"#,
        );

        assert!(
            matches!(
                request.into_engine_request(),
                Err(EngineError::Validation { .. })
            ),
            "natural language scheduling is a documented divergence, not a silent `now`"
        );
    }
}

#[cfg(test)]
mod email_response {
    use std::collections::BTreeMap;

    use chrono::{TimeZone, Utc};
    use mock_resend_core::{ApiKey, EmailEvent, EmailId, Message};

    use super::EmailResponse;

    fn message() -> Message {
        Message {
            id: EmailId::generate(),
            key: ApiKey::parse("re_email_response_fixture").expect("valid"),
            from: "Acme <onboarding@example.com>".to_owned(),
            to: vec!["delivered@example.org".to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Hello World".to_owned(),
            html: Some("<p>It works</p>".to_owned()),
            text: None,
            headers: BTreeMap::new(),
            attachments: Vec::new(),
            tags: Vec::new(),
            template: None,
            scheduled_at: None,
            created_at: Utc
                .with_ymd_and_hms(2023, 4, 3, 22, 13, 42)
                .single()
                .expect("a real instant"),
            last_event: EmailEvent::Sent,
            message_id: Some("<id@mock-resend.invalid>".to_owned()),
        }
    }

    fn json(message: Message) -> serde_json::Value {
        serde_json::to_value(EmailResponse::from(message)).expect("it serialises")
    }

    #[test]
    fn should_report_the_object_as_email() {
        assert_eq!(json(message())["object"], "email");
    }

    #[test]
    fn should_report_created_at_with_microseconds_and_a_numeric_offset() {
        assert_eq!(
            json(message())["created_at"],
            "2023-04-03T22:13:42.000000+00:00"
        );
    }

    #[test]
    fn should_report_an_empty_recipient_list_as_null() {
        let json = json(message());

        assert!(json["bcc"].is_null());
        assert!(json["cc"].is_null());
        assert!(json["reply_to"].is_null());
    }

    #[test]
    fn should_report_a_populated_recipient_list_as_an_array() {
        let mut message = message();
        message.cc = vec!["cc@example.org".to_owned()];

        assert_eq!(json(message)["cc"], serde_json::json!(["cc@example.org"]));
    }

    #[test]
    fn should_report_an_absent_body_as_null_rather_than_omitting_it() {
        let json = json(message());

        assert!(
            json.get("text").is_some_and(serde_json::Value::is_null),
            "the SDK expects the key to exist"
        );
    }

    #[test]
    fn should_omit_scheduled_at_for_a_message_that_was_not_scheduled() {
        assert!(json(message()).get("scheduled_at").is_none());
    }

    #[test]
    fn should_include_scheduled_at_for_a_scheduled_message() {
        let mut message = message();
        message.scheduled_at = Some(
            Utc.with_ymd_and_hms(2026, 8, 5, 11, 52, 1)
                .single()
                .expect("a real instant"),
        );
        message.last_event = EmailEvent::Scheduled;

        let json = json(message);

        assert_eq!(json["last_event"], "scheduled");
        assert_eq!(json["scheduled_at"], "2026-08-05T11:52:01.000000+00:00");
    }

    #[test]
    fn should_report_last_event_in_resends_snake_case() {
        let mut message = message();
        message.last_event = EmailEvent::DeliveryDelayed;

        assert_eq!(json(message)["last_event"], "delivery_delayed");
    }
}

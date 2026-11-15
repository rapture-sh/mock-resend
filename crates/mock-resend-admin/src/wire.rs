//! Request and response types for the `/_admin` surface.
//!
//! These are mock-resend's own shapes, not Resend's, so they are free to be convenient.
//! The one convention they keep is Resend's: the API key is the namespace everything
//! happens in, and an endpoint that can act on more than one key says so by taking the
//! key as optional rather than by having a second endpoint.

use std::collections::BTreeMap;

use mock_resend_core::{
    ApiKey, EmailEvent, Error as EngineError, FaultTarget, Message, Stats, WebhookEndpoint,
};
use serde::{Deserialize, Serialize};

/// A body naming the key to act on, or none to act on every key.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScopeRequest {
    /// The key, or absent for all of them.
    #[serde(default)]
    pub api_key: Option<String>,
}

impl ScopeRequest {
    /// Parses the key, if one was given.
    ///
    /// # Errors
    ///
    /// [`EngineError::MissingApiKey`] when the value is present but not a Resend key.
    pub fn key(&self) -> Result<Option<ApiKey>, EngineError> {
        self.api_key.as_deref().map(ApiKey::parse).transpose()
    }
}

/// A query naming the key to read.
#[derive(Debug, Clone, Deserialize)]
pub struct KeyQuery {
    /// The key. Required: reading needs a namespace to read from.
    pub api_key: String,
}

impl KeyQuery {
    /// Parses the key.
    ///
    /// # Errors
    ///
    /// [`EngineError::MissingApiKey`] when it is not a Resend key.
    pub fn key(&self) -> Result<ApiKey, EngineError> {
        ApiKey::parse(&self.api_key)
    }
}

/// What a reset did.
#[derive(Debug, Clone, Serialize)]
pub struct ResetResponse {
    /// `all`, or the key that was reset.
    pub scope: String,
}

/// A captured message, in full, for someone inspecting a mailbox.
///
/// Unlike Resend's retrieve response this includes the parts Resend does not report
/// back — headers, tags, attachment metadata — because inspecting them is the reason
/// the admin API exists.
#[derive(Debug, Clone, Serialize)]
pub struct CapturedEmail {
    /// The message id.
    pub id: String,
    /// The RFC 5322 `Message-ID`.
    pub message_id: Option<String>,
    /// Sender, as given.
    pub from: String,
    /// Recipients.
    pub to: Vec<String>,
    /// Carbon copy recipients.
    pub cc: Vec<String>,
    /// Blind carbon copy recipients.
    pub bcc: Vec<String>,
    /// Reply-to addresses.
    pub reply_to: Vec<String>,
    /// The subject line.
    pub subject: String,
    /// The HTML body.
    pub html: Option<String>,
    /// The plain text body.
    pub text: Option<String>,
    /// Caller-supplied headers.
    pub headers: BTreeMap<String, String>,
    /// Tags, as the array the request used.
    pub tags: Vec<TagView>,
    /// Attachment metadata. The bytes are reported as a length, not inline: a mailbox
    /// listing is not the place to hand back a 40MB attachment.
    pub attachments: Vec<AttachmentView>,
    /// The template id, if one was named.
    pub template: Option<String>,
    /// When the send is due, for a scheduled message.
    pub scheduled_at: Option<String>,
    /// When it was captured.
    pub created_at: String,
    /// Its most recent delivery event.
    pub last_event: String,
}

/// A tag, as the send request wrote it.
#[derive(Debug, Clone, Serialize)]
pub struct TagView {
    /// The tag name.
    pub name: String,
    /// The tag value.
    pub value: String,
}

/// What is known about an attachment, without its bytes.
#[derive(Debug, Clone, Serialize)]
pub struct AttachmentView {
    /// The caller-supplied filename, exactly as sent. Never resolved to a path here.
    pub filename: Option<String>,
    /// The declared content type.
    pub content_type: Option<String>,
    /// The inline content id, if it was marked inline.
    pub content_id: Option<String>,
    /// `content` or `path`, naming where the bytes would come from.
    pub source: &'static str,
    /// How many bytes were supplied inline, for a `content` attachment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<usize>,
    /// The URL the bytes would come from, for a `path` attachment. Reported, never
    /// fetched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl From<Message> for CapturedEmail {
    fn from(message: Message) -> Self {
        Self {
            id: message.id.to_string(),
            message_id: message.message_id,
            from: message.from,
            to: message.to,
            cc: message.cc,
            bcc: message.bcc,
            reply_to: message.reply_to,
            subject: message.subject,
            html: message.html,
            text: message.text,
            headers: message.headers,
            tags: message
                .tags
                .into_iter()
                .map(|tag| TagView {
                    name: tag.name,
                    value: tag.value,
                })
                .collect(),
            attachments: message
                .attachments
                .into_iter()
                .map(AttachmentView::from)
                .collect(),
            template: message.template,
            scheduled_at: message.scheduled_at.map(crate::timestamp),
            created_at: crate::timestamp(message.created_at),
            last_event: message.last_event.as_str().to_owned(),
        }
    }
}

impl From<mock_resend_core::Attachment> for AttachmentView {
    fn from(attachment: mock_resend_core::Attachment) -> Self {
        let (source, bytes, path) = match attachment.source {
            mock_resend_core::AttachmentSource::Content(content) => {
                ("content", Some(content.len()), None)
            }
            mock_resend_core::AttachmentSource::Path(path) => ("path", None, Some(path)),
        };

        Self {
            filename: attachment.filename,
            content_type: attachment.content_type,
            content_id: attachment.content_id,
            source,
            bytes,
            path,
        }
    }
}

/// A list of captured mail.
#[derive(Debug, Clone, Serialize)]
pub struct EmailListResponse {
    /// How many there are.
    pub count: usize,
    /// The messages, oldest first.
    pub data: Vec<CapturedEmail>,
}

/// A request to move a message to another delivery event.
#[derive(Debug, Clone, Deserialize)]
pub struct AdvanceEventRequest {
    /// The key that owns the message.
    pub api_key: String,
    /// The event to move it to, in Resend's wire form.
    pub event: String,
}

impl AdvanceEventRequest {
    /// Parses the key and the event.
    ///
    /// # Errors
    ///
    /// [`EngineError::MissingApiKey`] for a bad key, and [`EngineError::Validation`] for
    /// an event name that is not one Resend reports.
    pub fn parse(&self) -> Result<(ApiKey, EmailEvent), EngineError> {
        let key = ApiKey::parse(&self.api_key)?;

        let event = EmailEvent::parse(self.event.trim()).ok_or_else(|| {
            EngineError::validation(format!(
                "`{}` is not a delivery event Resend reports.",
                self.event
            ))
        })?;

        Ok((key, event))
    }
}

/// What a transition did.
#[derive(Debug, Clone, Serialize)]
pub struct TransitionResponse {
    /// The message that moved.
    pub id: String,
    /// The event it was at.
    pub from: String,
    /// The event it is at now.
    pub to: String,
    /// When it moved, by the engine's clock.
    pub at: String,
}

/// A request to move the clock.
#[derive(Debug, Clone, Deserialize)]
pub struct AdvanceClockRequest {
    /// How far forward to move it, in seconds.
    #[serde(default)]
    pub seconds: Option<i64>,
    /// How far forward to move it, in milliseconds. Added to `seconds`.
    #[serde(default)]
    pub milliseconds: Option<i64>,
}

impl AdvanceClockRequest {
    /// The total jump.
    ///
    /// # Errors
    ///
    /// [`EngineError::Validation`] when nothing was given, or the jump is not forward.
    pub fn duration(&self) -> Result<chrono::Duration, EngineError> {
        if self.seconds.is_none() && self.milliseconds.is_none() {
            return Err(EngineError::validation(
                "Say how far to move the clock, with `seconds` or `milliseconds`.",
            ));
        }

        let seconds = chrono::Duration::seconds(self.seconds.unwrap_or(0));
        let milliseconds = chrono::Duration::milliseconds(self.milliseconds.unwrap_or(0));
        let total = seconds + milliseconds;

        if total <= chrono::Duration::zero() {
            return Err(EngineError::validation(
                "The clock only moves forward. Reset the key instead of rewinding it.",
            ));
        }

        Ok(total)
    }
}

/// What moving the clock did.
#[derive(Debug, Clone, Serialize)]
pub struct AdvanceClockResponse {
    /// The engine's time after the jump.
    pub now: String,
    /// The transitions the jump caused.
    pub transitions: Vec<TransitionResponse>,
}

/// A request to add a fault rule.
#[derive(Debug, Clone, Deserialize)]
pub struct FaultRequest {
    /// The key the rule belongs to.
    pub api_key: String,
    /// The Resend error name to inject. Must be one Resend actually has.
    pub error: String,
    /// The status to serve it with, or absent for the one Resend uses.
    #[serde(default)]
    pub status: Option<u16>,
    /// The message to serve, or absent for a generic one.
    #[serde(default)]
    pub message: Option<String>,
    /// Which requests it applies to: `send`, `retrieve` or `any`. Defaults to `any`.
    #[serde(default)]
    pub target: Option<String>,
    /// How many times it fires, or absent to fire until removed.
    #[serde(default)]
    pub times: Option<u32>,
}

impl FaultRequest {
    /// Parses the key and the target.
    ///
    /// # Errors
    ///
    /// [`EngineError::MissingApiKey`] for a bad key, and [`EngineError::Validation`] for
    /// an unknown target.
    pub fn parse(&self) -> Result<(ApiKey, FaultTarget), EngineError> {
        let key = ApiKey::parse(&self.api_key)?;

        let target = match self.target.as_deref() {
            None => FaultTarget::Any,
            Some(value) => FaultTarget::parse(value).ok_or_else(|| {
                EngineError::validation(format!(
                    "`{value}` is not a fault target. Use `send`, `retrieve` or `any`."
                ))
            })?,
        };

        Ok((key, target))
    }
}

/// A request to change what delivery does under a key.
#[derive(Debug, Clone, Deserialize)]
pub struct DeliveryRequest {
    /// The key the rule belongs to.
    pub api_key: String,
    /// The event a sent message lands on instead of `delivered`, or absent to go back
    /// to delivering normally.
    #[serde(default)]
    pub outcome: Option<String>,
    /// How long after sending delivery happens, in milliseconds. Absent leaves the
    /// instance default.
    #[serde(default)]
    pub delay_ms: Option<i64>,
}

/// A request to register a webhook endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookRequest {
    /// The key the endpoint belongs to.
    pub api_key: String,
    /// Where to post. Checked against the outbound policy at delivery time.
    pub url: String,
    /// Which events to send, or empty for all of them.
    #[serde(default)]
    pub events: Vec<String>,
}

impl WebhookRequest {
    /// Parses the key and the event list.
    ///
    /// # Errors
    ///
    /// [`EngineError::MissingApiKey`] for a bad key, and [`EngineError::Validation`] for
    /// an event name that is not one Resend reports.
    pub fn parse(&self) -> Result<(ApiKey, Vec<EmailEvent>), EngineError> {
        let key = ApiKey::parse(&self.api_key)?;

        let mut events = Vec::with_capacity(self.events.len());
        for event in &self.events {
            // `email.delivered` and `delivered` are both accepted: the first is what a
            // webhook payload calls it and the second is what `last_event` calls it, and
            // nobody should have to remember which this endpoint wants.
            let name = event.trim().strip_prefix("email.").unwrap_or(event.trim());

            events.push(EmailEvent::parse(name).ok_or_else(|| {
                EngineError::validation(format!(
                    "`{event}` is not a delivery event Resend reports."
                ))
            })?);
        }

        Ok((key, events))
    }
}

/// A registered endpoint, as the admin API reports it.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookView {
    /// The endpoint id.
    pub id: String,
    /// Where it posts.
    pub url: String,
    /// Which events it wants, or empty for all of them.
    pub events: Vec<String>,
    /// Its signing secret, so a test can verify what it receives.
    ///
    /// Reported in full, which a real API would never do. Nothing here is a secret, and
    /// a test that cannot read it cannot check a signature.
    pub signing_secret: String,
    /// When it was registered.
    pub created_at: String,
}

impl From<WebhookEndpoint> for WebhookView {
    fn from(endpoint: WebhookEndpoint) -> Self {
        Self {
            id: endpoint.id.to_string(),
            url: endpoint.url,
            events: endpoint
                .events
                .into_iter()
                .filter_map(|event| event.webhook_name())
                .map(str::to_owned)
                .collect(),
            signing_secret: endpoint.secret.as_str().to_owned(),
            created_at: crate::timestamp(endpoint.created_at),
        }
    }
}

/// A list of registered endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookListResponse {
    /// How many there are.
    pub count: usize,
    /// The endpoints, in registration order.
    pub data: Vec<WebhookView>,
}

/// What a key has done.
#[derive(Debug, Clone, Serialize)]
pub struct StatsResponse {
    /// The key, as given.
    pub api_key: String,
    /// How many messages it has captured.
    pub captured: usize,
    /// How many are at each delivery event.
    pub by_event: BTreeMap<String, usize>,
    /// How many webhook endpoints are registered.
    pub webhook_endpoints: usize,
    /// How many fault rules are waiting to fire.
    pub fault_rules: usize,
}

impl StatsResponse {
    /// Builds a response for `key`.
    #[must_use]
    pub fn new(key: &ApiKey, stats: Stats) -> Self {
        Self {
            api_key: key.as_str().to_owned(),
            captured: stats.captured,
            by_event: stats
                .by_event
                .into_iter()
                .map(|(event, count)| (event.to_owned(), count))
                .collect(),
            webhook_endpoints: stats.webhook_endpoints,
            fault_rules: stats.fault_rules,
        }
    }
}

/// The liveness response.
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    /// Always `ok`.
    pub status: &'static str,
    /// The version this instance is running.
    pub version: &'static str,
    /// The engine's current time, which may be ahead of the wall clock.
    pub now: String,
}

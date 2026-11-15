//! The `email.*` webhook payload.
//!
//! The shape is Resend's, confirmed against `resend_rs::events`, whose fixtures are
//! copies of the documented payloads. Two things about it are surprising enough to be
//! worth stating plainly, because both look like bugs:
//!
//! - **`tags` is an object here**, `{"category": "confirm_email"}`, while the send
//!   request takes an array of `{name, value}`. The API really is asymmetric.
//! - **`created_at` carries milliseconds and a `Z`**, `2024-02-22T23:41:12.126Z`, while
//!   `GET /emails/{id}` reports microseconds and a `+00:00` offset. Two formats, one
//!   API.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use mock_resend_core::{EmailEvent, Message};
use serde::Serialize;

/// A webhook body.
#[derive(Debug, Clone, Serialize)]
pub struct Payload {
    /// The event name, such as `email.delivered`.
    #[serde(rename = "type")]
    pub event_type: String,
    /// When the event happened.
    pub created_at: String,
    /// The message it happened to.
    pub data: Data,
}

/// The `data` object of a webhook body.
#[derive(Debug, Clone, Serialize)]
pub struct Data {
    /// The broadcast this came from. Always `null` here: broadcasts are out of scope.
    pub broadcast_id: Option<String>,
    /// When the message was captured.
    pub created_at: String,
    /// The message id.
    pub email_id: String,
    /// The RFC 5322 `Message-ID`.
    pub message_id: String,
    /// Sender, as given.
    pub from: String,
    /// Recipients.
    pub to: Vec<String>,
    /// The subject line.
    pub subject: String,
    /// The template the message named, if any.
    pub template_id: Option<String>,
    /// The message's tags, as an object rather than the array the request used.
    pub tags: BTreeMap<String, String>,
    /// Why it bounced. Present only on `email.bounced`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounce: Option<Bounce>,
    /// Why it failed. Present only on `email.failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<Failed>,
}

/// The `bounce` object on `email.bounced`.
#[derive(Debug, Clone, Serialize)]
pub struct Bounce {
    /// What happened, in words.
    pub message: String,
    /// The bounce sub-type. camelCase on the wire, unlike everything around it.
    #[serde(rename = "subType")]
    pub sub_type: String,
    /// `Permanent` or `Transient`.
    #[serde(rename = "type")]
    pub kind: String,
}

/// The `failed` object on `email.failed`.
#[derive(Debug, Clone, Serialize)]
pub struct Failed {
    /// Why the send failed.
    pub reason: String,
}

/// Builds the payload for a transition, or `None` if Resend has no webhook for it.
#[must_use]
pub fn build(message: &Message, event: EmailEvent, at: DateTime<Utc>) -> Option<Payload> {
    let event_type = event.webhook_name()?;

    let data = Data {
        broadcast_id: None,
        created_at: timestamp(message.created_at),
        email_id: message.id.to_string(),
        // The SDK's type has this non-optional, and the documented `email.scheduled`
        // payload carries one, so every captured message is given one at capture.
        message_id: message.message_id.clone().unwrap_or_default(),
        from: message.from.clone(),
        to: message.to.clone(),
        subject: message.subject.clone(),
        template_id: message.template.clone(),
        tags: message
            .tags
            .iter()
            .map(|tag| (tag.name.clone(), tag.value.clone()))
            .collect(),
        bounce: (event == EmailEvent::Bounced).then(|| Bounce {
            message: "The recipient's mail server rejected the message. Injected by mock-resend."
                .to_owned(),
            sub_type: "General".to_owned(),
            kind: "Permanent".to_owned(),
        }),
        failed: (event == EmailEvent::Failed).then(|| Failed {
            reason: "The message could not be sent. Injected by mock-resend.".to_owned(),
        }),
    };

    Some(Payload {
        event_type: event_type.to_owned(),
        created_at: timestamp(at),
        data,
    })
}

/// Formats a timestamp the way a webhook payload does.
///
/// Milliseconds and a `Z`, which is **not** the format `GET /emails/{id}` uses.
fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod build {
    use std::collections::BTreeMap;

    use chrono::{TimeZone, Utc};
    use mock_resend_core::{ApiKey, EmailEvent, EmailId, Message, Tag};

    use super::build;

    fn message() -> Message {
        Message {
            id: EmailId::generate(),
            key: ApiKey::parse("re_payload_fixture").expect("valid"),
            from: "Acme <onboarding@example.com>".to_owned(),
            to: vec!["delivered@example.org".to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Sending this example".to_owned(),
            html: Some("<p>It works</p>".to_owned()),
            text: None,
            headers: BTreeMap::new(),
            attachments: Vec::new(),
            tags: vec![Tag {
                name: "category".to_owned(),
                value: "confirm_email".to_owned(),
            }],
            template: None,
            scheduled_at: None,
            created_at: Utc
                .with_ymd_and_hms(2024, 2, 22, 23, 41, 11)
                .single()
                .expect("a real instant"),
            last_event: EmailEvent::Sent,
            message_id: Some("<111-222-333@example.com>".to_owned()),
        }
    }

    fn json(event: EmailEvent) -> serde_json::Value {
        let at = Utc
            .with_ymd_and_hms(2024, 2, 22, 23, 41, 12)
            .single()
            .expect("a real instant");

        let payload = build(&message(), event, at).expect("this event has a webhook");

        serde_json::to_value(payload).expect("it serialises")
    }

    #[test]
    fn should_name_the_event_the_way_resend_does() {
        assert_eq!(
            json(EmailEvent::DeliveryDelayed)["type"],
            "email.delivery_delayed"
        );
    }

    #[test]
    fn should_report_tags_as_an_object_not_an_array() {
        // The request takes `[{"name": "category", "value": "confirm_email"}]`; the
        // webhook reports `{"category": "confirm_email"}`. The API is asymmetric here.
        assert_eq!(
            json(EmailEvent::Sent)["data"]["tags"],
            serde_json::json!({"category": "confirm_email"})
        );
    }

    #[test]
    fn should_report_timestamps_with_milliseconds_and_a_z() {
        let json = json(EmailEvent::Sent);

        assert_eq!(json["created_at"], "2024-02-22T23:41:12.000Z");
        assert_eq!(json["data"]["created_at"], "2024-02-22T23:41:11.000Z");
    }

    #[test]
    fn should_carry_the_message_id() {
        assert_eq!(
            json(EmailEvent::Sent)["data"]["message_id"],
            "<111-222-333@example.com>"
        );
    }

    #[test]
    fn should_carry_every_field_the_sdk_requires() {
        let data = json(EmailEvent::Sent);
        let data = &data["data"];

        for field in [
            "broadcast_id",
            "created_at",
            "email_id",
            "message_id",
            "from",
            "to",
            "subject",
            "template_id",
            "tags",
        ] {
            assert!(
                data.get(field).is_some(),
                "{field} is required by resend_rs::events::EmailBody"
            );
        }
    }

    #[test]
    fn should_add_a_bounce_object_only_to_a_bounce() {
        assert!(json(EmailEvent::Bounced)["data"]["bounce"]["subType"].is_string());
        assert!(json(EmailEvent::Sent)["data"].get("bounce").is_none());
    }

    #[test]
    fn should_add_a_failed_object_only_to_a_failure() {
        assert!(json(EmailEvent::Failed)["data"]["failed"]["reason"].is_string());
        assert!(json(EmailEvent::Sent)["data"].get("failed").is_none());
    }

    #[test]
    fn should_build_nothing_for_an_event_resend_has_no_webhook_for() {
        let at = Utc::now();

        assert!(build(&message(), EmailEvent::Canceled, at).is_none());
        assert!(build(&message(), EmailEvent::Queued, at).is_none());
    }
}

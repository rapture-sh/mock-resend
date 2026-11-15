//! The `.eml` on disk, and reading it back.
//!
//! A captured message is written as a real RFC 5322 message so a developer can open it
//! in a mail client and see what their code actually produced. That makes the `.eml` the
//! disk backend's record rather than a side artifact, which in turn means it has to
//! carry the things a mail message has no header for — which API key owns it, where it
//! is in the delivery lifecycle, its tags — in `X-Mock-Resend-*` headers.
//!
//! Those headers are stripped on the way back in, so they never reach a caller as
//! ordinary headers.
//!
//! The round trip is **semantic, not byte-for-byte**. An address is reconstructed into
//! `Name <addr>` form, a subject goes through RFC 2047 encoding and back, and a body is
//! encoded and decoded; what comes out equals what went in, but the bytes in between are
//! the MIME writer's business.

use std::collections::BTreeMap;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use mail_builder::headers::raw::Raw;
use mail_builder::mime::MimePart;
use mail_parser::MimeHeaders as _;

use crate::error::{Error, Result};
use crate::events::EmailEvent;
use crate::model::{ApiKey, Attachment, AttachmentSource, EmailId, Message, Tag};

/// The prefix for every header that carries mock-resend's own state.
pub const PRIVATE_PREFIX: &str = "X-Mock-Resend-";

/// Which API key owns the message.
const HEADER_KEY: &str = "X-Mock-Resend-Api-Key";

/// The message's own id, which is not the RFC `Message-ID`.
const HEADER_ID: &str = "X-Mock-Resend-Id";

/// Where the message is in the delivery lifecycle.
const HEADER_EVENT: &str = "X-Mock-Resend-Last-Event";

/// When the send is due, for a scheduled message.
const HEADER_SCHEDULED: &str = "X-Mock-Resend-Scheduled-At";

/// When the message was captured.
const HEADER_CREATED: &str = "X-Mock-Resend-Created-At";

/// The template the message named.
const HEADER_TEMPLATE: &str = "X-Mock-Resend-Template";

/// One tag, as `name=value`. Repeated once per tag.
const HEADER_TAG: &str = "X-Mock-Resend-Tag";

/// A `path` attachment, as `filename=url`. Repeated once per attachment.
///
/// An attachment that only ever had a URL has no bytes to write into a MIME part, so it
/// is recorded as a header instead. Writing a part with an empty body would turn "we
/// never fetched this" into "this file is empty", which is a different fact.
const HEADER_ATTACHMENT_PATH: &str = "X-Mock-Resend-Attachment-Path";

/// The content type used when an attachment did not declare one.
const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

/// Renders a captured message as an `.eml`.
///
/// # Errors
///
/// [`Error::Store`] if the MIME writer fails, which it only does on an I/O error
/// against the in-memory buffer.
pub fn to_eml(message: &Message) -> Result<Vec<u8>> {
    let mut builder = MessageBuilder::new()
        .from(address_of(&message.from))
        .subject(message.subject.as_str())
        .date(message.created_at.timestamp());

    if !message.to.is_empty() {
        builder = builder.to(addresses_of(&message.to));
    }
    if !message.cc.is_empty() {
        builder = builder.cc(addresses_of(&message.cc));
    }
    if !message.bcc.is_empty() {
        builder = builder.bcc(addresses_of(&message.bcc));
    }
    if !message.reply_to.is_empty() {
        builder = builder.reply_to(addresses_of(&message.reply_to));
    }
    // Always set, and never left to the writer. `mail-builder` generates a missing
    // `Message-ID` from the machine's hostname, which would write the developer's
    // computer name into every captured `.eml` on a mounted volume.
    builder = builder.message_id(message_id_body(message));

    for (name, value) in &message.headers {
        // A caller cannot smuggle state in: anything in the private namespace is
        // dropped here, so a header the engine reads back is one the engine wrote.
        if is_private(name) {
            continue;
        }

        builder = builder.header(name.clone(), Raw::new(value.clone()));
    }

    builder = builder
        .header(HEADER_KEY, Raw::new(message.key.as_str().to_owned()))
        .header(HEADER_ID, Raw::new(message.id.to_string()))
        .header(HEADER_EVENT, Raw::new(message.last_event.as_str()))
        .header(HEADER_CREATED, Raw::new(message.created_at.to_rfc3339()));

    if let Some(scheduled_at) = message.scheduled_at {
        builder = builder.header(HEADER_SCHEDULED, Raw::new(scheduled_at.to_rfc3339()));
    }
    if let Some(template) = &message.template {
        builder = builder.header(HEADER_TEMPLATE, Raw::new(template.clone()));
    }

    for tag in &message.tags {
        // Tag names and values are ASCII letters, digits, underscores and dashes, so
        // neither can contain the `=` that separates them or a newline that would end
        // the header.
        builder = builder.header(HEADER_TAG, Raw::new(format!("{}={}", tag.name, tag.value)));
    }

    if let Some(text) = &message.text {
        builder = builder.text_body(text.clone());
    }
    if let Some(html) = &message.html {
        builder = builder.html_body(html.clone());
    }

    // A message with neither body still has to be a valid MIME document.
    if message.text.is_none() && message.html.is_none() {
        builder = builder.text_body(String::new());
    }

    // Built by hand rather than through `MessageBuilder::attachment`, which takes a
    // filename, or `inline`, which takes a content id: an inline attachment needs both,
    // so that a `cid:` reference in the HTML resolves *and* a client that lists
    // attachments still shows a name.
    let mut parts = Vec::new();

    for attachment in &message.attachments {
        match &attachment.source {
            AttachmentSource::Content(content) => {
                let filename = attachment
                    .filename
                    .clone()
                    .unwrap_or_else(|| "attachment".to_owned());
                let content_type = attachment
                    .content_type
                    .clone()
                    .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_owned());

                let mut part = MimePart::new(content_type, content.to_vec()).attachment(filename);

                if let Some(content_id) = &attachment.content_id {
                    part = part.inline().cid(content_id.clone());
                }

                parts.push(part);
            }
            AttachmentSource::Path(path) => {
                let filename = attachment.filename.clone().unwrap_or_default();

                builder = builder.header(
                    HEADER_ATTACHMENT_PATH,
                    Raw::new(format!("{filename}={path}")),
                );
            }
        }
    }

    if !parts.is_empty() {
        builder.attachments = Some(parts);
    }

    builder
        .write_to_vec()
        .map_err(|error| Error::Store(format!("could not write the .eml: {error}")))
}

/// Reads an `.eml` back into a captured message.
///
/// # Errors
///
/// [`Error::Store`] when the bytes are not a parseable message, or when the
/// `X-Mock-Resend-*` headers that carry the engine's own state are missing. A file
/// without them was not written by this server.
pub fn from_eml(raw: &[u8]) -> Result<Message> {
    let parsed = mail_parser::MessageParser::default()
        .parse(raw)
        .ok_or_else(|| Error::Store("the .eml could not be parsed".to_owned()))?;

    let private = |name: &str| -> Option<String> {
        parsed.headers().iter().find_map(|header| {
            header
                .name()
                .eq_ignore_ascii_case(name)
                .then(|| raw_value(header))
                .flatten()
        })
    };

    let privates = |name: &str| -> Vec<String> {
        parsed
            .headers()
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case(name))
            .filter_map(raw_value)
            .collect()
    };

    let missing = |name: &str| Error::Store(format!("the .eml has no {name} header"));

    let key = ApiKey::parse(&private(HEADER_KEY).ok_or_else(|| missing(HEADER_KEY))?)
        .map_err(|_| Error::Store(format!("the .eml has an unusable {HEADER_KEY}")))?;

    let id: EmailId = private(HEADER_ID)
        .ok_or_else(|| missing(HEADER_ID))?
        .parse()
        .map_err(|_| Error::Store(format!("the .eml has an unusable {HEADER_ID}")))?;

    let last_event = private(HEADER_EVENT)
        .and_then(|value| EmailEvent::parse(&value))
        .ok_or_else(|| missing(HEADER_EVENT))?;

    let created_at = private(HEADER_CREATED)
        .as_deref()
        .and_then(parse_timestamp)
        .ok_or_else(|| missing(HEADER_CREATED))?;

    let scheduled_at = private(HEADER_SCHEDULED)
        .as_deref()
        .and_then(parse_timestamp);

    let tags = privates(HEADER_TAG)
        .into_iter()
        .filter_map(|entry| {
            entry.split_once('=').map(|(name, value)| Tag {
                name: name.to_owned(),
                value: value.to_owned(),
            })
        })
        .collect();

    let mut attachments: Vec<Attachment> = parsed
        .attachments()
        .map(|part| Attachment {
            filename: part.attachment_name().map(str::to_owned),
            content_type: part
                .content_type()
                .map(|content_type| match content_type.subtype() {
                    Some(subtype) => format!("{}/{subtype}", content_type.ctype()),
                    None => content_type.ctype().to_owned(),
                }),
            content_id: part.content_id().map(str::to_owned),
            source: AttachmentSource::Content(Bytes::from(part.contents().to_vec())),
        })
        .collect();

    for entry in privates(HEADER_ATTACHMENT_PATH) {
        let (filename, path) = entry.split_once('=').unwrap_or(("", entry.as_str()));

        attachments.push(Attachment {
            filename: (!filename.is_empty()).then(|| filename.to_owned()),
            content_type: None,
            content_id: None,
            source: AttachmentSource::Path(path.to_owned()),
        });
    }

    // Everything that is not an address header, a structural MIME header or one of ours.
    let headers: BTreeMap<String, String> = parsed
        .headers()
        .iter()
        .filter(|header| !is_private(header.name()) && !is_structural(header.name()))
        .filter_map(|header| raw_value(header).map(|value| (header.name().to_owned(), value)))
        .collect();

    // `body_text` and `body_html` will *synthesise* the other form when only one is
    // present — HTML stripped to text, or text wrapped in `<html>`. That would turn a
    // text-only message into one that also has an HTML body, so the parts are matched
    // by type instead.
    let text = parsed.text_bodies().find_map(|part| match &part.body {
        mail_parser::PartType::Text(text) => Some(text.as_ref().to_owned()),
        _ => None,
    });
    let html = parsed.html_bodies().find_map(|part| match &part.body {
        mail_parser::PartType::Html(html) => Some(html.as_ref().to_owned()),
        _ => None,
    });

    Ok(Message {
        id,
        key,
        from: first_address(parsed.from()).unwrap_or_default(),
        to: all_addresses(parsed.to()),
        cc: all_addresses(parsed.cc()),
        bcc: all_addresses(parsed.bcc()),
        reply_to: all_addresses(parsed.reply_to()),
        subject: parsed.subject().unwrap_or_default().to_owned(),
        // A message written with no body at all comes back with an empty text part, so
        // an empty string is restored as "there was no body".
        html: html.filter(|body| !body.is_empty()),
        text: text.filter(|body| !body.is_empty()),
        headers,
        attachments,
        tags,
        template: private(HEADER_TEMPLATE),
        scheduled_at,
        created_at,
        last_event,
        message_id: parsed.message_id().map(|id| format!("<{id}>")),
    })
}

/// The `Message-ID` body, without its angle brackets.
///
/// The writer adds the brackets. The fallback domain is `.invalid` on purpose: there is
/// no sending domain here, and a plausible-looking one in a header a test might assert
/// on is worse than an obviously fake one.
fn message_id_body(message: &Message) -> String {
    match &message.message_id {
        Some(message_id) => message_id
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_owned(),
        None => format!("{}@mock-resend.invalid", message.id),
    }
}

/// Whether a header belongs to mock-resend rather than to the message.
#[must_use]
pub fn is_private(name: &str) -> bool {
    name.len() >= PRIVATE_PREFIX.len()
        && name[..PRIVATE_PREFIX.len()].eq_ignore_ascii_case(PRIVATE_PREFIX)
}

/// Whether a header is one the MIME writer owns, rather than caller-supplied.
///
/// These are reconstructed from their own fields on the way back, so keeping them in the
/// header map too would duplicate them into every response.
fn is_structural(name: &str) -> bool {
    const STRUCTURAL: [&str; 13] = [
        "from",
        "to",
        "cc",
        "bcc",
        "reply-to",
        "subject",
        "date",
        "message-id",
        "mime-version",
        "content-type",
        "content-transfer-encoding",
        "content-disposition",
        "content-id",
    ];

    STRUCTURAL
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
}

/// A header's value as the text it was written as.
fn raw_value(header: &mail_parser::Header<'_>) -> Option<String> {
    match header.value() {
        mail_parser::HeaderValue::Text(text) => Some(text.trim().to_owned()),
        mail_parser::HeaderValue::TextList(list) => Some(list.join(", ")),
        _ => None,
    }
}

/// Parses an RFC 3339 timestamp.
fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

/// Splits `Name <addr>` into the pair the MIME writer wants.
fn address_of(value: &str) -> Address<'static> {
    let trimmed = value.trim();

    match (trimmed.rfind('<'), trimmed.rfind('>')) {
        (Some(open), Some(close)) if close > open => {
            let name = trimmed[..open].trim().trim_matches('"').to_owned();
            let email = trimmed[open + 1..close].trim().to_owned();

            if name.is_empty() {
                Address::new_address(None::<String>, email)
            } else {
                Address::new_address(Some(name), email)
            }
        }
        _ => Address::new_address(None::<String>, trimmed.to_owned()),
    }
}

/// The same for a list.
fn addresses_of(values: &[String]) -> Address<'static> {
    Address::new_list(values.iter().map(|value| address_of(value)).collect())
}

/// Reconstructs `Name <addr>`, or just the address.
fn render_address(address: &mail_parser::Addr<'_>) -> Option<String> {
    let email = address.address()?;

    Some(match address.name() {
        Some(name) if !name.trim().is_empty() => format!("{} <{email}>", name.trim()),
        _ => email.to_owned(),
    })
}

/// The first address of a parsed header.
fn first_address(address: Option<&mail_parser::Address<'_>>) -> Option<String> {
    all_addresses(address).into_iter().next()
}

/// Every address of a parsed header, in order.
fn all_addresses(address: Option<&mail_parser::Address<'_>>) -> Vec<String> {
    let Some(address) = address else {
        return Vec::new();
    };

    address
        .clone()
        .into_list()
        .iter()
        .filter_map(render_address)
        .collect()
}

/// The envelope an SMTP transaction carried, alongside the message itself.
///
/// SMTP keeps the envelope and the headers separate, and they genuinely differ: a `Bcc`
/// recipient is in the envelope and deliberately absent from the headers, which is the
/// whole point of a blind copy.
#[derive(Debug, Clone, Default)]
pub struct Envelope {
    /// `MAIL FROM`.
    pub sender: String,
    /// Every `RCPT TO`, in the order they arrived.
    pub recipients: Vec<String>,
}

/// Turns a message that arrived over SMTP into a send request.
///
/// The result goes through the same engine method an API send does, so a message that
/// arrives this way is held to the same validation and produces the same events.
///
/// Headers win over the envelope for what a reader sees, and the envelope fills in what
/// the headers do not say. Recipients that are in the envelope but in neither `To` nor
/// `Cc` are reconstructed as `Bcc`, which is where they came from.
///
/// # Errors
///
/// [`Error::Validation`] when the bytes are not a parseable message.
pub fn inbound_to_send(raw: &[u8], envelope: &Envelope) -> Result<crate::mailbox::SendEmail> {
    let parsed = mail_parser::MessageParser::default()
        .parse(raw)
        .ok_or_else(|| Error::validation("The message could not be parsed as MIME."))?;

    let header_to = all_addresses(parsed.to());
    let header_cc = all_addresses(parsed.cc());

    let to = if header_to.is_empty() {
        envelope.recipients.clone()
    } else {
        header_to.clone()
    };

    // Anything the envelope named that no visible header accounts for was blind-copied.
    let visible: Vec<String> = header_to
        .iter()
        .chain(header_cc.iter())
        .map(|address| bare_address(address).to_ascii_lowercase())
        .collect();

    let mut bcc = all_addresses(parsed.bcc());
    if !header_to.is_empty() {
        for recipient in &envelope.recipients {
            let bare = bare_address(recipient).to_ascii_lowercase();

            if !visible.contains(&bare)
                && !bcc
                    .iter()
                    .any(|existing| bare_address(existing).eq_ignore_ascii_case(&bare))
            {
                bcc.push(recipient.clone());
            }
        }
    }

    let text = parsed.text_bodies().find_map(|part| match &part.body {
        mail_parser::PartType::Text(text) => Some(text.as_ref().to_owned()),
        _ => None,
    });
    let html = parsed.html_bodies().find_map(|part| match &part.body {
        mail_parser::PartType::Html(html) => Some(html.as_ref().to_owned()),
        _ => None,
    });

    // A message with no body at all would be refused by the engine, and an empty text
    // body is a truer description of what arrived than a missing one.
    let text = match (&text, &html) {
        (None, None) => Some(String::new()),
        _ => text,
    };

    let headers: BTreeMap<String, String> = parsed
        .headers()
        .iter()
        .filter(|header| !is_private(header.name()) && !is_structural(header.name()))
        .filter_map(|header| raw_value(header).map(|value| (header.name().to_owned(), value)))
        .collect();

    let attachments = parsed
        .attachments()
        .map(|part| Attachment {
            filename: part.attachment_name().map(str::to_owned),
            content_type: part
                .content_type()
                .map(|content_type| match content_type.subtype() {
                    Some(subtype) => format!("{}/{subtype}", content_type.ctype()),
                    None => content_type.ctype().to_owned(),
                }),
            content_id: part.content_id().map(str::to_owned),
            source: AttachmentSource::Content(Bytes::from(part.contents().to_vec())),
        })
        .collect();

    Ok(crate::mailbox::SendEmail {
        from: first_address(parsed.from()).unwrap_or_else(|| envelope.sender.clone()),
        to,
        subject: parsed.subject().unwrap_or_default().to_owned(),
        html,
        text,
        cc: header_cc,
        bcc,
        reply_to: all_addresses(parsed.reply_to()),
        headers,
        attachments,
        tags: Vec::new(),
        template: None,
        scheduled_at: None,
    })
}

/// The address part of `Name <addr>`, or the whole string.
fn bare_address(value: &str) -> &str {
    let trimmed = value.trim();

    match (trimmed.rfind('<'), trimmed.rfind('>')) {
        (Some(open), Some(close)) if close > open => trimmed[open + 1..close].trim(),
        _ => trimmed,
    }
}

#[cfg(test)]
mod round_trip {
    use std::collections::BTreeMap;

    use bytes::Bytes;
    use chrono::{TimeZone, Utc};

    use super::{from_eml, to_eml};
    use crate::events::EmailEvent;
    use crate::model::{ApiKey, Attachment, AttachmentSource, EmailId, Message, Tag};

    fn message() -> Message {
        Message {
            id: EmailId::generate(),
            key: ApiKey::parse("re_mime_round_trip").expect("valid"),
            from: "Acme <onboarding@example.com>".to_owned(),
            to: vec!["delivered@example.org".to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Hello World".to_owned(),
            html: None,
            text: Some("It works".to_owned()),
            headers: BTreeMap::new(),
            attachments: Vec::new(),
            tags: Vec::new(),
            template: None,
            scheduled_at: None,
            created_at: Utc
                .with_ymd_and_hms(2026, 3, 14, 15, 9, 26)
                .single()
                .expect("a real instant"),
            last_event: EmailEvent::Sent,
            message_id: Some("<abc-123@mock-resend.invalid>".to_owned()),
        }
    }

    fn round_trip(message: &Message) -> Message {
        let eml = to_eml(message).expect("it writes");

        from_eml(&eml).expect("it reads back")
    }

    #[test]
    fn should_round_trip_a_minimal_message() {
        let original = message();

        assert_eq!(round_trip(&original), original);
    }

    #[test]
    fn should_write_something_a_mail_client_would_recognise() {
        let eml = to_eml(&message()).expect("it writes");
        let text = String::from_utf8(eml).expect("ascii headers");

        // The display name comes out quoted, which is valid RFC 5322 and what a mail
        // client expects.
        assert!(text.contains(r#"From: "Acme" <onboarding@example.com>"#));
        assert!(text.contains("To: <delivered@example.org>"));
        assert!(text.contains("Subject: Hello World"));
        assert!(text.contains("Date: "));
    }

    #[test]
    fn should_never_write_the_machines_hostname_into_a_message_id() {
        // `mail-builder` generates a missing `Message-ID` from the hostname. A captured
        // `.eml` lands on a mounted volume, so that would put the developer's computer
        // name in a file they may well hand to someone else.
        let mut original = message();
        original.message_id = None;

        let eml = to_eml(&original).expect("it writes");
        let text = String::from_utf8_lossy(&eml);

        let message_id = text
            .lines()
            .find(|line| line.starts_with("Message-ID:"))
            .expect("every message has one");

        assert!(
            message_id.contains("@mock-resend.invalid"),
            "expected a fixed fake domain, got {message_id}"
        );
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("Message-ID:"))
                .count(),
            1,
            "two Message-ID headers is not a valid message"
        );
    }

    #[test]
    fn should_round_trip_a_supplied_message_id_without_doubling_its_brackets() {
        let original = message();
        let eml = to_eml(&original).expect("it writes");
        let text = String::from_utf8_lossy(&eml);

        assert!(text.contains("Message-ID: <abc-123@mock-resend.invalid>"));
        assert_eq!(round_trip(&original).message_id, original.message_id);
    }

    #[test]
    fn should_round_trip_every_recipient_field() {
        let mut original = message();
        original.cc = vec!["cc@example.org".to_owned()];
        original.bcc = vec!["bcc@example.org".to_owned()];
        original.reply_to = vec!["reply@example.org".to_owned()];

        assert_eq!(round_trip(&original), original);
    }

    #[test]
    fn should_round_trip_several_recipients() {
        let mut original = message();
        original.to = vec![
            "one@example.org".to_owned(),
            "Two <two@example.org>".to_owned(),
            "three@example.org".to_owned(),
        ];

        assert_eq!(round_trip(&original).to, original.to);
    }

    #[test]
    fn should_round_trip_both_bodies() {
        let mut original = message();
        original.html = Some("<p>It works</p>".to_owned());

        let restored = round_trip(&original);

        assert_eq!(restored.text, original.text);
        assert_eq!(restored.html, original.html);
    }

    #[test]
    fn should_round_trip_an_html_only_message() {
        let mut original = message();
        original.text = None;
        original.html = Some("<p>It works</p>".to_owned());

        assert_eq!(round_trip(&original), original);
    }

    #[test]
    fn should_round_trip_a_non_ascii_subject() {
        // Goes out as an RFC 2047 encoded word and has to come back as itself.
        let mut original = message();
        original.subject = "Grüße aus München — 配達完了".to_owned();

        assert_eq!(round_trip(&original).subject, original.subject);
    }

    #[test]
    fn should_round_trip_a_non_ascii_body() {
        let mut original = message();
        original.text = Some("Grüße aus München — 配達完了".to_owned());

        assert_eq!(round_trip(&original).text, original.text);
    }

    #[test]
    fn should_round_trip_a_long_subject_that_has_to_be_folded() {
        let mut original = message();
        original.subject = "word ".repeat(40).trim_end().to_owned();

        assert_eq!(round_trip(&original).subject, original.subject);
    }

    #[test]
    fn should_round_trip_caller_supplied_headers() {
        let mut original = message();
        let _inserted = original
            .headers
            .insert("X-Entity-Ref-ID".to_owned(), "abc-123".to_owned());
        let _also = original
            .headers
            .insert("X-Campaign".to_owned(), "spring".to_owned());

        assert_eq!(round_trip(&original).headers, original.headers);
    }

    #[test]
    fn should_round_trip_tags() {
        let mut original = message();
        original.tags = vec![
            Tag {
                name: "category".to_owned(),
                value: "confirm_email".to_owned(),
            },
            Tag {
                name: "campaign".to_owned(),
                value: "spring-2026".to_owned(),
            },
        ];

        assert_eq!(round_trip(&original).tags, original.tags);
    }

    #[test]
    fn should_round_trip_a_scheduled_send() {
        let mut original = message();
        original.last_event = EmailEvent::Scheduled;
        original.scheduled_at = Some(
            Utc.with_ymd_and_hms(2026, 8, 5, 11, 52, 1)
                .single()
                .expect("a real instant"),
        );

        let restored = round_trip(&original);

        assert_eq!(restored.last_event, EmailEvent::Scheduled);
        assert_eq!(restored.scheduled_at, original.scheduled_at);
    }

    #[test]
    fn should_round_trip_a_template_id() {
        let mut original = message();
        original.text = None;
        original.html = None;
        original.template = Some("welcome".to_owned());

        assert_eq!(round_trip(&original), original);
    }

    #[test]
    fn should_round_trip_every_delivery_event() {
        for event in crate::events::ALL_EVENTS {
            let mut original = message();
            original.last_event = event;

            assert_eq!(
                round_trip(&original).last_event,
                event,
                "{event} should survive the round trip"
            );
        }
    }

    #[test]
    fn should_round_trip_an_attachment_with_content() {
        let mut original = message();
        original.attachments = vec![Attachment {
            filename: Some("invoice.pdf".to_owned()),
            content_type: Some("application/pdf".to_owned()),
            content_id: None,
            source: AttachmentSource::Content(Bytes::from_static(b"%PDF-1.4 not really")),
        }];

        let restored = round_trip(&original);

        assert_eq!(restored.attachments.len(), 1);
        assert_eq!(
            restored.attachments[0].filename.as_deref(),
            Some("invoice.pdf")
        );
        assert_eq!(
            restored.attachments[0].source,
            original.attachments[0].source
        );
    }

    #[test]
    fn should_round_trip_binary_attachment_bytes_exactly() {
        let bytes: Vec<u8> = (0..=255_u8).collect();

        let mut original = message();
        original.attachments = vec![Attachment {
            filename: Some("bytes.bin".to_owned()),
            content_type: None,
            content_id: None,
            source: AttachmentSource::Content(Bytes::from(bytes.clone())),
        }];

        let restored = round_trip(&original);

        assert_eq!(
            restored.attachments[0].source,
            AttachmentSource::Content(Bytes::from(bytes)),
            "base64 has to survive every byte value, not just printable ones"
        );
    }

    #[test]
    fn should_record_a_path_attachment_without_inventing_bytes_for_it() {
        let mut original = message();
        original.attachments = vec![Attachment {
            filename: Some("invoice.pdf".to_owned()),
            content_type: None,
            content_id: None,
            source: AttachmentSource::Path("https://example.com/invoice.pdf".to_owned()),
        }];

        let restored = round_trip(&original);

        assert_eq!(
            restored.attachments[0].source,
            AttachmentSource::Path("https://example.com/invoice.pdf".to_owned()),
            "an empty part would turn `never fetched` into `this file is empty`"
        );
    }

    #[test]
    fn should_not_let_a_caller_smuggle_engine_state_in_a_header() {
        let mut original = message();
        let _smuggled = original.headers.insert(
            "X-Mock-Resend-Api-Key".to_owned(),
            "re_somebody_elses_key".to_owned(),
        );

        let restored = round_trip(&original);

        assert_eq!(
            restored.key, original.key,
            "a caller-supplied private header must not change who owns the message"
        );
        assert!(
            !restored.headers.contains_key("X-Mock-Resend-Api-Key"),
            "the private namespace is dropped on the way out, not echoed back"
        );
    }

    #[test]
    fn should_round_trip_a_from_address_with_no_display_name() {
        let mut original = message();
        original.from = "onboarding@example.com".to_owned();

        assert_eq!(round_trip(&original).from, original.from);
    }

    #[test]
    fn should_refuse_an_eml_that_was_not_written_here() {
        // A plain message with none of the private headers is not a captured message,
        // and guessing values for them would invent an owner for someone's mail.
        let plain = b"From: a@example.com\r\nTo: b@example.org\r\nSubject: Hi\r\n\r\nBody\r\n";

        assert!(from_eml(plain).is_err());
    }

    #[test]
    fn should_refuse_bytes_that_are_not_a_message_at_all() {
        assert!(from_eml(b"").is_err());
    }
}

#[cfg(test)]
mod is_private {
    use super::is_private;

    #[test]
    fn should_recognise_the_private_namespace() {
        assert!(is_private("X-Mock-Resend-Api-Key"));
        assert!(is_private("x-mock-resend-tag"));
        assert!(is_private("X-MOCK-RESEND-ANYTHING"));
    }

    #[test]
    fn should_not_claim_an_ordinary_header() {
        for name in ["X-Entity-Ref-ID", "Subject", "X-Mock", "X-Mock-Resend"] {
            assert!(!is_private(name), "{name} is not in the private namespace");
        }
    }
}

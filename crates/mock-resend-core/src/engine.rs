//! The engine: the one handle the HTTP and SMTP layers hold.
//!
//! It owns the clock and the store, and every behaviour is a method on it. A handler
//! parses a request, calls one of these, and maps the error; it never reaches past this
//! into the store or appends an event itself.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::events::EmailEvent;
use crate::mailbox::SendEmail;
use crate::model::{ApiKey, EmailId, Message};
use crate::schedule::{Clock, SystemClock};
use crate::store::{Store, memory::MemoryStore};

/// The mock-resend engine.
///
/// Cheap to clone: both the store and the clock sit behind an [`Arc`], so every
/// listener and every request handler shares one mailbox.
#[derive(Clone)]
pub struct Engine {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
}

impl Engine {
    /// An engine with the in-memory store and the wall clock.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::new(Arc::new(MemoryStore::new()), Arc::new(SystemClock))
    }

    /// An engine over a given store and clock.
    #[must_use]
    pub fn new(store: Arc<dyn Store>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    /// The engine's current time.
    ///
    /// Comes from the clock, which a test can move. Nothing reads the wall clock
    /// directly.
    #[must_use]
    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.clock.now()
    }

    /// Validates and captures a message, returning the id `POST /emails` reports.
    ///
    /// Nothing is delivered. The message is stored, reported and forgotten.
    ///
    /// # Errors
    ///
    /// Whatever [`SendEmail::validate`] rejects, or [`Error::Store`] if the backend
    /// fails.
    pub async fn send_email(&self, key: &ApiKey, request: SendEmail) -> Result<EmailId> {
        request.validate()?;

        let now = self.clock.now();
        let id = EmailId::generate();

        // A send due in the future sits at `scheduled`; everything else is handed off at
        // once. Resend omits `message_id` until that hand-off, which is why a scheduled
        // message has none.
        let scheduled = request
            .scheduled_at
            .is_some_and(|scheduled_at| scheduled_at > now);

        let last_event = if scheduled {
            EmailEvent::Scheduled
        } else {
            EmailEvent::Sent
        };

        let message = Message {
            id,
            key: key.clone(),
            from: request.from,
            to: request.to,
            cc: request.cc,
            bcc: request.bcc,
            reply_to: request.reply_to,
            subject: request.subject,
            html: request.html,
            text: request.text,
            headers: request.headers,
            attachments: request.attachments,
            tags: request.tags,
            template: request.template,
            scheduled_at: request.scheduled_at,
            created_at: now,
            last_event,
            message_id: if scheduled {
                None
            } else {
                Some(message_id_for(id))
            },
        };

        self.store.put(message).await?;

        Ok(id)
    }

    /// Returns a captured message.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when no message with that id exists **under this key**. An id
    /// belonging to another key is not found, not forbidden: that is the isolation
    /// guarantee, and reporting the difference would leak one test's state into another.
    pub async fn get_email(&self, key: &ApiKey, id: &EmailId) -> Result<Message> {
        self.store.get(key, id).await?.ok_or(Error::NotFound)
    }

    /// Returns every message captured under `key`, oldest first.
    ///
    /// Not a Resend endpoint. The admin API and the testkit use it.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the backend fails.
    pub async fn list_emails(&self, key: &ApiKey) -> Result<Vec<Message>> {
        self.store.list(key).await
    }

    /// Discards everything under `key`, or the whole store when `key` is `None`.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the backend fails.
    pub async fn reset(&self, key: Option<&ApiKey>) -> Result<()> {
        self.store.reset(key).await
    }
}

impl std::fmt::Debug for Engine {
    /// Names the type without reaching into the store.
    ///
    /// A derived `Debug` would need `Store: Debug`, and a store that can print itself is
    /// a store that can print a captured body into a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine").finish_non_exhaustive()
    }
}

/// Builds the RFC 5322 `Message-ID` reported for a handed-off message.
///
/// The domain is a literal: there is no sending domain here, and inventing one that
/// looks real would put a plausible address into a header a test might assert on.
fn message_id_for(id: EmailId) -> String {
    format!("<{id}@mock-resend.invalid>")
}

#[cfg(test)]
mod engine {
    use std::sync::Arc;

    use chrono::Duration;

    use super::Engine;
    use crate::error::Error;
    use crate::events::EmailEvent;
    use crate::mailbox::SendEmail;
    use crate::model::{ApiKey, EmailId};
    use crate::schedule::{Clock, TestClock};
    use crate::store::memory::MemoryStore;

    fn key(name: &str) -> ApiKey {
        ApiKey::parse(name).expect("the test key is re_-prefixed")
    }

    /// An engine on a clock the test owns, so timestamps are deterministic.
    fn engine() -> (Engine, Arc<TestClock>) {
        let clock = Arc::new(TestClock::new());
        let shared: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
        let engine = Engine::new(Arc::new(MemoryStore::new()), shared);

        (engine, clock)
    }

    fn request() -> SendEmail {
        let mut request = SendEmail::new(
            "Acme <onboarding@example.com>",
            vec!["delivered@example.org".to_owned()],
            "Hello World",
        );
        request.text = Some("It works".to_owned());
        request
    }

    #[tokio::test]
    async fn should_return_the_captured_message_for_the_id_it_handed_out() {
        let (engine, _clock) = engine();
        let key = key("re_should_return_the_captured_message");

        let id = engine
            .send_email(&key, request())
            .await
            .expect("a valid request is captured");

        let message = engine.get_email(&key, &id).await.expect("it is there");

        assert_eq!(message.id, id);
        assert_eq!(message.subject, "Hello World");
        assert_eq!(message.to, ["delivered@example.org"]);
    }

    #[tokio::test]
    async fn should_stamp_created_at_from_the_engine_clock() {
        let (engine, clock) = engine();
        let key = key("re_should_stamp_created_at_from_the_clock");

        clock.advance_secs(3_600);
        let expected = clock.now();

        let id = engine.send_email(&key, request()).await.expect("captured");
        let message = engine.get_email(&key, &id).await.expect("it is there");

        assert_eq!(message.created_at, expected);
    }

    #[tokio::test]
    async fn should_report_a_fresh_send_as_sent() {
        let (engine, _clock) = engine();
        let key = key("re_should_report_a_fresh_send_as_sent");

        let id = engine.send_email(&key, request()).await.expect("captured");
        let message = engine.get_email(&key, &id).await.expect("it is there");

        assert_eq!(message.last_event, EmailEvent::Sent);
        assert!(message.message_id.is_some());
    }

    #[tokio::test]
    async fn should_report_a_future_send_as_scheduled() {
        let (engine, clock) = engine();
        let key = key("re_should_report_a_future_send_as_scheduled");

        let mut request = request();
        request.scheduled_at = Some(clock.now() + Duration::hours(1));

        let id = engine.send_email(&key, request).await.expect("captured");
        let message = engine.get_email(&key, &id).await.expect("it is there");

        assert_eq!(message.last_event, EmailEvent::Scheduled);
        assert!(message.scheduled_at.is_some());
    }

    #[tokio::test]
    async fn should_omit_the_message_id_while_a_send_is_scheduled() {
        let (engine, clock) = engine();
        let key = key("re_should_omit_message_id_while_scheduled");

        let mut request = request();
        request.scheduled_at = Some(clock.now() + Duration::hours(1));

        let id = engine.send_email(&key, request).await.expect("captured");
        let message = engine.get_email(&key, &id).await.expect("it is there");

        assert!(
            message.message_id.is_none(),
            "Resend has no Message-ID to report until the message is handed off"
        );
    }

    #[tokio::test]
    async fn should_treat_a_past_scheduled_at_as_an_immediate_send() {
        let (engine, clock) = engine();
        let key = key("re_should_treat_a_past_schedule_as_immediate");

        let mut request = request();
        request.scheduled_at = Some(clock.now() - Duration::hours(1));

        let id = engine.send_email(&key, request).await.expect("captured");
        let message = engine.get_email(&key, &id).await.expect("it is there");

        assert_eq!(message.last_event, EmailEvent::Sent);
    }

    #[tokio::test]
    async fn should_not_capture_an_invalid_request() {
        let (engine, _clock) = engine();
        let key = key("re_should_not_capture_an_invalid_request");

        let error = engine
            .send_email(&key, SendEmail::default())
            .await
            .expect_err("an empty request is rejected");

        assert!(matches!(error, Error::MissingRequiredField { .. }));
        assert!(
            engine.list_emails(&key).await.expect("list").is_empty(),
            "a rejected send must leave nothing behind"
        );
    }

    #[tokio::test]
    async fn should_not_find_an_email_that_was_never_sent() {
        let (engine, _clock) = engine();
        let key = key("re_should_not_find_an_email_never_sent");

        let error = engine
            .get_email(&key, &EmailId::generate())
            .await
            .expect_err("nothing was captured");

        assert!(matches!(error, Error::NotFound));
    }

    #[tokio::test]
    async fn should_not_find_an_email_belonging_to_another_key() {
        let (engine, _clock) = engine();
        let mine = key("re_should_not_find_another_keys_email_mine");
        let theirs = key("re_should_not_find_another_keys_email_theirs");

        let id = engine.send_email(&mine, request()).await.expect("captured");

        let error = engine
            .get_email(&theirs, &id)
            .await
            .expect_err("it belongs to another key");

        assert!(
            matches!(error, Error::NotFound),
            "another key's message is not found, not forbidden"
        );
    }

    #[tokio::test]
    async fn should_keep_two_keys_mailboxes_independent() {
        let (engine, _clock) = engine();
        let first = key("re_should_keep_two_mailboxes_independent_one");
        let second = key("re_should_keep_two_mailboxes_independent_two");

        let _first = engine
            .send_email(&first, request())
            .await
            .expect("captured");
        let _second = engine
            .send_email(&second, request())
            .await
            .expect("captured");
        let _also = engine
            .send_email(&second, request())
            .await
            .expect("captured");

        assert_eq!(engine.list_emails(&first).await.expect("list").len(), 1);
        assert_eq!(engine.list_emails(&second).await.expect("list").len(), 2);
    }

    #[tokio::test]
    async fn should_give_every_send_a_distinct_id() {
        let (engine, _clock) = engine();
        let key = key("re_should_give_every_send_a_distinct_id");

        let first = engine.send_email(&key, request()).await.expect("captured");
        let second = engine.send_email(&key, request()).await.expect("captured");

        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn should_empty_only_the_key_it_was_asked_to_reset() {
        let (engine, _clock) = engine();
        let reset = key("re_should_empty_only_the_named_key_reset");
        let kept = key("re_should_empty_only_the_named_key_kept");

        let _reset = engine
            .send_email(&reset, request())
            .await
            .expect("captured");
        let _kept = engine.send_email(&kept, request()).await.expect("captured");

        engine.reset(Some(&reset)).await.expect("reset succeeds");

        assert!(engine.list_emails(&reset).await.expect("list").is_empty());
        assert_eq!(engine.list_emails(&kept).await.expect("list").len(), 1);
    }
}

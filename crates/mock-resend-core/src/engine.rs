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
        // once.
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
            message_id: Some(message_id_for(id)),
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

    /// Captures up to [`MAX_BATCH`] messages in one call.
    ///
    /// `validation` decides what happens to an invalid entry. In
    /// [`BatchValidation::Strict`], Resend's default, the whole batch is rejected and
    /// nothing is captured; in [`BatchValidation::Permissive`] the valid ones are
    /// captured and the rest are reported by index.
    ///
    /// <https://resend.com/docs/api-reference/emails/send-batch-emails>
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for an empty batch or one over the limit, the first
    /// entry's own error in strict mode, or [`Error::Store`] if the backend fails.
    pub async fn send_batch(
        &self,
        key: &ApiKey,
        requests: Vec<SendEmail>,
        validation: BatchValidation,
    ) -> Result<BatchOutcome> {
        if requests.is_empty() {
            return Err(Error::validation(
                "A batch must contain at least one email.",
            ));
        }

        if requests.len() > MAX_BATCH {
            return Err(Error::validation(format!(
                "A batch can contain at most {MAX_BATCH} emails, and this request has {}.",
                requests.len()
            )));
        }

        // Validate everything before capturing anything, so that strict mode is atomic
        // rather than "atomic until the first failure".
        let mut failures = Vec::new();
        for (index, request) in requests.iter().enumerate() {
            if let Err(error) = request.validate() {
                if matches!(validation, BatchValidation::Strict) {
                    return Err(error);
                }

                failures.push(BatchFailure {
                    index,
                    message: error.to_string(),
                });
            }
        }

        let failed: Vec<usize> = failures.iter().map(|failure| failure.index).collect();

        let mut ids = Vec::new();
        for (index, request) in requests.into_iter().enumerate() {
            if failed.contains(&index) {
                continue;
            }

            ids.push(self.send_email(key, request).await?);
        }

        Ok(BatchOutcome { ids, failures })
    }

    /// Moves a scheduled send to a new time.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when the id is unknown under this key, and
    /// [`Error::Validation`] when the message is no longer scheduled.
    pub async fn reschedule_email(
        &self,
        key: &ApiKey,
        id: &EmailId,
        scheduled_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<EmailId> {
        let mut message = self.get_email(key, id).await?;

        if !message.last_event.is_scheduled() {
            return Err(Error::validation(format!(
                "Only a scheduled email can be rescheduled, and this one is `{}`.",
                message.last_event
            )));
        }

        message.scheduled_at = Some(scheduled_at);

        // A reschedule into the past is due immediately, so the next tick sends it
        // rather than leaving it scheduled forever.
        self.store.put(message).await?;

        Ok(*id)
    }

    /// Cancels a scheduled send.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when the id is unknown under this key, and
    /// [`Error::Validation`] when the message is no longer scheduled.
    pub async fn cancel_email(&self, key: &ApiKey, id: &EmailId) -> Result<EmailId> {
        let mut message = self.get_email(key, id).await?;

        if !message.last_event.can_transition_to(EmailEvent::Canceled) {
            return Err(Error::validation(format!(
                "Only a scheduled email can be cancelled, and this one is `{}`.",
                message.last_event
            )));
        }

        message.last_event = EmailEvent::Canceled;
        self.store.put(message).await?;

        Ok(*id)
    }

    /// Sends every scheduled message whose time has come.
    ///
    /// Called by the server on an interval and by the admin API after the clock moves,
    /// so a test advances the clock and ticks rather than sleeping. Returns the
    /// transitions that happened, which is what the webhook sender needs.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the backend fails.
    pub async fn tick(&self) -> Result<Vec<Transition>> {
        let now = self.clock.now();
        let due = self.store.due_scheduled(now).await?;

        let mut transitions = Vec::new();
        for mut message in due {
            if !message.last_event.can_transition_to(EmailEvent::Sent) {
                continue;
            }

            let from = message.last_event;
            message.last_event = EmailEvent::Sent;

            let transition = Transition {
                key: message.key.clone(),
                id: message.id,
                from,
                to: EmailEvent::Sent,
                at: now,
            };

            self.store.put(message).await?;
            transitions.push(transition);
        }

        Ok(transitions)
    }

    /// Moves a captured message to `next`, if the lifecycle allows it.
    ///
    /// This is how the admin API drives a bounce or an open on demand. The engine owns
    /// the decision; a handler never appends an event itself.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when the id is unknown under this key, and
    /// [`Error::Validation`] when the lifecycle forbids the transition.
    pub async fn advance_event(
        &self,
        key: &ApiKey,
        id: &EmailId,
        next: EmailEvent,
    ) -> Result<Transition> {
        let mut message = self.get_email(key, id).await?;
        let from = message.last_event;

        if !from.can_transition_to(next) {
            return Err(Error::validation(format!(
                "An email at `{from}` cannot move to `{next}`."
            )));
        }

        message.last_event = next;
        let at = self.clock.now();

        self.store.put(message).await?;

        Ok(Transition {
            key: key.clone(),
            id: *id,
            from,
            to: next,
            at,
        })
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

/// Resend's ceiling for one batch call.
///
/// <https://resend.com/docs/api-reference/emails/send-batch-emails>
pub const MAX_BATCH: usize = 100;

/// How a batch treats an invalid entry.
///
/// Sent as the `x-batch-validation` header, and strict is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BatchValidation {
    /// Reject the whole batch if any entry is invalid, capturing nothing.
    #[default]
    Strict,
    /// Capture the valid entries and report the rest by index.
    Permissive,
}

impl BatchValidation {
    /// Parses the `x-batch-validation` header value, defaulting to strict.
    ///
    /// An unrecognised value is strict rather than an error: strict is the safe reading,
    /// because it captures nothing a caller did not clearly ask for.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some(value) if value.eq_ignore_ascii_case("permissive") => Self::Permissive,
            _ => Self::Strict,
        }
    }
}

/// What a batch call captured, and what it refused.
#[derive(Debug, Clone, Default)]
pub struct BatchOutcome {
    /// The ids of the captured messages, in request order.
    pub ids: Vec<EmailId>,
    /// The entries that failed validation, in request order. Always empty in strict
    /// mode, which fails the whole call instead.
    pub failures: Vec<BatchFailure>,
}

/// One rejected batch entry.
#[derive(Debug, Clone)]
pub struct BatchFailure {
    /// The entry's position in the request.
    pub index: usize,
    /// Why it was rejected.
    pub message: String,
}

/// A delivery event that just happened.
///
/// Carries what the webhook sender needs to build a payload, and nothing more.
#[derive(Debug, Clone)]
pub struct Transition {
    /// The key whose namespace the message lives in.
    pub key: ApiKey,
    /// The message that moved.
    pub id: EmailId,
    /// The event it was at.
    pub from: EmailEvent,
    /// The event it is at now.
    pub to: EmailEvent,
    /// When it moved, by the engine's clock.
    pub at: chrono::DateTime<chrono::Utc>,
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

/// Builds the RFC 5322 `Message-ID` reported for a captured message.
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
    async fn should_report_a_message_id_for_a_scheduled_send_too() {
        let (engine, clock) = engine();
        let key = key("re_should_report_a_message_id_when_scheduled");

        let mut request = request();
        request.scheduled_at = Some(clock.now() + Duration::hours(1));

        let id = engine.send_email(&key, request).await.expect("captured");
        let message = engine.get_email(&key, &id).await.expect("it is there");

        // Resend's own documented `email.scheduled` webhook payload carries a
        // `message_id`, so one is minted at capture rather than at hand-off.
        assert!(message.message_id.is_some());
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

#[cfg(test)]
mod send_batch {
    use std::sync::Arc;

    use super::{BatchValidation, Engine, MAX_BATCH};
    use crate::error::Error;
    use crate::mailbox::SendEmail;
    use crate::model::ApiKey;
    use crate::schedule::{Clock, TestClock};
    use crate::store::memory::MemoryStore;

    fn engine() -> Engine {
        let clock: Arc<dyn Clock> = Arc::new(TestClock::new());

        Engine::new(Arc::new(MemoryStore::new()), clock)
    }

    fn key(name: &str) -> ApiKey {
        ApiKey::parse(name).expect("the test key is re_-prefixed")
    }

    fn valid(subject: &str) -> SendEmail {
        let mut request = SendEmail::new(
            "Acme <onboarding@example.com>",
            vec!["delivered@example.org".to_owned()],
            subject,
        );
        request.text = Some("It works".to_owned());
        request
    }

    fn invalid() -> SendEmail {
        let mut request = valid("broken");
        request.to = vec!["not-an-address".to_owned()];
        request
    }

    #[tokio::test]
    async fn should_capture_every_message_in_a_valid_batch() {
        let engine = engine();
        let key = key("re_should_capture_every_message_in_a_batch");

        let outcome = engine
            .send_batch(
                &key,
                vec![valid("one"), valid("two"), valid("three")],
                BatchValidation::Strict,
            )
            .await
            .expect("a valid batch is captured");

        assert_eq!(outcome.ids.len(), 3);
        assert!(outcome.failures.is_empty());
        assert_eq!(engine.list_emails(&key).await.expect("list").len(), 3);
    }

    #[tokio::test]
    async fn should_report_batch_ids_in_request_order() {
        let engine = engine();
        let key = key("re_should_report_batch_ids_in_order");

        let outcome = engine
            .send_batch(
                &key,
                vec![valid("first"), valid("second")],
                BatchValidation::Strict,
            )
            .await
            .expect("captured");

        let captured = engine.list_emails(&key).await.expect("list");

        assert_eq!(outcome.ids, [captured[0].id, captured[1].id]);
        assert_eq!(captured[0].subject, "first");
    }

    #[tokio::test]
    async fn should_reject_an_empty_batch() {
        let engine = engine();
        let key = key("re_should_reject_an_empty_batch");

        assert!(matches!(
            engine
                .send_batch(&key, Vec::new(), BatchValidation::Strict)
                .await,
            Err(Error::Validation { .. })
        ));
    }

    #[tokio::test]
    async fn should_accept_exactly_one_hundred_messages() {
        let engine = engine();
        let key = key("re_should_accept_exactly_one_hundred");

        let batch: Vec<SendEmail> = (0..MAX_BATCH)
            .map(|index| valid(&index.to_string()))
            .collect();

        let outcome = engine
            .send_batch(&key, batch, BatchValidation::Strict)
            .await
            .expect("the limit itself is allowed");

        assert_eq!(outcome.ids.len(), MAX_BATCH);
    }

    #[tokio::test]
    async fn should_reject_more_than_one_hundred_messages() {
        let engine = engine();
        let key = key("re_should_reject_more_than_one_hundred");

        let batch: Vec<SendEmail> = (0..=MAX_BATCH)
            .map(|index| valid(&index.to_string()))
            .collect();

        assert!(matches!(
            engine
                .send_batch(&key, batch, BatchValidation::Strict)
                .await,
            Err(Error::Validation { .. })
        ));
    }

    #[tokio::test]
    async fn should_capture_nothing_when_a_strict_batch_has_an_invalid_entry() {
        let engine = engine();
        let key = key("re_should_capture_nothing_in_a_failed_strict_batch");

        let error = engine
            .send_batch(
                &key,
                vec![valid("fine"), invalid(), valid("also fine")],
                BatchValidation::Strict,
            )
            .await
            .expect_err("strict mode rejects the whole batch");

        assert!(matches!(error, Error::Validation { .. }));
        assert!(
            engine.list_emails(&key).await.expect("list").is_empty(),
            "strict mode is atomic, not atomic-until-the-first-failure"
        );
    }

    #[tokio::test]
    async fn should_capture_the_valid_entries_of_a_permissive_batch() {
        let engine = engine();
        let key = key("re_should_capture_valid_entries_permissively");

        let outcome = engine
            .send_batch(
                &key,
                vec![valid("fine"), invalid(), valid("also fine")],
                BatchValidation::Permissive,
            )
            .await
            .expect("permissive mode allows partial success");

        assert_eq!(outcome.ids.len(), 2);
        assert_eq!(engine.list_emails(&key).await.expect("list").len(), 2);
    }

    #[tokio::test]
    async fn should_report_a_permissive_failure_by_its_index_in_the_request() {
        let engine = engine();
        let key = key("re_should_report_a_permissive_failure_by_index");

        let outcome = engine
            .send_batch(
                &key,
                vec![valid("fine"), invalid(), valid("also fine")],
                BatchValidation::Permissive,
            )
            .await
            .expect("partial success");

        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].index, 1);
        assert!(!outcome.failures[0].message.is_empty());
    }

    #[tokio::test]
    async fn should_default_the_batch_validation_header_to_strict() {
        assert_eq!(BatchValidation::parse(None), BatchValidation::Strict);
        assert_eq!(
            BatchValidation::parse(Some("permissive")),
            BatchValidation::Permissive
        );
        assert_eq!(
            BatchValidation::parse(Some("PERMISSIVE")),
            BatchValidation::Permissive
        );
        assert_eq!(
            BatchValidation::parse(Some("nonsense")),
            BatchValidation::Strict,
            "an unrecognised mode captures nothing the caller did not ask for"
        );
    }
}

#[cfg(test)]
mod scheduling {
    use std::sync::Arc;

    use chrono::Duration;

    use super::Engine;
    use crate::error::Error;
    use crate::events::EmailEvent;
    use crate::mailbox::SendEmail;
    use crate::model::{ApiKey, EmailId};
    use crate::schedule::{Clock, TestClock};
    use crate::store::memory::MemoryStore;

    fn engine() -> (Engine, Arc<TestClock>) {
        let clock = Arc::new(TestClock::new());
        let shared: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;

        (Engine::new(Arc::new(MemoryStore::new()), shared), clock)
    }

    fn key(name: &str) -> ApiKey {
        ApiKey::parse(name).expect("the test key is re_-prefixed")
    }

    fn scheduled_in(hours: i64, clock: &TestClock) -> SendEmail {
        let mut request = SendEmail::new(
            "Acme <onboarding@example.com>",
            vec!["delivered@example.org".to_owned()],
            "Later",
        );
        request.text = Some("It works".to_owned());
        request.scheduled_at = Some(clock.now() + Duration::hours(hours));
        request
    }

    #[tokio::test]
    async fn should_send_a_scheduled_message_once_its_time_arrives() {
        let (engine, clock) = engine();
        let key = key("re_should_send_a_scheduled_message_when_due");

        let id = engine
            .send_email(&key, scheduled_in(1, &clock))
            .await
            .expect("captured");

        clock.advance_secs(3_600);
        let transitions = engine.tick().await.expect("tick succeeds");

        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].from, EmailEvent::Scheduled);
        assert_eq!(transitions[0].to, EmailEvent::Sent);
        assert_eq!(
            engine
                .get_email(&key, &id)
                .await
                .expect("it is there")
                .last_event,
            EmailEvent::Sent
        );
    }

    #[tokio::test]
    async fn should_leave_a_scheduled_message_alone_before_its_time() {
        let (engine, clock) = engine();
        let key = key("re_should_leave_a_future_message_alone");

        let id = engine
            .send_email(&key, scheduled_in(2, &clock))
            .await
            .expect("captured");

        clock.advance_secs(3_600);

        assert!(engine.tick().await.expect("tick succeeds").is_empty());
        assert_eq!(
            engine
                .get_email(&key, &id)
                .await
                .expect("it is there")
                .last_event,
            EmailEvent::Scheduled
        );
    }

    #[tokio::test]
    async fn should_send_a_scheduled_message_only_once() {
        let (engine, clock) = engine();
        let key = key("re_should_send_a_scheduled_message_once");

        let _id = engine
            .send_email(&key, scheduled_in(1, &clock))
            .await
            .expect("captured");

        clock.advance_secs(7_200);

        assert_eq!(engine.tick().await.expect("tick").len(), 1);
        assert!(
            engine.tick().await.expect("tick").is_empty(),
            "a second tick must not re-send what the first one sent"
        );
    }

    #[tokio::test]
    async fn should_not_tick_a_message_that_was_cancelled() {
        let (engine, clock) = engine();
        let key = key("re_should_not_tick_a_cancelled_message");

        let id = engine
            .send_email(&key, scheduled_in(1, &clock))
            .await
            .expect("captured");

        engine.cancel_email(&key, &id).await.expect("cancelled");
        clock.advance_secs(7_200);

        assert!(engine.tick().await.expect("tick").is_empty());
        assert_eq!(
            engine
                .get_email(&key, &id)
                .await
                .expect("it is there")
                .last_event,
            EmailEvent::Canceled
        );
    }

    #[tokio::test]
    async fn should_move_a_scheduled_send_to_a_new_time() {
        let (engine, clock) = engine();
        let key = key("re_should_move_a_scheduled_send");

        let id = engine
            .send_email(&key, scheduled_in(1, &clock))
            .await
            .expect("captured");

        let later = clock.now() + Duration::hours(5);
        let rescheduled = engine
            .reschedule_email(&key, &id, later)
            .await
            .expect("rescheduled");

        assert_eq!(rescheduled, id);

        // Past the original time, before the new one.
        clock.advance_secs(7_200);

        assert!(engine.tick().await.expect("tick").is_empty());
        assert_eq!(
            engine
                .get_email(&key, &id)
                .await
                .expect("it is there")
                .scheduled_at,
            Some(later)
        );
    }

    #[tokio::test]
    async fn should_send_a_message_rescheduled_into_the_past_on_the_next_tick() {
        let (engine, clock) = engine();
        let key = key("re_should_send_a_message_rescheduled_backwards");

        let id = engine
            .send_email(&key, scheduled_in(5, &clock))
            .await
            .expect("captured");

        let earlier = clock.now() - Duration::hours(1);
        engine
            .reschedule_email(&key, &id, earlier)
            .await
            .expect("rescheduled");

        assert_eq!(engine.tick().await.expect("tick").len(), 1);
    }

    #[tokio::test]
    async fn should_not_reschedule_a_message_that_already_went_out() {
        let (engine, clock) = engine();
        let key = key("re_should_not_reschedule_a_sent_message");

        let mut request = scheduled_in(1, &clock);
        request.scheduled_at = None;

        let id = engine.send_email(&key, request).await.expect("captured");

        let error = engine
            .reschedule_email(&key, &id, clock.now() + Duration::hours(1))
            .await
            .expect_err("a sent message cannot be rescheduled");

        assert!(matches!(error, Error::Validation { .. }));
    }

    #[tokio::test]
    async fn should_not_reschedule_an_unknown_id() {
        let (engine, clock) = engine();
        let key = key("re_should_not_reschedule_an_unknown_id");

        let error = engine
            .reschedule_email(&key, &EmailId::generate(), clock.now())
            .await
            .expect_err("nothing was captured");

        assert!(matches!(error, Error::NotFound));
    }

    #[tokio::test]
    async fn should_cancel_a_scheduled_send() {
        let (engine, clock) = engine();
        let key = key("re_should_cancel_a_scheduled_send");

        let id = engine
            .send_email(&key, scheduled_in(1, &clock))
            .await
            .expect("captured");

        let cancelled = engine.cancel_email(&key, &id).await.expect("cancelled");

        assert_eq!(cancelled, id);
        assert_eq!(
            engine
                .get_email(&key, &id)
                .await
                .expect("it is there")
                .last_event,
            EmailEvent::Canceled
        );
    }

    #[tokio::test]
    async fn should_not_cancel_a_message_that_already_went_out() {
        let (engine, _clock) = engine();
        let key = key("re_should_not_cancel_a_sent_message");

        let mut request = SendEmail::new(
            "Acme <onboarding@example.com>",
            vec!["delivered@example.org".to_owned()],
            "Gone",
        );
        request.text = Some("It works".to_owned());

        let id = engine.send_email(&key, request).await.expect("captured");

        assert!(matches!(
            engine.cancel_email(&key, &id).await,
            Err(Error::Validation { .. })
        ));
    }

    #[tokio::test]
    async fn should_not_cancel_the_same_message_twice() {
        let (engine, clock) = engine();
        let key = key("re_should_not_cancel_twice");

        let id = engine
            .send_email(&key, scheduled_in(1, &clock))
            .await
            .expect("captured");

        engine.cancel_email(&key, &id).await.expect("cancelled");

        assert!(matches!(
            engine.cancel_email(&key, &id).await,
            Err(Error::Validation { .. })
        ));
    }

    #[tokio::test]
    async fn should_not_cancel_another_keys_scheduled_send() {
        let (engine, clock) = engine();
        let mine = key("re_should_not_cancel_another_keys_send_mine");
        let theirs = key("re_should_not_cancel_another_keys_send_theirs");

        let id = engine
            .send_email(&mine, scheduled_in(1, &clock))
            .await
            .expect("captured");

        assert!(matches!(
            engine.cancel_email(&theirs, &id).await,
            Err(Error::NotFound)
        ));
    }

    #[tokio::test]
    async fn should_tick_messages_under_every_key() {
        let (engine, clock) = engine();
        let first = key("re_should_tick_every_key_first");
        let second = key("re_should_tick_every_key_second");

        let _first = engine
            .send_email(&first, scheduled_in(1, &clock))
            .await
            .expect("captured");
        let _second = engine
            .send_email(&second, scheduled_in(1, &clock))
            .await
            .expect("captured");

        clock.advance_secs(7_200);
        let transitions = engine.tick().await.expect("tick");

        assert_eq!(transitions.len(), 2);
        assert_eq!(
            transitions
                .iter()
                .filter(|transition| transition.key == first)
                .count(),
            1
        );
    }
}

#[cfg(test)]
mod advance_event {
    use std::sync::Arc;

    use super::Engine;
    use crate::error::Error;
    use crate::events::EmailEvent;
    use crate::mailbox::SendEmail;
    use crate::model::ApiKey;
    use crate::schedule::{Clock, TestClock};
    use crate::store::memory::MemoryStore;

    fn engine() -> Engine {
        let clock: Arc<dyn Clock> = Arc::new(TestClock::new());

        Engine::new(Arc::new(MemoryStore::new()), clock)
    }

    fn key(name: &str) -> ApiKey {
        ApiKey::parse(name).expect("the test key is re_-prefixed")
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
    async fn should_move_a_sent_message_to_delivered() {
        let engine = engine();
        let key = key("re_should_move_a_sent_message_to_delivered");

        let id = engine.send_email(&key, request()).await.expect("captured");

        let transition = engine
            .advance_event(&key, &id, EmailEvent::Delivered)
            .await
            .expect("sent can become delivered");

        assert_eq!(transition.from, EmailEvent::Sent);
        assert_eq!(transition.to, EmailEvent::Delivered);
        assert_eq!(
            engine
                .get_email(&key, &id)
                .await
                .expect("it is there")
                .last_event,
            EmailEvent::Delivered
        );
    }

    #[tokio::test]
    async fn should_refuse_a_transition_the_lifecycle_forbids() {
        let engine = engine();
        let key = key("re_should_refuse_an_illegal_transition");

        let id = engine.send_email(&key, request()).await.expect("captured");
        engine
            .advance_event(&key, &id, EmailEvent::Bounced)
            .await
            .expect("sent can bounce");

        let error = engine
            .advance_event(&key, &id, EmailEvent::Delivered)
            .await
            .expect_err("a bounced message cannot then be delivered");

        assert!(matches!(error, Error::Validation { .. }));
    }

    #[tokio::test]
    async fn should_leave_the_message_untouched_when_a_transition_is_refused() {
        let engine = engine();
        let key = key("re_should_leave_the_message_untouched_on_refusal");

        let id = engine.send_email(&key, request()).await.expect("captured");

        let _refused = engine.advance_event(&key, &id, EmailEvent::Scheduled).await;

        assert_eq!(
            engine
                .get_email(&key, &id)
                .await
                .expect("it is there")
                .last_event,
            EmailEvent::Sent
        );
    }

    #[tokio::test]
    async fn should_not_advance_another_keys_message() {
        let engine = engine();
        let mine = key("re_should_not_advance_another_keys_message_mine");
        let theirs = key("re_should_not_advance_another_keys_message_theirs");

        let id = engine.send_email(&mine, request()).await.expect("captured");

        assert!(matches!(
            engine
                .advance_event(&theirs, &id, EmailEvent::Delivered)
                .await,
            Err(Error::NotFound)
        ));
    }

    #[tokio::test]
    async fn should_walk_a_message_through_the_whole_happy_lifecycle() {
        let engine = engine();
        let key = key("re_should_walk_the_whole_happy_lifecycle");

        let id = engine.send_email(&key, request()).await.expect("captured");

        for next in [
            EmailEvent::Delivered,
            EmailEvent::Opened,
            EmailEvent::Clicked,
            EmailEvent::Complained,
        ] {
            engine
                .advance_event(&key, &id, next)
                .await
                .unwrap_or_else(|error| panic!("{next} should be reachable: {error}"));
        }

        assert_eq!(
            engine
                .get_email(&key, &id)
                .await
                .expect("it is there")
                .last_event,
            EmailEvent::Complained
        );
    }
}

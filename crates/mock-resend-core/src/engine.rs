//! The engine: the one handle the HTTP and SMTP layers hold.
//!
//! It owns the clock and the store, and every behaviour is a method on it. A handler
//! parses a request, calls one of these, and maps the error; it never reaches past this
//! into the store or appends an event itself.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use tokio::sync::mpsc::UnboundedSender;

use crate::error::{Error, Result};
use crate::events::EmailEvent;
use crate::fault::{Allowance, FaultTarget, Faults, RateLimiter};
use crate::idempotency::{Begin, Idempotency, IdempotencyKey, Outcome as IdempotentOutcome};
use crate::mailbox::SendEmail;
use crate::model::{ApiKey, EmailId, Message};
use crate::schedule::{Clock, SystemClock};
use crate::store::{Store, memory::MemoryStore};
use crate::webhook::Webhooks;

/// The mock-resend engine.
///
/// Cheap to clone: both the store and the clock sit behind an [`Arc`], so every
/// listener and every request handler shares one mailbox.
#[derive(Clone)]
pub struct Engine {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    idempotency: Arc<Idempotency>,
    faults: Arc<Faults>,
    rate_limiter: Arc<RateLimiter>,
    webhooks: Arc<Webhooks>,
    /// Where transitions are announced, if anything is listening.
    ///
    /// A channel rather than a direct call, because the thing that wants them makes
    /// outbound requests and this crate must not know that outbound requests exist.
    transitions: Option<UnboundedSender<Transition>>,
    /// How long after being handed off a message is delivered. `None` turns automatic
    /// delivery off, which is the default.
    delivery_delay: Option<Duration>,
}

impl Engine {
    /// An engine with the in-memory store and the wall clock.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::new(Arc::new(MemoryStore::new()), Arc::new(SystemClock::new()))
    }

    /// An engine over a given store and clock.
    #[must_use]
    pub fn new(store: Arc<dyn Store>, clock: Arc<dyn Clock>) -> Self {
        Self {
            store,
            clock,
            idempotency: Arc::new(Idempotency::new()),
            faults: Arc::new(Faults::new()),
            rate_limiter: Arc::new(RateLimiter::new(None)),
            webhooks: Arc::new(Webhooks::new()),
            transitions: None,
            delivery_delay: None,
        }
    }

    /// Announces every delivery event on `sender`.
    ///
    /// `mock-resend-webhooks` passes its own receiver's sender here. Without one,
    /// transitions still happen and are still reported to the caller; nothing is
    /// delivered anywhere.
    #[must_use]
    pub fn with_transition_sink(mut self, sender: UnboundedSender<Transition>) -> Self {
        self.transitions = Some(sender);
        self
    }

    /// Limits every key to `limit` requests a second.
    ///
    /// Off by default, even though real Resend allows 10 a second: a test suite that
    /// trips a limit it did not ask for is a flaky test suite.
    #[must_use]
    pub fn with_rate_limit(mut self, limit: Option<u32>) -> Self {
        self.rate_limiter = Arc::new(RateLimiter::new(limit));
        self
    }

    /// Delivers a message `delay` after it is handed off.
    ///
    /// Off by default, so a message stays at `sent` until something advances it. A
    /// suite that wants the full lifecycle without driving each step turns this on.
    #[must_use]
    pub fn with_delivery_delay(mut self, delay: Option<Duration>) -> Self {
        self.delivery_delay = delay;
        self
    }

    /// The engine's current time.
    ///
    /// Comes from the clock, which a test can move. Nothing reads the wall clock
    /// directly.
    #[must_use]
    pub fn now(&self) -> DateTime<Utc> {
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
        self.send_email_idempotent(key, request, None).await
    }

    /// Validates and captures a message, honouring an `Idempotency-Key`.
    ///
    /// A retry with the same key and the same payload returns the original id without
    /// capturing a second message. A retry with the same key and a different payload is
    /// [`Error::InvalidIdempotentRequest`], because the caller is reusing a key.
    ///
    /// # Errors
    ///
    /// Whatever [`SendEmail::validate`] rejects, the idempotency errors above, or
    /// [`Error::Store`] if the backend fails.
    pub async fn send_email_idempotent(
        &self,
        key: &ApiKey,
        request: SendEmail,
        idempotency_key: Option<&IdempotencyKey>,
    ) -> Result<EmailId> {
        let Some(idempotency_key) = idempotency_key else {
            return self.capture(key, request).await;
        };

        let fingerprint = request.fingerprint();
        let now = self.clock.now();

        match self
            .idempotency
            .begin(key, idempotency_key, fingerprint, now)?
        {
            Begin::Replay(IdempotentOutcome::Email(id)) => return Ok(id),
            // A key used for a batch cannot be replayed as a single send. Resend treats
            // a mismatched payload as a conflict, and a different endpoint is the most
            // mismatched payload there is.
            Begin::Replay(IdempotentOutcome::Batch(_)) => {
                return Err(Error::InvalidIdempotentRequest);
            }
            Begin::Proceed => {}
        }

        match self.capture(key, request).await {
            Ok(id) => {
                self.idempotency.complete(
                    key,
                    idempotency_key,
                    fingerprint,
                    IdempotentOutcome::Email(id),
                    now,
                )?;

                Ok(id)
            }
            Err(error) => {
                // A rejected request is not remembered, so the caller can fix the
                // payload and retry under the same key.
                self.idempotency.abandon(key, idempotency_key)?;

                Err(error)
            }
        }
    }

    /// Validates and stores one message, with no idempotency involved.
    async fn capture(&self, key: &ApiKey, request: SendEmail) -> Result<EmailId> {
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
            origin: request.origin,
            message_id: Some(message_id_for(id)),
        };

        self.store.put(message).await?;

        // A freshly captured message has arrived at its first event, which is the
        // `email.sent` or `email.scheduled` a handler expects to receive.
        self.announce(Transition {
            key: key.clone(),
            id,
            from: EmailEvent::Queued,
            to: last_event,
            at: now,
        });

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
        self.send_batch_idempotent(key, requests, validation, None)
            .await
    }

    /// Captures a batch, honouring an `Idempotency-Key`.
    ///
    /// The fingerprint covers the whole batch, so a retry has to repeat every entry in
    /// the same order to replay rather than capture again.
    ///
    /// # Errors
    ///
    /// As [`Engine::send_batch`], plus the idempotency errors.
    pub async fn send_batch_idempotent(
        &self,
        key: &ApiKey,
        requests: Vec<SendEmail>,
        validation: BatchValidation,
        idempotency_key: Option<&IdempotencyKey>,
    ) -> Result<BatchOutcome> {
        let Some(idempotency_key) = idempotency_key else {
            return self.capture_batch(key, requests, validation).await;
        };

        let fingerprint = crate::idempotency::fingerprint_of(&requests);
        let now = self.clock.now();

        match self
            .idempotency
            .begin(key, idempotency_key, fingerprint, now)?
        {
            Begin::Replay(IdempotentOutcome::Batch(outcome)) => return Ok(outcome),
            Begin::Replay(IdempotentOutcome::Email(_)) => {
                return Err(Error::InvalidIdempotentRequest);
            }
            Begin::Proceed => {}
        }

        match self.capture_batch(key, requests, validation).await {
            Ok(outcome) => {
                self.idempotency.complete(
                    key,
                    idempotency_key,
                    fingerprint,
                    IdempotentOutcome::Batch(outcome.clone()),
                    now,
                )?;

                Ok(outcome)
            }
            Err(error) => {
                self.idempotency.abandon(key, idempotency_key)?;

                Err(error)
            }
        }
    }

    /// Captures a batch, with no idempotency involved.
    async fn capture_batch(
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

            ids.push(self.capture(key, request).await?);
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
        scheduled_at: DateTime<Utc>,
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

    /// Moves time-driven mail along: scheduled sends that are due, and sent messages
    /// whose delivery delay has elapsed.
    ///
    /// Called by the server on an interval and by the admin API after the clock moves,
    /// so a test advances the clock and ticks rather than sleeping. Returns the
    /// transitions that happened, which is what the webhook sender needs.
    ///
    /// Automatic delivery is off unless a delivery delay is configured, globally or for
    /// the key. With it off a message stays at `sent` until something advances it, which
    /// keeps a test that only checks "was it accepted" from racing the ticker.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the backend fails.
    pub async fn tick(&self) -> Result<Vec<Transition>> {
        let now = self.clock.now();
        let in_flight = self.store.in_flight().await?;

        let mut transitions = Vec::new();
        for mut message in in_flight {
            let from = message.last_event;

            let Some(to) = self.next_event_for(&message, now)? else {
                continue;
            };

            if !from.can_transition_to(to) {
                continue;
            }

            message.last_event = to;

            let transition = Transition {
                key: message.key.clone(),
                id: message.id,
                from,
                to,
                at: now,
            };

            self.store.put(message).await?;
            self.announce(transition.clone());
            transitions.push(transition);
        }

        Ok(transitions)
    }

    /// What `message` should become by `now`, if anything.
    fn next_event_for(&self, message: &Message, now: DateTime<Utc>) -> Result<Option<EmailEvent>> {
        match message.last_event {
            EmailEvent::Scheduled => Ok(message
                .scheduled_at
                .is_some_and(|scheduled_at| scheduled_at <= now)
                .then_some(EmailEvent::Sent)),

            EmailEvent::Sent => {
                let faults = self.faults.for_key(&message.key)?;

                let Some(delay) = faults.delivery_delay.or(self.delivery_delay) else {
                    return Ok(None);
                };

                // Delivery is measured from when the message was handed off, which for a
                // scheduled send is its send time rather than when it was captured.
                let sent_at = message.scheduled_at.unwrap_or(message.created_at);

                if now.signed_duration_since(sent_at) < delay {
                    return Ok(None);
                }

                // A delivery rule is what makes a bounce testable without a mail server
                // that misbehaves on request.
                Ok(Some(
                    faults
                        .delivery
                        .map_or(EmailEvent::Delivered, |rule| rule.outcome),
                ))
            }

            _ => Ok(None),
        }
    }

    /// Consults `key`'s fault rules for a request of this kind.
    ///
    /// Called once per request, early, before any work. An endpoint that skips this
    /// cannot be tested for error handling.
    ///
    /// # Errors
    ///
    /// [`Error::Injected`] when a rule fires, carrying the error the rule chose.
    pub fn check_fault(&self, key: &ApiKey, target: FaultTarget) -> Result<()> {
        match self.faults.take(key, target)? {
            Some(error) => Err(Error::Injected(error)),
            None => Ok(()),
        }
    }

    /// Counts a request against `key`'s rate limit.
    ///
    /// Returns `None` when no limit is configured, so a caller can leave the
    /// `ratelimit-*` headers off entirely rather than reporting a limit that does not
    /// exist.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the limiter's lock is poisoned.
    pub fn check_rate_limit(&self, key: &ApiKey) -> Result<Option<Allowance>> {
        self.rate_limiter.check(key, self.clock.now())
    }

    /// The fault register, for the admin API to configure.
    #[must_use]
    pub fn faults(&self) -> &Faults {
        &self.faults
    }

    /// The webhook endpoint registry, for the admin API to configure.
    #[must_use]
    pub fn webhooks(&self) -> &Webhooks {
        &self.webhooks
    }

    /// Moves the engine's clock forward, reporting whether it could.
    ///
    /// This is what makes "this email goes out in 24 hours" a millisecond instead of a
    /// day. The caller is expected to [`Engine::tick`] afterwards, which is what
    /// actually sends anything the jump made due.
    pub fn advance_clock(&self, by: Duration) -> bool {
        self.clock.advance(by)
    }

    /// Counts what has been captured under `key`, by delivery event.
    ///
    /// Not a Resend endpoint. The admin API reports it so a suite can assert on what it
    /// sent without retrieving every message.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the backend fails.
    pub async fn stats(&self, key: &ApiKey) -> Result<Stats> {
        let messages = self.store.list(key).await?;

        let mut by_event: BTreeMap<&'static str, usize> = BTreeMap::new();
        for message in &messages {
            *by_event.entry(message.last_event.as_str()).or_default() += 1;
        }

        Ok(Stats {
            captured: messages.len(),
            by_event,
            webhook_endpoints: self.webhooks.for_key(key)?.len(),
            fault_rules: self.faults.for_key(key)?.rules.len(),
        })
    }

    /// Announces a transition, if anything is listening.
    ///
    /// A closed or absent channel is not an error. Nothing the caller asked for depends
    /// on a webhook being delivered, and failing a send because a listener went away
    /// would make webhook delivery a hidden dependency of capturing mail.
    fn announce(&self, transition: Transition) {
        if let Some(sender) = &self.transitions {
            let _delivered = sender.send(transition);
        }
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

        let transition = Transition {
            key: key.clone(),
            id: *id,
            from,
            to: next,
            at,
        };
        self.announce(transition.clone());

        Ok(transition)
    }

    /// Discards everything under `key`, or the whole store when `key` is `None`.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the backend fails.
    pub async fn reset(&self, key: Option<&ApiKey>) -> Result<()> {
        // Everything scoped to the key goes, not just the mail. A key whose replay
        // records survived would answer the next identical send with an id that no
        // longer resolves, and a surviving fault rule would break the next test to pick
        // that key up.
        self.idempotency.reset(key)?;
        self.faults.reset(key)?;
        self.rate_limiter.reset(key)?;
        self.webhooks.reset(key)?;
        self.store.reset(key).await
    }

    /// Every key that has captured mail.
    ///
    /// The console needs this because nothing hands it a key: a developer opens it to
    /// find out what their suite sent, under whatever keys the suite happened to use.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the backend fails.
    pub async fn keys(&self) -> Result<Vec<ApiKey>> {
        let mut keys = self.store.keys().await?;
        keys.sort();

        Ok(keys)
    }

    /// What each key has captured, newest first.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the backend fails.
    pub async fn key_summaries(&self) -> Result<Vec<KeySummary>> {
        let mut summaries = Vec::new();

        for key in self.keys().await? {
            let messages = self.store.list(&key).await?;

            summaries.push(KeySummary {
                captured: messages.len(),
                last_captured_at: messages.iter().map(|message| message.created_at).max(),
                key,
            });
        }

        // Most recently used first: a console open beside a test run wants the key that
        // is doing something, not the one that is alphabetically first.
        summaries.sort_by(|left, right| {
            right
                .last_captured_at
                .cmp(&left.last_captured_at)
                .then_with(|| left.key.cmp(&right.key))
        });

        Ok(summaries)
    }

    /// Captured mail across every key, newest first.
    ///
    /// Not a Resend endpoint, and deliberately not scoped: the console's whole job is to
    /// show what a suite sent, and a suite gives every test its own key.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the backend fails.
    pub async fn list_all_emails(&self, limit: usize) -> Result<Vec<Message>> {
        let mut messages = Vec::new();

        for key in self.keys().await? {
            messages.extend(self.store.list(&key).await?);
        }

        // Ties broken by id so the order is stable: a test clock can hand out the same
        // instant twice, and a list that reshuffles between polls is unreadable.
        messages.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        messages.truncate(limit);

        Ok(messages)
    }

    /// Finds a message by id without being told which key owns it.
    ///
    /// For the console, which follows a link rather than knowing a key. The Resend
    /// surface never uses this: there, a message belongs to the key that asks for it,
    /// and that is the isolation guarantee.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when no key has it, or [`Error::Store`] if the backend fails.
    pub async fn find_email(&self, id: &EmailId) -> Result<Message> {
        for key in self.keys().await? {
            if let Some(message) = self.store.get(&key, id).await? {
                return Ok(message);
            }
        }

        Err(Error::NotFound)
    }
}

/// What one key has captured.
#[derive(Debug, Clone)]
pub struct KeySummary {
    /// The key itself.
    pub key: ApiKey,
    /// How many messages it holds.
    pub captured: usize,
    /// When the most recent one arrived, if there is one.
    pub last_captured_at: Option<DateTime<Utc>>,
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

/// What a key has done, for the admin API to report.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    /// How many messages have been captured.
    pub captured: usize,
    /// How many are at each delivery event, keyed by the wire form.
    pub by_event: BTreeMap<&'static str, usize>,
    /// How many webhook endpoints are registered.
    pub webhook_endpoints: usize,
    /// How many fault rules are waiting to fire.
    pub fault_rules: usize,
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
    pub at: DateTime<Utc>,
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

#[cfg(test)]
mod idempotent_sends {
    use std::sync::Arc;

    use chrono::Duration;

    use super::{BatchValidation, Engine};
    use crate::error::Error;
    use crate::idempotency::{IdempotencyKey, WINDOW_HOURS};
    use crate::mailbox::SendEmail;
    use crate::model::ApiKey;
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

    fn idempotency_key() -> IdempotencyKey {
        IdempotencyKey::parse("order-1234").expect("valid")
    }

    fn request(subject: &str) -> SendEmail {
        let mut request = SendEmail::new(
            "Acme <onboarding@example.com>",
            vec!["delivered@example.org".to_owned()],
            subject,
        );
        request.text = Some("It works".to_owned());
        request
    }

    #[tokio::test]
    async fn should_capture_only_once_for_a_repeated_idempotency_key() {
        let (engine, _clock) = engine();
        let key = key("re_should_capture_once_for_a_repeated_key");
        let idempotency = idempotency_key();

        let first = engine
            .send_email_idempotent(&key, request("Hello"), Some(&idempotency))
            .await
            .expect("captured");
        let second = engine
            .send_email_idempotent(&key, request("Hello"), Some(&idempotency))
            .await
            .expect("replayed");

        assert_eq!(first, second);
        assert_eq!(
            engine.list_emails(&key).await.expect("list").len(),
            1,
            "a replay must not capture a second message"
        );
    }

    #[tokio::test]
    async fn should_capture_twice_without_an_idempotency_key() {
        let (engine, _clock) = engine();
        let key = key("re_should_capture_twice_without_a_key");

        let first = engine
            .send_email_idempotent(&key, request("Hello"), None)
            .await
            .expect("captured");
        let second = engine
            .send_email_idempotent(&key, request("Hello"), None)
            .await
            .expect("captured");

        assert_ne!(first, second);
        assert_eq!(engine.list_emails(&key).await.expect("list").len(), 2);
    }

    #[tokio::test]
    async fn should_reject_a_reused_key_with_a_different_payload() {
        let (engine, _clock) = engine();
        let key = key("re_should_reject_a_reused_key_different_payload");
        let idempotency = idempotency_key();

        let _first = engine
            .send_email_idempotent(&key, request("Hello"), Some(&idempotency))
            .await
            .expect("captured");

        let error = engine
            .send_email_idempotent(&key, request("Something else"), Some(&idempotency))
            .await
            .expect_err("the payload changed");

        assert!(matches!(error, Error::InvalidIdempotentRequest));
    }

    #[tokio::test]
    async fn should_not_capture_anything_when_a_reused_key_is_rejected() {
        let (engine, _clock) = engine();
        let key = key("re_should_not_capture_on_a_rejected_reuse");
        let idempotency = idempotency_key();

        let _first = engine
            .send_email_idempotent(&key, request("Hello"), Some(&idempotency))
            .await
            .expect("captured");
        let _rejected = engine
            .send_email_idempotent(&key, request("Different"), Some(&idempotency))
            .await;

        assert_eq!(engine.list_emails(&key).await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn should_capture_again_once_the_window_has_passed() {
        let (engine, clock) = engine();
        let key = key("re_should_capture_again_after_the_window");
        let idempotency = idempotency_key();

        let first = engine
            .send_email_idempotent(&key, request("Hello"), Some(&idempotency))
            .await
            .expect("captured");

        clock.advance_secs(Duration::hours(WINDOW_HOURS).num_seconds());

        let second = engine
            .send_email_idempotent(&key, request("Hello"), Some(&idempotency))
            .await
            .expect("the record has expired");

        assert_ne!(first, second);
        assert_eq!(engine.list_emails(&key).await.expect("list").len(), 2);
    }

    #[tokio::test]
    async fn should_let_a_retry_proceed_after_the_first_attempt_was_invalid() {
        let (engine, _clock) = engine();
        let key = key("re_should_let_a_retry_proceed_after_invalid");
        let idempotency = idempotency_key();

        let _rejected = engine
            .send_email_idempotent(&key, SendEmail::default(), Some(&idempotency))
            .await
            .expect_err("an empty request is rejected");

        assert!(
            engine
                .send_email_idempotent(&key, request("Fixed"), Some(&idempotency))
                .await
                .is_ok(),
            "a rejected request must not burn the key"
        );
    }

    #[tokio::test]
    async fn should_keep_one_idempotency_key_separate_across_api_keys() {
        let (engine, _clock) = engine();
        let mine = key("re_should_keep_idempotency_separate_sends_mine");
        let theirs = key("re_should_keep_idempotency_separate_sends_theirs");
        let idempotency = idempotency_key();

        let first = engine
            .send_email_idempotent(&mine, request("Hello"), Some(&idempotency))
            .await
            .expect("captured");
        let second = engine
            .send_email_idempotent(&theirs, request("Hello"), Some(&idempotency))
            .await
            .expect("captured under a different namespace");

        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn should_ignore_header_order_when_comparing_payloads() {
        let (engine, _clock) = engine();
        let key = key("re_should_ignore_header_order_in_payloads");
        let idempotency = idempotency_key();

        let mut first = request("Hello");
        let _a = first.headers.insert("X-A".to_owned(), "1".to_owned());
        let _b = first.headers.insert("X-B".to_owned(), "2".to_owned());

        let mut second = request("Hello");
        let _b2 = second.headers.insert("X-B".to_owned(), "2".to_owned());
        let _a2 = second.headers.insert("X-A".to_owned(), "1".to_owned());

        let original = engine
            .send_email_idempotent(&key, first, Some(&idempotency))
            .await
            .expect("captured");
        let replayed = engine
            .send_email_idempotent(&key, second, Some(&idempotency))
            .await
            .expect("the same headers in another order are the same payload");

        assert_eq!(original, replayed);
    }

    #[tokio::test]
    async fn should_replay_a_whole_batch_for_a_repeated_key() {
        let (engine, _clock) = engine();
        let key = key("re_should_replay_a_whole_batch");
        let idempotency = idempotency_key();

        let batch = vec![request("one"), request("two")];

        let first = engine
            .send_batch_idempotent(
                &key,
                batch.clone(),
                BatchValidation::Strict,
                Some(&idempotency),
            )
            .await
            .expect("captured");
        let second = engine
            .send_batch_idempotent(&key, batch, BatchValidation::Strict, Some(&idempotency))
            .await
            .expect("replayed");

        assert_eq!(first.ids, second.ids);
        assert_eq!(engine.list_emails(&key).await.expect("list").len(), 2);
    }

    #[tokio::test]
    async fn should_reject_a_batch_key_reused_for_a_single_send() {
        let (engine, _clock) = engine();
        let key = key("re_should_reject_a_batch_key_on_a_single_send");
        let idempotency = idempotency_key();

        let _batch = engine
            .send_batch_idempotent(
                &key,
                vec![request("one")],
                BatchValidation::Strict,
                Some(&idempotency),
            )
            .await
            .expect("captured");

        assert!(matches!(
            engine
                .send_email_idempotent(&key, request("one"), Some(&idempotency))
                .await,
            Err(Error::InvalidIdempotentRequest)
        ));
    }

    #[tokio::test]
    async fn should_reject_a_batch_retry_whose_entries_changed_order() {
        let (engine, _clock) = engine();
        let key = key("re_should_reject_a_reordered_batch_retry");
        let idempotency = idempotency_key();

        let _first = engine
            .send_batch_idempotent(
                &key,
                vec![request("one"), request("two")],
                BatchValidation::Strict,
                Some(&idempotency),
            )
            .await
            .expect("captured");

        assert!(matches!(
            engine
                .send_batch_idempotent(
                    &key,
                    vec![request("two"), request("one")],
                    BatchValidation::Strict,
                    Some(&idempotency),
                )
                .await,
            Err(Error::InvalidIdempotentRequest)
        ));
    }

    #[tokio::test]
    async fn should_forget_replay_records_when_the_key_is_reset() {
        let (engine, _clock) = engine();
        let key = key("re_should_forget_replays_on_reset");
        let idempotency = idempotency_key();

        let first = engine
            .send_email_idempotent(&key, request("Hello"), Some(&idempotency))
            .await
            .expect("captured");

        engine.reset(Some(&key)).await.expect("reset");

        let second = engine
            .send_email_idempotent(&key, request("Hello"), Some(&idempotency))
            .await
            .expect("captured again");

        assert_ne!(
            first, second,
            "replaying an id whose message was wiped would hand back a dangling id"
        );
    }
}

#[cfg(test)]
mod automatic_delivery {
    use std::sync::Arc;

    use chrono::Duration;

    use super::Engine;
    use crate::events::EmailEvent;
    use crate::fault::DeliveryRule;
    use crate::mailbox::SendEmail;
    use crate::model::ApiKey;
    use crate::schedule::{Clock, TestClock};
    use crate::store::memory::MemoryStore;

    fn engine(delay: Option<Duration>) -> (Engine, Arc<TestClock>) {
        let clock = Arc::new(TestClock::new());
        let shared: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
        let engine = Engine::new(Arc::new(MemoryStore::new()), shared).with_delivery_delay(delay);

        (engine, clock)
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
    async fn should_leave_a_message_at_sent_when_no_delay_is_configured() {
        let (engine, clock) = engine(None);
        let key = key("re_should_leave_a_message_at_sent");

        let id = engine.send_email(&key, request()).await.expect("captured");

        clock.advance_secs(86_400);
        let transitions = engine.tick().await.expect("tick");

        assert!(
            transitions.is_empty(),
            "automatic delivery is off by default, so nothing should move"
        );
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
    async fn should_deliver_a_message_once_the_delay_has_elapsed() {
        let (engine, clock) = engine(Some(Duration::seconds(30)));
        let key = key("re_should_deliver_after_the_delay");

        let id = engine.send_email(&key, request()).await.expect("captured");

        clock.advance_secs(30);
        let transitions = engine.tick().await.expect("tick");

        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to, EmailEvent::Delivered);
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
    async fn should_not_deliver_before_the_delay_has_elapsed() {
        let (engine, clock) = engine(Some(Duration::seconds(30)));
        let key = key("re_should_not_deliver_before_the_delay");

        let _id = engine.send_email(&key, request()).await.expect("captured");

        clock.advance_secs(29);

        assert!(engine.tick().await.expect("tick").is_empty());
    }

    #[tokio::test]
    async fn should_deliver_only_once() {
        let (engine, clock) = engine(Some(Duration::seconds(1)));
        let key = key("re_should_deliver_only_once");

        let _id = engine.send_email(&key, request()).await.expect("captured");

        clock.advance_secs(10);

        assert_eq!(engine.tick().await.expect("tick").len(), 1);
        assert!(engine.tick().await.expect("tick").is_empty());
    }

    #[tokio::test]
    async fn should_measure_a_scheduled_sends_delay_from_its_send_time() {
        let (engine, clock) = engine(Some(Duration::seconds(30)));
        let key = key("re_should_measure_delay_from_send_time");

        let mut request = request();
        request.scheduled_at = Some(clock.now() + Duration::hours(1));
        let id = engine.send_email(&key, request).await.expect("captured");

        // Past the send time, but not yet 30 seconds past it.
        clock.advance_secs(3_610);

        let sent = engine.tick().await.expect("tick");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, EmailEvent::Sent);

        assert!(
            engine.tick().await.expect("tick").is_empty(),
            "delivery is measured from the send time, not from capture"
        );

        clock.advance_secs(30);
        assert_eq!(
            engine.tick().await.expect("tick")[0].to,
            EmailEvent::Delivered
        );
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
    async fn should_bounce_instead_of_delivering_when_a_rule_says_so() {
        let (engine, clock) = engine(Some(Duration::seconds(1)));
        let key = key("re_should_bounce_when_a_rule_says_so");

        engine
            .faults()
            .set_delivery(
                &key,
                Some(DeliveryRule {
                    outcome: EmailEvent::Bounced,
                }),
            )
            .expect("set");

        let id = engine.send_email(&key, request()).await.expect("captured");

        clock.advance_secs(5);
        let transitions = engine.tick().await.expect("tick");

        assert_eq!(transitions[0].to, EmailEvent::Bounced);
        assert_eq!(
            engine
                .get_email(&key, &id)
                .await
                .expect("it is there")
                .last_event,
            EmailEvent::Bounced
        );
    }

    #[tokio::test]
    async fn should_let_a_key_set_its_own_delivery_delay() {
        let (engine, clock) = engine(None);
        let key = key("re_should_let_a_key_set_its_own_delay");

        engine
            .faults()
            .set_delivery_delay(&key, Some(Duration::seconds(5)))
            .expect("set");

        let _id = engine.send_email(&key, request()).await.expect("captured");

        clock.advance_secs(5);

        assert_eq!(
            engine.tick().await.expect("tick")[0].to,
            EmailEvent::Delivered,
            "a per-key delay should apply even with automatic delivery off globally"
        );
    }

    #[tokio::test]
    async fn should_not_deliver_under_a_key_that_has_no_delay_of_its_own() {
        let (engine, clock) = engine(None);
        let configured = key("re_should_not_deliver_without_a_delay_configured");
        let other = key("re_should_not_deliver_without_a_delay_other");

        engine
            .faults()
            .set_delivery_delay(&configured, Some(Duration::seconds(1)))
            .expect("set");

        let _mine = engine
            .send_email(&configured, request())
            .await
            .expect("captured");
        let theirs = engine
            .send_email(&other, request())
            .await
            .expect("captured");

        clock.advance_secs(5);
        let _transitions = engine.tick().await.expect("tick");

        assert_eq!(
            engine
                .get_email(&other, &theirs)
                .await
                .expect("it is there")
                .last_event,
            EmailEvent::Sent,
            "one key's delivery settings must not reach another"
        );
    }
}

#[cfg(test)]
mod injected_faults {
    use std::sync::Arc;

    use super::Engine;
    use crate::error::Error;
    use crate::fault::{FaultRule, FaultTarget, ResendError};
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

    fn rule(remaining: Option<u32>) -> FaultRule {
        FaultRule {
            target: FaultTarget::Send,
            error: ResendError {
                name: "rate_limit_exceeded".to_owned(),
                status: 429,
                message: "Too many requests.".to_owned(),
            },
            remaining,
        }
    }

    #[tokio::test]
    async fn should_pass_a_request_through_when_nothing_is_faulted() {
        let engine = engine();

        assert!(
            engine
                .check_fault(&key("re_should_pass_through_unfaulted"), FaultTarget::Send)
                .is_ok()
        );
    }

    #[tokio::test]
    async fn should_inject_the_error_a_rule_names() {
        let engine = engine();
        let key = key("re_should_inject_the_named_error");

        engine.faults().add_rule(&key, rule(None)).expect("added");

        let error = engine
            .check_fault(&key, FaultTarget::Send)
            .expect_err("the rule fires");

        let Error::Injected(injected) = error else {
            panic!("expected an injected error, got {error:?}");
        };

        assert_eq!(injected.name, "rate_limit_exceeded");
        assert_eq!(injected.status, 429);
    }

    #[tokio::test]
    async fn should_stop_injecting_once_a_counted_rule_runs_out() {
        let engine = engine();
        let key = key("re_should_stop_injecting_after_the_count");

        engine
            .faults()
            .add_rule(&key, rule(Some(1)))
            .expect("added");

        assert!(engine.check_fault(&key, FaultTarget::Send).is_err());
        assert!(
            engine.check_fault(&key, FaultTarget::Send).is_ok(),
            "fail-once-then-succeed is the case most retry logic has to handle"
        );
    }

    #[tokio::test]
    async fn should_clear_faults_when_the_key_is_reset() {
        let engine = engine();
        let key = key("re_should_clear_faults_on_reset");

        engine.faults().add_rule(&key, rule(None)).expect("added");
        engine.reset(Some(&key)).await.expect("reset");

        assert!(
            engine.check_fault(&key, FaultTarget::Send).is_ok(),
            "a surviving fault would break the next test to pick this key up"
        );
    }

    #[tokio::test]
    async fn should_report_no_allowance_without_a_configured_limit() {
        let engine = engine();

        assert_eq!(
            engine
                .check_rate_limit(&key("re_should_report_no_allowance_engine"))
                .expect("checks"),
            None
        );
    }

    #[tokio::test]
    async fn should_refuse_a_request_past_the_configured_limit() {
        let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
        let engine = Engine::new(Arc::new(MemoryStore::new()), clock).with_rate_limit(Some(1));
        let key = key("re_should_refuse_past_the_limit_engine");

        let first = engine
            .check_rate_limit(&key)
            .expect("checks")
            .expect("configured");
        let second = engine
            .check_rate_limit(&key)
            .expect("checks")
            .expect("configured");

        assert!(first.allowed);
        assert!(!second.allowed);
    }
}

#[cfg(test)]
mod header_injection {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use chrono::Duration;

    use crate::engine::Engine;
    use crate::error::Error;
    use crate::mailbox::SendEmail;
    use crate::model::{ApiKey, EmailId};
    use crate::schedule::{Clock, TestClock};
    use crate::store::disk::DiskStore;

    /// A temporary directory that cleans itself up.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mock-resend-{name}-{}", EmailId::generate()));

            std::fs::create_dir_all(&path).expect("the temporary directory is creatable");

            Self(path)
        }

        fn path(&self) -> &PathBuf {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _removed = std::fs::remove_dir_all(&self.0);
        }
    }

    fn key(name: &str) -> ApiKey {
        ApiKey::parse(name).expect("the test key is re_-prefixed")
    }

    /// An engine on the **disk** backend, which is the default the server runs with and
    /// the one these regressions only appear on.
    async fn engine(directory: &TempDir) -> (Engine, Arc<TestClock>) {
        let store = DiskStore::open(directory.path())
            .await
            .expect("the store opens");
        let clock = Arc::new(TestClock::new());
        let shared: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;

        (Engine::new(Arc::new(store), shared), clock)
    }

    fn request(header: &str, value: &str) -> SendEmail {
        let mut headers = BTreeMap::new();
        let _inserted = headers.insert(header.to_owned(), value.to_owned());

        let mut request = SendEmail::new(
            "attacker@example.com",
            vec!["victim-recipient@example.org".to_owned()],
            "a subject that must not leak",
        );
        request.text = Some("a body that must not leak".to_owned());
        request.headers = headers;
        request
    }

    #[tokio::test]
    async fn should_not_let_a_header_move_a_captured_message_into_another_keys_mailbox() {
        // The regression this exists for: a CR/LF in a header value used to append an
        // `X-Mock-Resend-Api-Key` to the stored `.eml`. Reading the file back then
        // reported the forged owner, and the next write — a scheduled send going out —
        // filed the message, its subject and its recipients under the victim's key, and
        // announced the `email.sent` webhook there too.
        let directory = TempDir::new("header-injection");
        let (engine, clock) = engine(&directory).await;

        let attacker = key("re_should_not_move_mail_attacker");
        let victim = key("re_should_not_move_mail_victim");

        let mut request = request(
            "X-Harmless",
            "ok\r\nX-Mock-Resend-Api-Key: re_should_not_move_mail_victim",
        );
        request.scheduled_at = Some(clock.now() + Duration::hours(1));

        let refused = engine
            .send_email(&attacker, request)
            .await
            .expect_err("a header value with a newline in it is not a header value");

        assert!(matches!(refused, Error::Validation { .. }));

        // And nothing reached either mailbox, including after the tick that used to be
        // the step that moved the file.
        clock.advance_secs(7_200);
        let _transitions = engine.tick().await.expect("tick succeeds");

        assert!(
            engine
                .list_emails(&attacker)
                .await
                .expect("list")
                .is_empty()
        );
        assert!(
            engine.list_emails(&victim).await.expect("list").is_empty(),
            "one key's mail must never appear under another's"
        );
    }

    #[tokio::test]
    async fn should_not_let_a_header_forge_a_delivery_event() {
        let directory = TempDir::new("forged-event");
        let (engine, _clock) = engine(&directory).await;
        let key = key("re_should_not_forge_a_delivery_event");

        let refused = engine
            .send_email(
                &key,
                request("X-Harmless", "ok\r\nX-Mock-Resend-Last-Event: delivered"),
            )
            .await
            .expect_err("refused at validation");

        assert!(matches!(refused, Error::Validation { .. }));
    }

    #[tokio::test]
    async fn should_keep_the_engines_own_headers_ahead_of_the_callers_in_the_eml() {
        // Defence in depth for the same bug. `from_eml` takes the first match for each
        // private header, so the engine's block has to be written before anything the
        // caller supplied: then even a value that somehow carried a newline could not
        // shadow the field that says who owns the message.
        let directory = TempDir::new("header-order");
        let (engine, _clock) = engine(&directory).await;
        let key = key("re_should_write_engine_headers_first");

        let id = engine
            .send_email(&key, request("X-Harmless", "ok"))
            .await
            .expect("captured");

        let message = engine.get_email(&key, &id).await.expect("it is there");
        let eml = crate::mime::to_eml(&message).expect("it writes");
        let text = String::from_utf8_lossy(&eml);

        let engine_block = text
            .find("X-Mock-Resend-Api-Key:")
            .expect("the engine's block is there");
        let caller_header = text
            .find("X-Harmless:")
            .expect("the caller's header is there");

        assert!(
            engine_block < caller_header,
            "the engine's headers have to come first"
        );
    }

    #[tokio::test]
    async fn should_ignore_an_eml_whose_recorded_owner_is_not_the_mailbox_it_is_in() {
        // The last layer: a file hand-edited in the volume, or a volume shared between
        // two data directories, must not hand one key's mail to another.
        let directory = TempDir::new("mismatched-owner");
        let attacker = key("re_should_ignore_a_mismatched_owner_attacker");
        let victim = key("re_should_ignore_a_mismatched_owner_victim");

        let id = {
            let (engine, _clock) = engine(&directory).await;

            engine
                .send_email(&attacker, request("X-Harmless", "ok"))
                .await
                .expect("captured")
        };

        // Rewrite the stored file so it claims the victim owns it, exactly as a
        // successful injection would have.
        let mailbox = std::fs::read_dir(directory.path())
            .expect("the root is readable")
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.is_dir())
            .expect("there is a mailbox");
        let file = mailbox.join(format!("{id}.eml"));

        let original = std::fs::read_to_string(&file).expect("the .eml is text");
        let forged = original.replace(attacker.as_str(), victim.as_str());
        std::fs::write(&file, forged).expect("the file is writable");

        let (engine, _clock) = engine(&directory).await;

        assert!(
            engine.list_emails(&victim).await.expect("list").is_empty(),
            "a file that claims another owner must not be indexed under it"
        );
        assert!(
            matches!(engine.get_email(&attacker, &id).await, Err(Error::NotFound)),
            "and the mailbox it sits in must not serve it either"
        );
    }
}

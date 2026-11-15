//! `Idempotency-Key` to result, inside a 24 hour window.
//!
//! A retry with the same key and the same payload returns the original result without
//! capturing a second message. A retry with the same key and a *different* payload is
//! an error, because the caller has a bug: they are reusing a key for two requests.
//!
//! The records are deliberately not persisted. Resend's window is 24 hours and
//! mock-resend loses captured mail on restart anyway, so a replay record outliving the
//! mail it points at would be the stranger behaviour.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::RwLock;

use chrono::{DateTime, Duration, Utc};

use crate::engine::BatchOutcome;
use crate::error::{Error, Result};
use crate::model::{ApiKey, EmailId};

/// How long Resend remembers an idempotency key.
///
/// <https://resend.com/docs/api-reference/errors>
pub const WINDOW_HOURS: i64 = 24;

/// The shortest key Resend accepts.
pub const MIN_KEY_LENGTH: usize = 1;

/// The longest key Resend accepts.
pub const MAX_KEY_LENGTH: usize = 256;

/// A validated `Idempotency-Key` header value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Accepts a key between 1 and 256 characters.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidIdempotencyKey`] outside that range, which is what Resend
    /// returns with a `400`.
    pub fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();

        // Counted in characters rather than bytes: the limit Resend documents is on the
        // key as the caller wrote it, and a key of 200 emoji is not 800 characters long.
        let length = trimmed.chars().count();

        if !(MIN_KEY_LENGTH..=MAX_KEY_LENGTH).contains(&length) {
            return Err(Error::InvalidIdempotencyKey);
        }

        Ok(Self(trimmed.to_owned()))
    }

    /// The key as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a request produced, so a replay can return it verbatim.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// A single send.
    Email(EmailId),
    /// A batch send, including the entries it refused.
    Batch(BatchOutcome),
}

/// What [`Idempotency::begin`] decided.
#[derive(Debug)]
pub enum Begin {
    /// No record for this key: do the work, then call [`Idempotency::complete`].
    Proceed,
    /// A matching record exists: return this instead of doing the work again.
    Replay(Outcome),
}

/// One remembered request.
#[derive(Debug, Clone)]
enum Record {
    /// Reserved but not finished. A second request on the same key is a conflict.
    InFlight { at: DateTime<Utc> },
    /// Finished, with the result to replay.
    Completed {
        fingerprint: u64,
        outcome: Outcome,
        at: DateTime<Utc>,
    },
}

impl Record {
    fn at(&self) -> DateTime<Utc> {
        match self {
            Self::InFlight { at } | Self::Completed { at, .. } => *at,
        }
    }
}

/// The per-key register of idempotency keys.
///
/// Scoped to an [`ApiKey`] like everything else: two tests using the same idempotency
/// key under different API keys do not collide.
#[derive(Debug, Default)]
pub struct Idempotency {
    records: RwLock<HashMap<(ApiKey, IdempotencyKey), Record>>,
}

/// Turns a poisoned lock into a storage failure.
fn poisoned() -> Error {
    Error::Store("the idempotency register's lock was poisoned by an earlier panic".to_owned())
}

impl Idempotency {
    /// An empty register.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserves `idempotency_key`, or reports what to replay.
    ///
    /// # Errors
    ///
    /// - [`Error::ConcurrentIdempotentRequests`] when the key is reserved and unfinished.
    /// - [`Error::InvalidIdempotentRequest`] when the key was used with a different
    ///   payload.
    pub fn begin(
        &self,
        key: &ApiKey,
        idempotency_key: &IdempotencyKey,
        fingerprint: u64,
        now: DateTime<Utc>,
    ) -> Result<Begin> {
        let mut records = self.records.write().map_err(|_| poisoned())?;

        // Evicting here rather than on a timer keeps the register free of background
        // work, and the clock a test moves is the same clock the eviction reads.
        records.retain(|_, record| !expired(record.at(), now));

        match records.entry((key.clone(), idempotency_key.clone())) {
            Entry::Vacant(slot) => {
                let _reserved = slot.insert(Record::InFlight { at: now });
                Ok(Begin::Proceed)
            }
            Entry::Occupied(slot) => match slot.get() {
                Record::InFlight { .. } => Err(Error::ConcurrentIdempotentRequests),
                Record::Completed {
                    fingerprint: original,
                    outcome,
                    ..
                } => {
                    if *original == fingerprint {
                        Ok(Begin::Replay(outcome.clone()))
                    } else {
                        Err(Error::InvalidIdempotentRequest)
                    }
                }
            },
        }
    }

    /// Records what the reserved request produced.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the register's lock is poisoned.
    pub fn complete(
        &self,
        key: &ApiKey,
        idempotency_key: &IdempotencyKey,
        fingerprint: u64,
        outcome: Outcome,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut records = self.records.write().map_err(|_| poisoned())?;

        let _replaced = records.insert(
            (key.clone(), idempotency_key.clone()),
            Record::Completed {
                fingerprint,
                outcome,
                at: now,
            },
        );

        Ok(())
    }

    /// Releases a reservation whose request failed.
    ///
    /// A rejected request is not remembered: the caller fixes the payload and retries
    /// with the same key, and that retry has to be allowed to run.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the register's lock is poisoned.
    pub fn abandon(&self, key: &ApiKey, idempotency_key: &IdempotencyKey) -> Result<()> {
        let mut records = self.records.write().map_err(|_| poisoned())?;

        // Only an unfinished reservation is dropped. A completed record belongs to an
        // earlier, successful request and is not this one's to remove.
        if let Entry::Occupied(slot) = records.entry((key.clone(), idempotency_key.clone()))
            && matches!(slot.get(), Record::InFlight { .. })
        {
            let _removed = slot.remove();
        }

        Ok(())
    }

    /// Forgets every record under `key`, or all of them when `key` is `None`.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the register's lock is poisoned.
    pub fn reset(&self, key: Option<&ApiKey>) -> Result<()> {
        let mut records = self.records.write().map_err(|_| poisoned())?;

        match key {
            Some(key) => records.retain(|(owner, _), _| owner != key),
            None => records.clear(),
        }

        Ok(())
    }
}

/// Whether a record made at `at` has fallen out of the window by `now`.
fn expired(at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(at) >= Duration::hours(WINDOW_HOURS)
}

/// A stable digest of anything hashable.
///
/// Used to tell "the same request again" from "a different request with the same key".
/// Only compared against other fingerprints from the same process, so the hasher's
/// per-process seeding is not a problem.
pub(crate) fn fingerprint_of<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod idempotency_key {
    use super::{IdempotencyKey, MAX_KEY_LENGTH};
    use crate::error::Error;

    #[test]
    fn should_accept_an_ordinary_key() {
        let key = IdempotencyKey::parse("order-1234").expect("valid");

        assert_eq!(key.as_str(), "order-1234");
    }

    #[test]
    fn should_accept_a_single_character_key() {
        assert!(IdempotencyKey::parse("a").is_ok());
    }

    #[test]
    fn should_accept_a_key_at_the_length_limit() {
        assert!(IdempotencyKey::parse(&"a".repeat(MAX_KEY_LENGTH)).is_ok());
    }

    #[test]
    fn should_reject_a_key_over_the_length_limit() {
        assert!(matches!(
            IdempotencyKey::parse(&"a".repeat(MAX_KEY_LENGTH + 1)),
            Err(Error::InvalidIdempotencyKey)
        ));
    }

    #[test]
    fn should_reject_an_empty_key() {
        assert!(matches!(
            IdempotencyKey::parse(""),
            Err(Error::InvalidIdempotencyKey)
        ));
    }

    #[test]
    fn should_reject_a_whitespace_only_key() {
        assert!(matches!(
            IdempotencyKey::parse("   "),
            Err(Error::InvalidIdempotencyKey)
        ));
    }

    #[test]
    fn should_measure_the_limit_in_characters_not_bytes() {
        // Each of these is four bytes and one character. Measuring bytes would refuse a
        // key Resend accepts.
        let key = "\u{1f4e7}".repeat(MAX_KEY_LENGTH);

        assert!(IdempotencyKey::parse(&key).is_ok());
    }
}

#[cfg(test)]
mod register {
    use chrono::{DateTime, Duration, Utc};

    use super::{Begin, Idempotency, IdempotencyKey, Outcome, WINDOW_HOURS};
    use crate::error::Error;
    use crate::model::{ApiKey, EmailId};

    fn api_key(name: &str) -> ApiKey {
        ApiKey::parse(name).expect("the test key is re_-prefixed")
    }

    fn idempotency_key() -> IdempotencyKey {
        IdempotencyKey::parse("order-1234").expect("valid")
    }

    fn now() -> DateTime<Utc> {
        DateTime::UNIX_EPOCH
    }

    #[test]
    fn should_let_a_first_request_proceed() {
        let register = Idempotency::new();

        let begin = register
            .begin(
                &api_key("re_should_let_a_first_request_proceed"),
                &idempotency_key(),
                1,
                now(),
            )
            .expect("a fresh key proceeds");

        assert!(matches!(begin, Begin::Proceed));
    }

    #[test]
    fn should_replay_a_retry_with_the_same_payload() {
        let register = Idempotency::new();
        let key = api_key("re_should_replay_a_matching_retry");
        let id = EmailId::generate();

        let _first = register.begin(&key, &idempotency_key(), 42, now());
        register
            .complete(&key, &idempotency_key(), 42, Outcome::Email(id), now())
            .expect("completes");

        let begin = register
            .begin(&key, &idempotency_key(), 42, now())
            .expect("a matching retry replays");

        let Begin::Replay(Outcome::Email(replayed)) = begin else {
            panic!("expected a replay of the original id, got {begin:?}");
        };

        assert_eq!(replayed, id);
    }

    #[test]
    fn should_reject_a_retry_with_a_different_payload() {
        let register = Idempotency::new();
        let key = api_key("re_should_reject_a_mismatched_retry");

        let _first = register.begin(&key, &idempotency_key(), 42, now());
        register
            .complete(
                &key,
                &idempotency_key(),
                42,
                Outcome::Email(EmailId::generate()),
                now(),
            )
            .expect("completes");

        assert!(matches!(
            register.begin(&key, &idempotency_key(), 99, now()),
            Err(Error::InvalidIdempotentRequest)
        ));
    }

    #[test]
    fn should_reject_a_second_request_while_the_first_is_in_flight() {
        let register = Idempotency::new();
        let key = api_key("re_should_reject_a_concurrent_request");

        let _first = register.begin(&key, &idempotency_key(), 42, now());

        assert!(matches!(
            register.begin(&key, &idempotency_key(), 42, now()),
            Err(Error::ConcurrentIdempotentRequests)
        ));
    }

    #[test]
    fn should_let_a_retry_proceed_after_the_first_attempt_was_abandoned() {
        let register = Idempotency::new();
        let key = api_key("re_should_let_a_retry_proceed_after_failure");

        let _first = register.begin(&key, &idempotency_key(), 42, now());
        register
            .abandon(&key, &idempotency_key())
            .expect("abandons");

        assert!(matches!(
            register
                .begin(&key, &idempotency_key(), 42, now())
                .expect("the retry proceeds"),
            Begin::Proceed
        ));
    }

    #[test]
    fn should_not_abandon_a_completed_record() {
        let register = Idempotency::new();
        let key = api_key("re_should_not_abandon_a_completed_record");
        let id = EmailId::generate();

        let _first = register.begin(&key, &idempotency_key(), 42, now());
        register
            .complete(&key, &idempotency_key(), 42, Outcome::Email(id), now())
            .expect("completes");
        register
            .abandon(&key, &idempotency_key())
            .expect("abandons nothing");

        assert!(matches!(
            register.begin(&key, &idempotency_key(), 42, now()),
            Ok(Begin::Replay(_))
        ));
    }

    #[test]
    fn should_forget_a_record_once_the_window_has_passed() {
        let register = Idempotency::new();
        let key = api_key("re_should_forget_a_record_after_the_window");

        let _first = register.begin(&key, &idempotency_key(), 42, now());
        register
            .complete(
                &key,
                &idempotency_key(),
                42,
                Outcome::Email(EmailId::generate()),
                now(),
            )
            .expect("completes");

        let later = now() + Duration::hours(WINDOW_HOURS);

        assert!(matches!(
            register
                .begin(&key, &idempotency_key(), 42, later)
                .expect("the record has expired"),
            Begin::Proceed
        ));
    }

    #[test]
    fn should_still_replay_just_inside_the_window() {
        let register = Idempotency::new();
        let key = api_key("re_should_still_replay_inside_the_window");

        let _first = register.begin(&key, &idempotency_key(), 42, now());
        register
            .complete(
                &key,
                &idempotency_key(),
                42,
                Outcome::Email(EmailId::generate()),
                now(),
            )
            .expect("completes");

        let later = now() + Duration::hours(WINDOW_HOURS) - Duration::seconds(1);

        assert!(matches!(
            register.begin(&key, &idempotency_key(), 42, later),
            Ok(Begin::Replay(_))
        ));
    }

    #[test]
    fn should_keep_the_same_idempotency_key_separate_under_two_api_keys() {
        let register = Idempotency::new();
        let mine = api_key("re_should_keep_idempotency_separate_mine");
        let theirs = api_key("re_should_keep_idempotency_separate_theirs");

        let _first = register.begin(&mine, &idempotency_key(), 42, now());
        register
            .complete(
                &mine,
                &idempotency_key(),
                42,
                Outcome::Email(EmailId::generate()),
                now(),
            )
            .expect("completes");

        assert!(
            matches!(
                register
                    .begin(&theirs, &idempotency_key(), 42, now())
                    .expect("a different api key is a different namespace"),
                Begin::Proceed
            ),
            "one test's idempotency key must not replay into another's"
        );
    }

    #[test]
    fn should_forget_only_the_key_it_was_asked_to_reset() {
        let register = Idempotency::new();
        let reset = api_key("re_should_forget_only_the_named_key_reset");
        let kept = api_key("re_should_forget_only_the_named_key_kept");

        for key in [&reset, &kept] {
            let _begun = register.begin(key, &idempotency_key(), 42, now());
            register
                .complete(
                    key,
                    &idempotency_key(),
                    42,
                    Outcome::Email(EmailId::generate()),
                    now(),
                )
                .expect("completes");
        }

        register.reset(Some(&reset)).expect("resets");

        assert!(matches!(
            register.begin(&reset, &idempotency_key(), 42, now()),
            Ok(Begin::Proceed)
        ));
        assert!(matches!(
            register.begin(&kept, &idempotency_key(), 42, now()),
            Ok(Begin::Replay(_))
        ));
    }
}

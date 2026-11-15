//! Per-key fault rules and the per-key rate limiter.
//!
//! Both are consulted once per request, early, before any work. An endpoint that does
//! not check them cannot be tested for error handling, which is the whole point: a
//! suite that wants to prove its retry logic works turns a fault on deliberately
//! instead of needing a broken mail server.
//!
//! Everything here is scoped to an [`ApiKey`]. There are no global switches, so a test
//! that makes every send return `429` cannot affect a test working under another key.

use std::collections::HashMap;
use std::sync::RwLock;

use chrono::{DateTime, Duration, Utc};

use crate::error::{Error, Result};
use crate::events::EmailEvent;
use crate::model::ApiKey;

/// A Resend error, as it appears on the wire.
///
/// Carries the `statusCode` because Resend's own error body does; it is a field of
/// Resend's data model rather than anything to do with a web framework.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResendError {
    /// The error `name` callers branch on.
    pub name: String,
    /// The status Resend serves it with.
    pub status: u16,
    /// The human-readable message.
    pub message: String,
}

/// Resend's documented error names and the statuses they come with.
///
/// Taken from the [errors reference](https://resend.com/docs/api-reference/errors) by
/// way of `resend_rs::types::ErrorKind`, whose doc comments name the status for each.
/// A fault may only inject a name from this list: inventing one would let a test assert
/// against an error real Resend never sends.
///
/// `service_unavailable` is the one judgement call. The SDK's own comment heads it
/// "500 Service Unavailable", which is self-contradictory, and `dns_lookup_failed`
/// beside it is headed 503. 503 is used here for both, and a rule may override the
/// status if you know better.
pub const RESEND_ERRORS: [(&str, u16); 21] = [
    ("invalid_idempotency_key", 400),
    ("validation_error", 400),
    ("missing_api_key", 401),
    ("restricted_api_key", 401),
    ("email_above_quota", 403),
    ("invalid_permission", 403),
    ("suspended_api_key", 403),
    ("not_found", 404),
    ("method_not_allowed", 405),
    ("concurrent_idempotent_requests", 409),
    ("invalid_idempotent_request", 409),
    ("resource_locked", 409),
    ("forge_not_enabled", 409),
    ("invalid_attachment", 422),
    ("invalid_parameter", 422),
    ("missing_required_field", 422),
    ("missing_required_parameter", 422),
    ("daily_quota_exceeded", 429),
    ("monthly_quota_exceeded", 429),
    ("rate_limit_exceeded", 429),
    ("application_error", 500),
];

/// The status Resend serves `name` with, if it is an error Resend has.
#[must_use]
pub fn status_for(name: &str) -> Option<u16> {
    if let Some((_, status)) = RESEND_ERRORS.iter().find(|(known, _)| *known == name) {
        return Some(*status);
    }

    // Not in the table above because the table is keyed by what the SDK documents with
    // a single status; see the note on `RESEND_ERRORS`.
    match name {
        "service_unavailable" | "dns_lookup_failed" => Some(503),
        _ => None,
    }
}

/// Which requests a fault applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultTarget {
    /// `POST /emails` and `POST /emails/batch`.
    Send,
    /// `GET /emails/{id}`.
    Retrieve,
    /// Every Resend endpoint. The admin API is never faulted: a test needs a working
    /// way to turn its own fault off again.
    Any,
}

impl FaultTarget {
    /// Parses the wire form used by the admin API.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "send" => Some(Self::Send),
            "retrieve" => Some(Self::Retrieve),
            "any" => Some(Self::Any),
            _ => None,
        }
    }

    /// The wire form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Retrieve => "retrieve",
            Self::Any => "any",
        }
    }

    /// Whether a fault aimed at `self` fires for a request aimed at `request`.
    #[must_use]
    pub fn covers(self, request: Self) -> bool {
        self == Self::Any || self == request
    }
}

/// A rule that makes matching requests fail.
#[derive(Debug, Clone)]
pub struct FaultRule {
    /// Which requests it applies to.
    pub target: FaultTarget,
    /// The error to return instead of doing the work.
    pub error: ResendError,
    /// How many more times it fires, or `None` to fire forever.
    ///
    /// A count is what makes "fail once, then succeed" testable, which is the case most
    /// retry logic actually has to handle.
    pub remaining: Option<u32>,
}

/// What a message becomes when the engine delivers it.
///
/// Without a rule a message is delivered. With one it bounces, is deferred, draws a
/// complaint or fails outright, so error handling can be tested without a mail server
/// that misbehaves on request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryRule {
    /// The event the message lands on instead of `delivered`.
    pub outcome: EmailEvent,
}

/// One key's faults.
#[derive(Debug, Clone, Default)]
pub struct KeyFaults {
    /// Rules that make requests fail, in the order they were added.
    pub rules: Vec<FaultRule>,
    /// What delivery does under this key.
    pub delivery: Option<DeliveryRule>,
    /// How long after being sent a message is delivered, overriding the default.
    pub delivery_delay: Option<Duration>,
}

/// The per-key fault register.
#[derive(Debug, Default)]
pub struct Faults {
    keys: RwLock<HashMap<ApiKey, KeyFaults>>,
}

/// Turns a poisoned lock into a storage failure.
fn poisoned() -> Error {
    Error::Store("the fault register's lock was poisoned by an earlier panic".to_owned())
}

impl Faults {
    /// An empty register. Nothing is faulted until something is added.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a rule under `key`.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the error name is not one Resend has, and
    /// [`Error::Store`] if the register's lock is poisoned.
    pub fn add_rule(&self, key: &ApiKey, rule: FaultRule) -> Result<()> {
        if status_for(&rule.error.name).is_none() {
            return Err(Error::validation(format!(
                "`{}` is not a Resend error name; injecting it would let a test assert against an error the real API never sends.",
                rule.error.name
            )));
        }

        let mut keys = self.keys.write().map_err(|_| poisoned())?;
        keys.entry(key.clone()).or_default().rules.push(rule);

        Ok(())
    }

    /// Sets what delivery does under `key`.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the outcome is not something a sent message can
    /// become, and [`Error::Store`] if the register's lock is poisoned.
    pub fn set_delivery(&self, key: &ApiKey, rule: Option<DeliveryRule>) -> Result<()> {
        if let Some(rule) = rule
            && !EmailEvent::Sent.can_transition_to(rule.outcome)
        {
            return Err(Error::validation(format!(
                "A sent email cannot become `{}`.",
                rule.outcome
            )));
        }

        let mut keys = self.keys.write().map_err(|_| poisoned())?;
        keys.entry(key.clone()).or_default().delivery = rule;

        Ok(())
    }

    /// Sets how long after sending a message is delivered under `key`.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a negative delay, and [`Error::Store`] if the
    /// register's lock is poisoned.
    pub fn set_delivery_delay(&self, key: &ApiKey, delay: Option<Duration>) -> Result<()> {
        if let Some(delay) = delay
            && delay < Duration::zero()
        {
            return Err(Error::validation("A delivery delay cannot be negative."));
        }

        let mut keys = self.keys.write().map_err(|_| poisoned())?;
        keys.entry(key.clone()).or_default().delivery_delay = delay;

        Ok(())
    }

    /// Returns `key`'s faults.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the register's lock is poisoned.
    pub fn for_key(&self, key: &ApiKey) -> Result<KeyFaults> {
        let keys = self.keys.read().map_err(|_| poisoned())?;

        Ok(keys.get(key).cloned().unwrap_or_default())
    }

    /// Consumes the first rule matching `target`, if there is one.
    ///
    /// Returns the error the caller should be given. A rule with a remaining count is
    /// decremented and dropped when it runs out, so "fail the next two sends" works.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the register's lock is poisoned.
    pub fn take(&self, key: &ApiKey, target: FaultTarget) -> Result<Option<ResendError>> {
        let mut keys = self.keys.write().map_err(|_| poisoned())?;

        let Some(faults) = keys.get_mut(key) else {
            return Ok(None);
        };

        let Some(position) = faults
            .rules
            .iter()
            .position(|rule| rule.target.covers(target))
        else {
            return Ok(None);
        };

        let rule = &mut faults.rules[position];
        let error = rule.error.clone();

        match rule.remaining {
            None => {}
            Some(1) => {
                let _spent = faults.rules.remove(position);
            }
            Some(remaining) => rule.remaining = Some(remaining.saturating_sub(1)),
        }

        Ok(Some(error))
    }

    /// Removes every rule under `key`, or every rule in the register.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the register's lock is poisoned.
    pub fn reset(&self, key: Option<&ApiKey>) -> Result<()> {
        let mut keys = self.keys.write().map_err(|_| poisoned())?;

        match key {
            Some(key) => {
                let _removed = keys.remove(key);
            }
            None => keys.clear(),
        }

        Ok(())
    }
}

/// A per-key request allowance.
///
/// Off unless a limit is configured. Real Resend allows 10 requests a second, but a
/// test suite that trips a limit it did not ask for is a flaky test suite, so a suite
/// that wants to prove its backoff works turns this on deliberately.
#[derive(Debug)]
pub struct RateLimiter {
    /// Requests allowed per second, per key. `None` disables the limiter entirely.
    limit: Option<u32>,
    windows: RwLock<HashMap<ApiKey, Window>>,
}

/// One key's current second.
#[derive(Debug, Clone, Copy)]
struct Window {
    started: DateTime<Utc>,
    used: u32,
}

/// What the limiter decided, and the numbers for the `ratelimit-*` headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowance {
    /// The configured ceiling.
    pub limit: u32,
    /// How many requests are left in the current window.
    pub remaining: u32,
    /// Seconds until the window resets.
    pub reset: u64,
    /// Whether this request is allowed through.
    pub allowed: bool,
}

impl RateLimiter {
    /// A limiter allowing `limit` requests per second per key, or no limit at all.
    #[must_use]
    pub fn new(limit: Option<u32>) -> Self {
        Self {
            limit,
            windows: RwLock::new(HashMap::new()),
        }
    }

    /// Whether a limit is configured.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.limit.is_some()
    }

    /// Counts a request against `key`'s allowance.
    ///
    /// Returns `None` when no limit is configured, so a caller can tell "unlimited"
    /// from "allowed" and leave the headers off entirely.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the limiter's lock is poisoned.
    pub fn check(&self, key: &ApiKey, now: DateTime<Utc>) -> Result<Option<Allowance>> {
        let Some(limit) = self.limit else {
            return Ok(None);
        };

        // A zero limit refuses everything, which is a legitimate way to test a client
        // that has to survive being shut out.
        if limit == 0 {
            return Ok(Some(Allowance {
                limit: 0,
                remaining: 0,
                reset: 1,
                allowed: false,
            }));
        }

        let mut windows = self.windows.write().map_err(|_| poisoned())?;
        let window = windows.entry(key.clone()).or_insert(Window {
            started: now,
            used: 0,
        });

        if now.signed_duration_since(window.started) >= Duration::seconds(1) {
            window.started = now;
            window.used = 0;
        }

        let allowed = window.used < limit;
        if allowed {
            window.used = window.used.saturating_add(1);
        }

        let elapsed = now.signed_duration_since(window.started);
        let reset = (Duration::seconds(1) - elapsed).num_milliseconds().max(0);

        Ok(Some(Allowance {
            limit,
            remaining: limit.saturating_sub(window.used),
            // Resend reports `ratelimit-reset` in seconds, and a sub-second remainder
            // rounds up: reporting 0 would tell a client to retry immediately into the
            // same refusal.
            reset: u64::try_from((reset + 999) / 1_000).unwrap_or(1),
            allowed,
        }))
    }

    /// Forgets `key`'s window, or every window.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the limiter's lock is poisoned.
    pub fn reset(&self, key: Option<&ApiKey>) -> Result<()> {
        let mut windows = self.windows.write().map_err(|_| poisoned())?;

        match key {
            Some(key) => {
                let _removed = windows.remove(key);
            }
            None => windows.clear(),
        }

        Ok(())
    }
}

#[cfg(test)]
mod resend_errors {
    use super::{RESEND_ERRORS, status_for};

    #[test]
    fn should_know_the_status_of_every_catalogued_error() {
        for (name, status) in RESEND_ERRORS {
            assert_eq!(status_for(name), Some(status));
        }
    }

    #[test]
    fn should_place_validation_error_at_four_hundred() {
        assert_eq!(status_for("validation_error"), Some(400));
    }

    #[test]
    fn should_place_rate_limit_exceeded_at_four_twenty_nine() {
        assert_eq!(status_for("rate_limit_exceeded"), Some(429));
    }

    #[test]
    fn should_know_the_errors_that_are_not_in_the_table() {
        assert_eq!(status_for("service_unavailable"), Some(503));
        assert_eq!(status_for("dns_lookup_failed"), Some(503));
    }

    #[test]
    fn should_not_know_an_invented_error_name() {
        assert_eq!(status_for("everything_is_fine"), None);
        assert_eq!(status_for("mock_resend_error"), None);
    }

    #[test]
    fn should_not_list_the_same_name_twice() {
        let mut names: Vec<&str> = RESEND_ERRORS.iter().map(|(name, _)| *name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();

        assert_eq!(names.len(), total);
    }
}

#[cfg(test)]
mod fault_target {
    use super::FaultTarget;

    #[test]
    fn should_round_trip_through_its_wire_form() {
        for target in [FaultTarget::Send, FaultTarget::Retrieve, FaultTarget::Any] {
            assert_eq!(FaultTarget::parse(target.as_str()), Some(target));
        }
    }

    #[test]
    fn should_ignore_case_when_parsing() {
        assert_eq!(FaultTarget::parse("SEND"), Some(FaultTarget::Send));
    }

    #[test]
    fn should_not_parse_an_unknown_target() {
        assert_eq!(FaultTarget::parse("webhooks"), None);
    }

    #[test]
    fn should_have_any_cover_every_target() {
        for request in [FaultTarget::Send, FaultTarget::Retrieve, FaultTarget::Any] {
            assert!(FaultTarget::Any.covers(request));
        }
    }

    #[test]
    fn should_not_have_a_specific_target_cover_another() {
        assert!(!FaultTarget::Send.covers(FaultTarget::Retrieve));
        assert!(!FaultTarget::Retrieve.covers(FaultTarget::Send));
    }
}

#[cfg(test)]
mod faults {
    use chrono::Duration;

    use super::{DeliveryRule, FaultRule, FaultTarget, Faults, ResendError};
    use crate::error::Error;
    use crate::events::EmailEvent;
    use crate::model::ApiKey;

    fn key(name: &str) -> ApiKey {
        ApiKey::parse(name).expect("the test key is re_-prefixed")
    }

    fn rate_limit(remaining: Option<u32>) -> FaultRule {
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

    #[test]
    fn should_fault_nothing_by_default() {
        let faults = Faults::new();

        assert_eq!(
            faults
                .take(
                    &key("re_should_fault_nothing_by_default"),
                    FaultTarget::Send
                )
                .expect("takes"),
            None
        );
    }

    #[test]
    fn should_return_the_injected_error_for_a_matching_request() {
        let faults = Faults::new();
        let key = key("re_should_return_the_injected_error");

        faults.add_rule(&key, rate_limit(None)).expect("added");

        let error = faults
            .take(&key, FaultTarget::Send)
            .expect("takes")
            .expect("the rule fires");

        assert_eq!(error.name, "rate_limit_exceeded");
        assert_eq!(error.status, 429);
    }

    #[test]
    fn should_not_fire_for_a_different_target() {
        let faults = Faults::new();
        let key = key("re_should_not_fire_for_another_target");

        faults.add_rule(&key, rate_limit(None)).expect("added");

        assert!(
            faults
                .take(&key, FaultTarget::Retrieve)
                .expect("takes")
                .is_none()
        );
    }

    #[test]
    fn should_keep_firing_a_rule_with_no_count() {
        let faults = Faults::new();
        let key = key("re_should_keep_firing_an_uncounted_rule");

        faults.add_rule(&key, rate_limit(None)).expect("added");

        for _attempt in 0..5 {
            assert!(
                faults
                    .take(&key, FaultTarget::Send)
                    .expect("takes")
                    .is_some()
            );
        }
    }

    #[test]
    fn should_fire_a_counted_rule_exactly_that_many_times() {
        let faults = Faults::new();
        let key = key("re_should_fire_a_counted_rule_n_times");

        faults.add_rule(&key, rate_limit(Some(2))).expect("added");

        assert!(
            faults
                .take(&key, FaultTarget::Send)
                .expect("takes")
                .is_some()
        );
        assert!(
            faults
                .take(&key, FaultTarget::Send)
                .expect("takes")
                .is_some()
        );
        assert!(
            faults
                .take(&key, FaultTarget::Send)
                .expect("takes")
                .is_none(),
            "a rule set to fire twice must not fire a third time"
        );
    }

    #[test]
    fn should_reject_an_error_name_resend_does_not_have() {
        let faults = Faults::new();
        let mut rule = rate_limit(None);
        rule.error.name = "everything_is_broken".to_owned();

        assert!(matches!(
            faults.add_rule(&key("re_should_reject_an_invented_error"), rule),
            Err(Error::Validation { .. })
        ));
    }

    #[test]
    fn should_keep_one_keys_faults_away_from_another() {
        let faults = Faults::new();
        let faulted = key("re_should_keep_faults_separate_faulted");
        let healthy = key("re_should_keep_faults_separate_healthy");

        faults.add_rule(&faulted, rate_limit(None)).expect("added");

        assert!(
            faults
                .take(&healthy, FaultTarget::Send)
                .expect("takes")
                .is_none(),
            "a fault under one key must not reach a test under another"
        );
    }

    #[test]
    fn should_forget_only_the_key_it_was_asked_to_reset() {
        let faults = Faults::new();
        let reset = key("re_should_forget_only_the_reset_key_faults");
        let kept = key("re_should_forget_only_the_kept_key_faults");

        faults.add_rule(&reset, rate_limit(None)).expect("added");
        faults.add_rule(&kept, rate_limit(None)).expect("added");

        faults.reset(Some(&reset)).expect("resets");

        assert!(
            faults
                .take(&reset, FaultTarget::Send)
                .expect("takes")
                .is_none()
        );
        assert!(
            faults
                .take(&kept, FaultTarget::Send)
                .expect("takes")
                .is_some()
        );
    }

    #[test]
    fn should_accept_a_delivery_outcome_a_sent_message_can_reach() {
        let faults = Faults::new();
        let key = key("re_should_accept_a_reachable_delivery_outcome");

        for outcome in [
            EmailEvent::Bounced,
            EmailEvent::Complained,
            EmailEvent::DeliveryDelayed,
            EmailEvent::Failed,
        ] {
            assert!(
                faults
                    .set_delivery(&key, Some(DeliveryRule { outcome }))
                    .is_ok(),
                "{outcome} should be reachable from sent"
            );
        }
    }

    #[test]
    fn should_reject_a_delivery_outcome_a_sent_message_cannot_reach() {
        let faults = Faults::new();
        let key = key("re_should_reject_an_unreachable_delivery_outcome");

        assert!(matches!(
            faults.set_delivery(
                &key,
                Some(DeliveryRule {
                    outcome: EmailEvent::Scheduled
                })
            ),
            Err(Error::Validation { .. })
        ));
    }

    #[test]
    fn should_report_a_configured_delivery_rule_back() {
        let faults = Faults::new();
        let key = key("re_should_report_a_delivery_rule_back");

        faults
            .set_delivery(
                &key,
                Some(DeliveryRule {
                    outcome: EmailEvent::Bounced,
                }),
            )
            .expect("set");

        assert_eq!(
            faults.for_key(&key).expect("read").delivery,
            Some(DeliveryRule {
                outcome: EmailEvent::Bounced
            })
        );
    }

    #[test]
    fn should_reject_a_negative_delivery_delay() {
        let faults = Faults::new();

        assert!(matches!(
            faults.set_delivery_delay(
                &key("re_should_reject_a_negative_delay"),
                Some(Duration::seconds(-1))
            ),
            Err(Error::Validation { .. })
        ));
    }
}

#[cfg(test)]
mod rate_limiter {
    use chrono::{DateTime, Duration, Utc};

    use super::RateLimiter;
    use crate::model::ApiKey;

    fn key(name: &str) -> ApiKey {
        ApiKey::parse(name).expect("the test key is re_-prefixed")
    }

    fn now() -> DateTime<Utc> {
        DateTime::UNIX_EPOCH
    }

    #[test]
    fn should_report_no_allowance_when_no_limit_is_configured() {
        let limiter = RateLimiter::new(None);

        assert!(!limiter.is_enabled());
        assert_eq!(
            limiter
                .check(&key("re_should_report_no_allowance"), now())
                .expect("checks"),
            None
        );
    }

    #[test]
    fn should_allow_requests_up_to_the_limit() {
        let limiter = RateLimiter::new(Some(3));
        let key = key("re_should_allow_up_to_the_limit");

        for _attempt in 0..3 {
            let allowance = limiter
                .check(&key, now())
                .expect("checks")
                .expect("a limit is configured");

            assert!(allowance.allowed);
        }
    }

    #[test]
    fn should_refuse_the_request_after_the_limit() {
        let limiter = RateLimiter::new(Some(2));
        let key = key("re_should_refuse_after_the_limit");

        for _attempt in 0..2 {
            let _allowed = limiter.check(&key, now());
        }

        let allowance = limiter
            .check(&key, now())
            .expect("checks")
            .expect("configured");

        assert!(!allowance.allowed);
        assert_eq!(allowance.remaining, 0);
    }

    #[test]
    fn should_count_down_the_remaining_allowance() {
        let limiter = RateLimiter::new(Some(3));
        let key = key("re_should_count_down_the_allowance");

        let remaining: Vec<u32> = (0..3)
            .map(|_attempt| {
                limiter
                    .check(&key, now())
                    .expect("checks")
                    .expect("configured")
                    .remaining
            })
            .collect();

        assert_eq!(remaining, [2, 1, 0]);
    }

    #[test]
    fn should_start_a_fresh_window_after_a_second() {
        let limiter = RateLimiter::new(Some(1));
        let key = key("re_should_start_a_fresh_window");

        let _first = limiter.check(&key, now());
        assert!(
            !limiter
                .check(&key, now())
                .expect("checks")
                .expect("configured")
                .allowed
        );

        let later = now() + Duration::seconds(1);

        assert!(
            limiter
                .check(&key, later)
                .expect("checks")
                .expect("configured")
                .allowed
        );
    }

    #[test]
    fn should_never_report_a_reset_of_zero() {
        let limiter = RateLimiter::new(Some(1));
        let key = key("re_should_never_report_a_reset_of_zero");

        // A reset of zero tells a client to retry immediately, straight into the same
        // refusal.
        for offset in [0, 1, 500, 999] {
            let allowance = limiter
                .check(&key, now() + Duration::milliseconds(offset))
                .expect("checks")
                .expect("configured");

            assert!(
                allowance.reset >= 1,
                "reset was {} at {offset}ms",
                allowance.reset
            );
        }
    }

    #[test]
    fn should_limit_each_key_separately() {
        let limiter = RateLimiter::new(Some(1));
        let busy = key("re_should_limit_each_key_separately_busy");
        let idle = key("re_should_limit_each_key_separately_idle");

        let _first = limiter.check(&busy, now());
        let _second = limiter.check(&busy, now());

        assert!(
            limiter
                .check(&idle, now())
                .expect("checks")
                .expect("configured")
                .allowed,
            "one key exhausting its allowance must not shut out another"
        );
    }

    #[test]
    fn should_refuse_everything_at_a_limit_of_zero() {
        let limiter = RateLimiter::new(Some(0));

        let allowance = limiter
            .check(&key("re_should_refuse_everything_at_zero"), now())
            .expect("checks")
            .expect("configured");

        assert!(!allowance.allowed);
        assert_eq!(allowance.limit, 0);
    }

    #[test]
    fn should_forget_a_window_when_the_key_is_reset() {
        let limiter = RateLimiter::new(Some(1));
        let key = key("re_should_forget_a_window_on_reset");

        let _first = limiter.check(&key, now());
        limiter.reset(Some(&key)).expect("resets");

        assert!(
            limiter
                .check(&key, now())
                .expect("checks")
                .expect("configured")
                .allowed
        );
    }
}

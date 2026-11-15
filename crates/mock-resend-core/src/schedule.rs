//! The clock, and later `scheduled_at` and cancellation.
//!
//! The clock is a dependency rather than [`Utc::now`] called inline, because advancing
//! it is a feature: a test for "this goes out in 24 hours" has to take a millisecond,
//! and a test that sleeps is a test that is flaky on a loaded CI box.

use std::sync::atomic::{AtomicI64, Ordering};

use chrono::{DateTime, Duration, TimeZone, Utc};

/// A source of the current time.
///
/// Every timestamp the engine reports comes from here. Nothing calls [`Utc::now`]
/// directly outside [`SystemClock`].
pub trait Clock: Send + Sync + 'static {
    /// The current instant, as the engine understands it.
    fn now(&self) -> DateTime<Utc>;

    /// Moves the clock forward by `by`, reporting whether it could.
    ///
    /// A clock that cannot be moved returns `false` rather than failing, so the admin
    /// API can report "this instance's clock is fixed" instead of pretending the
    /// request worked.
    fn advance(&self, by: Duration) -> bool {
        let _unused = by;
        false
    }
}

/// The wall clock, with an offset the admin API can move.
///
/// What the server runs with. Time passes on its own, so a delivery delay elapses
/// without anyone asking, and the offset means a test against a running container can
/// still jump a day forward instead of waiting one.
#[derive(Debug, Default)]
pub struct SystemClock {
    /// Milliseconds added to the wall clock. Only ever moved forward.
    offset_millis: AtomicI64,
}

impl SystemClock {
    /// A clock reading the wall clock exactly.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        let offset = self.offset_millis.load(Ordering::SeqCst);

        Utc::now() + Duration::milliseconds(offset)
    }

    fn advance(&self, by: Duration) -> bool {
        // Backwards is refused rather than clamped: a clock that silently ignored a
        // negative jump would make a test that expected one quietly meaningless.
        if by < Duration::zero() {
            return false;
        }

        let _previous =
            self.offset_millis
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                    Some(current.saturating_add(by.num_milliseconds()))
                });

        true
    }
}

/// A clock that only moves when a test moves it.
///
/// Starts at a fixed instant so that two runs of the same test see the same timestamps.
#[derive(Debug)]
pub struct TestClock {
    /// Milliseconds since the Unix epoch.
    ///
    /// An integer rather than a `DateTime` so that reading the time needs no lock at
    /// all: a clock that blocks is a clock that changes the timing it is measuring.
    millis: AtomicI64,
}

impl TestClock {
    /// 2026-01-01T00:00:00Z, in milliseconds since the Unix epoch.
    const EPOCH_MILLIS: i64 = 1_767_225_600_000;

    /// A clock stopped at a fixed, arbitrary instant.
    #[must_use]
    pub fn new() -> Self {
        Self {
            millis: AtomicI64::new(Self::EPOCH_MILLIS),
        }
    }

    /// A clock stopped at `instant`.
    #[must_use]
    pub fn at(instant: DateTime<Utc>) -> Self {
        Self {
            millis: AtomicI64::new(instant.timestamp_millis()),
        }
    }

    /// Moves the clock forward by `millis`, saturating rather than wrapping.
    pub fn advance_millis(&self, millis: i64) {
        let _previous = self
            .millis
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                Some(current.saturating_add(millis))
            });
    }

    /// Moves the clock forward by `seconds`.
    pub fn advance_secs(&self, seconds: i64) {
        self.advance_millis(seconds.saturating_mul(1_000));
    }

    /// Places the clock at `instant`, forward or back.
    pub fn set(&self, instant: DateTime<Utc>) {
        self.millis
            .store(instant.timestamp_millis(), Ordering::SeqCst);
    }
}

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for TestClock {
    fn advance(&self, by: Duration) -> bool {
        if by < Duration::zero() {
            return false;
        }

        self.advance_millis(by.num_milliseconds());

        true
    }

    fn now(&self) -> DateTime<Utc> {
        let millis = self.millis.load(Ordering::SeqCst);

        // `timestamp_millis_opt` is only ambiguous or out of range for values far
        // outside what any test will set, and falling back to the epoch there keeps the
        // trait infallible.
        Utc.timestamp_millis_opt(millis)
            .single()
            .unwrap_or(DateTime::UNIX_EPOCH)
    }
}

#[cfg(test)]
mod test_clock {
    use super::{Clock, TestClock};

    #[test]
    fn should_not_move_on_its_own() {
        let clock = TestClock::new();
        let first = clock.now();
        let second = clock.now();

        assert_eq!(first, second);
    }

    #[test]
    fn should_start_at_the_same_instant_every_run() {
        assert_eq!(TestClock::new().now(), TestClock::new().now());
    }

    #[test]
    fn should_move_forward_when_advanced() {
        let clock = TestClock::new();
        let before = clock.now();

        clock.advance_secs(86_400);

        assert_eq!((clock.now() - before).num_hours(), 24);
    }

    #[test]
    fn should_accumulate_successive_advances() {
        let clock = TestClock::new();
        let before = clock.now();

        clock.advance_millis(250);
        clock.advance_millis(750);

        assert_eq!((clock.now() - before).num_milliseconds(), 1_000);
    }

    #[test]
    fn should_move_back_when_set_to_an_earlier_instant() {
        let clock = TestClock::new();
        let start = clock.now();

        clock.advance_secs(60);
        clock.set(start);

        assert_eq!(clock.now(), start);
    }
}

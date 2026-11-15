//! Spawns an in-process mock-resend from a Rust test.
//!
//! Rust tests use this instead of shelling out to a container. The instance binds port
//! zero and reports what the operating system gave it, because tests run in parallel and
//! a fixed port makes them flaky.

#![forbid(unsafe_code)]

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use mock_resend_core::{Clock, Engine, MemoryStore, TestClock, Transition};
use mock_resend_webhooks::{Config as WebhookConfig, Policy, Sender};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// A running instance, on a port of the operating system's choosing.
///
/// The server stops when this is dropped, so a test holds it for as long as it needs
/// the instance.
#[derive(Debug)]
pub struct MockResend {
    address: SocketAddr,
    engine: Engine,
    clock: Arc<TestClock>,
    server: JoinHandle<()>,
    webhooks: JoinHandle<()>,
}

/// What to turn on for an instance.
///
/// Everything here is off by default, which is the point: a suite that trips a rate
/// limit or a delivery it did not ask for is a flaky suite, so each is opt-in.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Requests allowed per second per key, or `None` for no limit.
    pub rate_limit: Option<u32>,
    /// How long after being sent a message is delivered, or `None` to leave it at
    /// `sent` until the test advances it.
    pub delivery_delay: Option<Duration>,
    /// What the webhook sender is allowed to reach.
    pub webhook_policy: Policy,
    /// How many times a failing webhook delivery is retried.
    pub webhook_retries: u32,
}

impl Options {
    /// Options with nothing turned on.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Limits every key to `limit` requests a second.
    #[must_use]
    pub fn rate_limit(mut self, limit: u32) -> Self {
        self.rate_limit = Some(limit);
        self
    }

    /// Delivers a message `delay` after it is sent.
    #[must_use]
    pub fn delivery_delay(mut self, delay: Duration) -> Self {
        self.delivery_delay = Some(delay);
        self
    }

    /// Lets the webhook sender reach public addresses too.
    #[must_use]
    pub fn allow_public_webhooks(mut self) -> Self {
        self.webhook_policy = Policy::AllowPublic;
        self
    }

    /// Retries a failing webhook delivery this many times.
    #[must_use]
    pub fn webhook_retries(mut self, retries: u32) -> Self {
        self.webhook_retries = retries;
        self
    }
}

impl MockResend {
    /// Starts an instance with the in-memory store and a clock the test controls.
    ///
    /// # Errors
    ///
    /// Any failure binding the listener.
    pub async fn start() -> io::Result<Self> {
        Self::start_with(Options::new()).await
    }

    /// Starts an instance with rate limiting or automatic delivery turned on.
    ///
    /// # Errors
    ///
    /// Any failure binding the listener.
    pub async fn start_with(options: Options) -> io::Result<Self> {
        let clock = Arc::new(TestClock::new());
        let shared: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;

        let (transitions_tx, transitions_rx) = mpsc::unbounded_channel();

        let engine = Engine::new(Arc::new(MemoryStore::new()), shared)
            .with_rate_limit(options.rate_limit)
            .with_delivery_delay(options.delivery_delay)
            .with_transition_sink(transitions_tx);

        // No backoff at all: a test that waits on a retry delay is a test that is slow
        // for no reason, and the retry itself is what is being checked.
        let sender = Sender::new(WebhookConfig {
            policy: options.webhook_policy,
            retries: options.webhook_retries,
            backoff: StdDuration::ZERO,
            ..WebhookConfig::default()
        })
        .map_err(|error| io::Error::other(error.to_string()))?;

        let webhooks = tokio::spawn(sender.run(engine.clone(), transitions_rx));

        // Port zero: the operating system picks one that is free, so two tests running
        // at the same time cannot collide.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;

        let router = mock_resend_api::router(engine.clone());
        let server = tokio::spawn(async move {
            // A serve error here means the test's listener went away, which is what
            // happens on drop and is not worth reporting.
            let _served = axum::serve(listener, router).await;
        });

        Ok(Self {
            address,
            engine,
            clock,
            server,
            webhooks,
        })
    }

    /// The base URL to configure an SDK with, without a trailing slash.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// The address the instance is listening on.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The engine behind the instance, for asserting on captured mail directly.
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The instance's clock.
    ///
    /// Advance this instead of sleeping: a scheduling test that waits on the wall clock
    /// is a test that fails on a loaded CI box.
    #[must_use]
    pub fn clock(&self) -> &TestClock {
        &self.clock
    }

    /// The instance's current time.
    ///
    /// Offered here so a test can build a `scheduled_at` without importing the [`Clock`]
    /// trait.
    #[must_use]
    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// Moves the instance's clock forward and lets the engine catch up.
    ///
    /// This is the replacement for sleeping: a scheduled send due in 24 hours goes out
    /// the moment a test advances past it, with no wall-clock wait and no flakiness on a
    /// loaded CI box.
    ///
    /// # Errors
    ///
    /// [`mock_resend_core::Error::Store`] if the backend fails while sending what is
    /// now due.
    pub async fn advance_secs(&self, seconds: i64) -> mock_resend_core::Result<Vec<Transition>> {
        self.clock.advance_secs(seconds);
        self.engine.tick().await
    }

    /// Lets the engine send anything already due, without moving the clock.
    ///
    /// # Errors
    ///
    /// [`mock_resend_core::Error::Store`] if the backend fails.
    pub async fn tick(&self) -> mock_resend_core::Result<Vec<Transition>> {
        self.engine.tick().await
    }
}

impl Drop for MockResend {
    fn drop(&mut self) {
        self.server.abort();
        self.webhooks.abort();
    }
}

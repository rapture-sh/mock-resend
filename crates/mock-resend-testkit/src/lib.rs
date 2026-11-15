//! Spawns an in-process mock-resend from a Rust test.
//!
//! Rust tests use this instead of shelling out to a container. The instance binds port
//! zero and reports what the operating system gave it, because tests run in parallel and
//! a fixed port makes them flaky.

#![forbid(unsafe_code)]

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use mock_resend_core::{Clock, Engine, MemoryStore, TestClock, Transition};
use tokio::net::TcpListener;
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
}

impl MockResend {
    /// Starts an instance with the in-memory store and a clock the test controls.
    ///
    /// # Errors
    ///
    /// Any failure binding the listener.
    pub async fn start() -> io::Result<Self> {
        let clock = Arc::new(TestClock::new());
        let shared: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
        let engine = Engine::new(Arc::new(MemoryStore::new()), shared);

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
    }
}

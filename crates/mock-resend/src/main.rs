//! Wiring only: parse arguments, build the engine, start the listener.

#![forbid(unsafe_code)]

mod cli;

use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser as _;
use mock_resend_core::Engine;
use mock_resend_webhooks::{Config as WebhookConfig, Policy, Sender};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = cli::Cli::parse();

    // `MOCK_RESEND_LOG` sets the level, and `RUST_LOG` overrides it with a full filter
    // directive for anyone who wants per-module control.
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&cli.log))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt().with_env_filter(filter).init();

    match serve(&cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Binds the API listener and serves until the process is asked to stop.
async fn serve(cli: &cli::Cli) -> Result<(), Box<dyn std::error::Error>> {
    let delivery_delay = cli
        .delivery_delay
        .map(|millis| chrono::Duration::milliseconds(i64::from(millis)));

    let (transitions_tx, transitions_rx) = mpsc::unbounded_channel();

    let engine = Engine::in_memory()
        .with_rate_limit(cli.rate_limit)
        .with_delivery_delay(delivery_delay)
        .with_transition_sink(transitions_tx);

    let policy = if cli.allow_public_webhooks {
        Policy::AllowPublic
    } else {
        Policy::LocalOnly
    };

    let sender = Sender::new(WebhookConfig {
        policy,
        ..WebhookConfig::default()
    })
    .map_err(|error| format!("could not build the webhook client: {error}"))?;

    let webhooks = tokio::spawn(sender.run(engine.clone(), transitions_rx));

    let address = SocketAddr::new(cli.host, cli.port);

    let listener = TcpListener::bind(address)
        .await
        .map_err(|error| format!("could not bind {address}: {error}"))?;

    let bound = listener.local_addr().unwrap_or(address);

    // The engine does not run timers of its own: a scheduled send goes out when
    // something ticks it. The server ticks on an interval; a test moves the clock and
    // ticks directly, which is why neither has to sleep.
    let ticker = tokio::spawn(tick_loop(engine.clone()));

    tracing::info!("mock-resend listening on http://{bound}");
    tracing::info!("point a Resend client at it with RESEND_BASE_URL=http://{bound}");
    tracing::warn!(
        "captured mail is held in memory and goes away with this process; it is never delivered"
    );

    if let Some(limit) = cli.rate_limit {
        tracing::info!("rate limiting at {limit} requests per second per API key");
    }
    if let Some(delay) = cli.delivery_delay {
        tracing::info!("delivering captured mail {delay}ms after it is sent");
    }
    if cli.allow_public_webhooks {
        tracing::warn!("webhooks may reach public addresses; captured mail can leave this machine");
    }

    axum::serve(listener, mock_resend_api::router(engine))
        .with_graceful_shutdown(shutdown())
        .await?;

    ticker.abort();
    webhooks.abort();

    Ok(())
}

/// How often the server looks for a scheduled send that has come due.
///
/// A second is well inside the precision anyone schedules at, and the scan is a map
/// walk over captured mail on a developer machine.
const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Sends scheduled mail as its time arrives.
async fn tick_loop(engine: Engine) {
    let mut interval = tokio::time::interval(TICK_INTERVAL);

    loop {
        let _tick = interval.tick().await;

        match engine.tick().await {
            Ok(transitions) => {
                for transition in transitions {
                    tracing::info!(
                        email_id = %transition.id,
                        event = %transition.to,
                        "scheduled message went out"
                    );
                }
            }
            // A tick failure is a storage problem, not a reason to stop ticking: the
            // next one may well succeed, and a silent dead ticker is worse.
            Err(error) => tracing::error!("could not send scheduled mail: {error}"),
        }
    }
}

/// Resolves on Ctrl-C, and on `SIGTERM` where there is one.
///
/// A container stop sends `SIGTERM`, and a server that ignores it takes the full
/// ten-second grace period to die every time a compose stack comes down.
async fn shutdown() {
    let interrupt = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!("could not listen for Ctrl-C: {error}");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                let _received = signal.recv().await;
            }
            Err(error) => tracing::error!("could not listen for SIGTERM: {error}"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }

    tracing::info!("shutting down");
}

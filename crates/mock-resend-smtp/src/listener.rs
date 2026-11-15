//! Accepting connections.

use mock_resend_core::Engine;
use tokio::io::BufReader;
use tokio::net::TcpListener;

use crate::session::{self, Config};

/// Serves SMTP on `listener` until the future is dropped.
///
/// Each connection gets its own task, and a task that fails takes only its own
/// connection down: a peer that misbehaves must not stop the listener.
pub async fn serve(engine: Engine, listener: TcpListener, config: Config) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::error!("could not accept an SMTP connection: {error}");
                continue;
            }
        };

        let engine = engine.clone();

        let _task = tokio::spawn(async move {
            // The peer's address, not the mail. A connection log is not the place for a
            // recipient or a subject.
            tracing::debug!(peer = %peer, "SMTP connection opened");

            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);

            if let Err(error) = session::run(engine, config, &mut read, &mut write).await {
                tracing::debug!(peer = %peer, "SMTP connection ended: {error}");
            }
        });
    }
}

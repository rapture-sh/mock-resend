//! Outbound delivery, with retries.
//!
//! **This is the only place in the workspace that makes an outbound request.** Rule 5
//! is enforceable because there is exactly one function to audit, and it goes through
//! [`crate::guard`] first. Do not add another client anywhere.

use std::time::Duration;

use mock_resend_core::{Engine, Transition, WebhookEndpoint};
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

use crate::guard::{self, Policy};
use crate::payload;
use crate::signature;

/// How delivery behaves.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// What the guard will reach.
    pub policy: Policy,
    /// How long a single attempt may take.
    pub timeout: Duration,
    /// How many times a failing delivery is retried after the first attempt.
    pub retries: u32,
    /// How long to wait before the first retry. Each retry then doubles it.
    pub backoff: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            policy: Policy::default(),
            // Short, because the handler is on the same machine. A test waiting on a
            // webhook should not be waiting on a network timeout.
            timeout: Duration::from_secs(5),
            retries: 2,
            backoff: Duration::from_millis(100),
        }
    }
}

/// A webhook sender.
#[derive(Debug, Clone)]
pub struct Sender {
    client: reqwest::Client,
    config: Config,
}

impl Sender {
    /// Builds a sender.
    ///
    /// # Errors
    ///
    /// Any failure building the underlying client.
    pub fn new(config: Config) -> reqwest::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            // Redirects are not followed. A 302 is a second URL the caller did not
            // choose and the guard never saw, which is the classic way around an SSRF
            // check.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("mock-resend/", env!("CARGO_PKG_VERSION")))
            .build()?;

        Ok(Self { client, config })
    }

    /// Consumes transitions until the engine goes away, delivering each one.
    ///
    /// Run this as a task. It is the half of the project that makes the other half
    /// worth having: the code that reacts to email is usually where the bugs are, and
    /// it cannot be exercised without something posting to it.
    pub async fn run(self, engine: Engine, mut transitions: UnboundedReceiver<Transition>) {
        while let Some(transition) = transitions.recv().await {
            self.handle(&engine, &transition).await;
        }
    }

    /// Delivers one transition to every endpoint subscribed to it.
    async fn handle(&self, engine: &Engine, transition: &Transition) {
        let endpoints = match engine
            .webhooks()
            .subscribed_to(&transition.key, transition.to)
        {
            Ok(endpoints) => endpoints,
            Err(error) => {
                tracing::error!("could not read webhook endpoints: {error}");
                return;
            }
        };

        if endpoints.is_empty() {
            return;
        }

        let message = match engine.get_email(&transition.key, &transition.id).await {
            Ok(message) => message,
            // The message was reset out from under the transition, which a suite
            // resetting between tests does routinely. Nothing to report.
            Err(_) => return,
        };

        let Some(payload) = payload::build(&message, transition.to, transition.at) else {
            return;
        };

        let body = match serde_json::to_vec(&payload) {
            Ok(body) => body,
            Err(error) => {
                tracing::error!("could not build a webhook payload: {error}");
                return;
            }
        };

        for endpoint in endpoints {
            self.deliver(&endpoint, &body, transition).await;
        }
    }

    /// Posts one payload to one endpoint, retrying a failure.
    async fn deliver(&self, endpoint: &WebhookEndpoint, body: &[u8], transition: &Transition) {
        let url = match guard::check(&endpoint.url, self.config.policy).await {
            Ok(url) => url,
            Err(refusal) => {
                // Loud, because a refused endpoint looks exactly like a webhook that
                // silently never arrived, and that is an hour of someone's afternoon.
                tracing::warn!(
                    webhook_id = %endpoint.id,
                    "refusing to deliver a webhook: {refusal}"
                );
                return;
            }
        };

        // One Svix message id for the whole delivery, reused across retries so a
        // receiver can deduplicate rather than acting twice on the same event.
        let message_id = format!("msg_{}", Uuid::new_v4().as_simple());

        // The wall clock, deliberately, *not* `transition.at`. The signed timestamp is
        // replay protection on the wire, and a real `svix` verifier rejects anything
        // outside a five-minute window before it checks the signature. Moving the
        // engine's clock a day forward is the headline feature, so signing with it would
        // make every webhook after an `/_admin/clock` call unverifiable. The event's own
        // time is in the payload, where it belongs.
        let timestamp = chrono::Utc::now().timestamp();

        let headers = match signature::sign(&endpoint.secret, &message_id, timestamp, body) {
            Ok(headers) => headers,
            Err(error) => {
                tracing::error!("could not sign a webhook: {error}");
                return;
            }
        };

        let mut backoff = self.config.backoff;

        for attempt in 0..=self.config.retries {
            if attempt > 0 {
                // Sleeping in a background task, not in a handler. The backoff is
                // configurable so a test can set it to nothing.
                tokio::time::sleep(backoff).await;
                backoff = backoff.saturating_mul(2);
            }

            let response = self
                .client
                .post(url.clone())
                .header("content-type", "application/json")
                .header(signature::ID_HEADER, &headers.id)
                .header(signature::TIMESTAMP_HEADER, &headers.timestamp)
                .header(signature::SIGNATURE_HEADER, &headers.signature)
                .body(body.to_vec())
                .send()
                .await;

            match response {
                Ok(response) if response.status().is_success() => {
                    tracing::debug!(
                        webhook_id = %endpoint.id,
                        email_id = %transition.id,
                        event = %transition.to,
                        "delivered a webhook"
                    );
                    return;
                }
                Ok(response) => {
                    tracing::debug!(
                        webhook_id = %endpoint.id,
                        status = response.status().as_u16(),
                        attempt = attempt + 1,
                        "a webhook handler refused a delivery"
                    );
                }
                Err(error) => {
                    tracing::debug!(
                        webhook_id = %endpoint.id,
                        attempt = attempt + 1,
                        "could not reach a webhook handler: {error}"
                    );
                }
            }
        }

        tracing::warn!(
            webhook_id = %endpoint.id,
            email_id = %transition.id,
            event = %transition.to,
            attempts = self.config.retries + 1,
            "gave up delivering a webhook"
        );
    }
}

#[cfg(test)]
mod config {
    use super::{Config, Policy};

    #[test]
    fn should_default_to_local_endpoints_only() {
        assert_eq!(Config::default().policy, Policy::LocalOnly);
    }

    #[test]
    fn should_default_to_retrying_a_failure() {
        assert!(Config::default().retries > 0);
    }
}

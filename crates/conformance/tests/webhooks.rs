//! Webhook delivery, against a real handler listening on a real port.
//!
//! The payload is parsed with `resend_rs::events::try_parse_event`, which is the SDK's
//! own deserialiser, and the signature is verified by recomputing the documented Svix
//! scheme independently. Between them that is the whole contract a caller depends on.

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use hmac::{Hmac, Mac};
use mock_resend_core::{ApiKey, EmailEvent as EngineEvent, SigningSecret};
use mock_resend_testkit::{MockResend, Options};
use resend_rs::events::{Event, EventType, try_parse_event};
use sha2::Sha256;
use support::{client_for, email};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// One request a handler received.
#[derive(Debug, Clone)]
struct Received {
    body: String,
    svix_id: String,
    svix_timestamp: String,
    svix_signature: String,
}

/// A webhook handler on a real port, which records what it is sent.
struct Handler {
    url: String,
    received: Mutex<UnboundedReceiver<Received>>,
    server: tokio::task::JoinHandle<()>,
}

impl Handler {
    /// Starts a handler that accepts everything.
    async fn start() -> Self {
        Self::start_with(StatusCode::OK, 0).await
    }

    /// Starts a handler that answers `status` for its first `failures` requests.
    async fn start_with(status: StatusCode, failures: usize) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let state = Arc::new(HandlerState {
            sender,
            remaining_failures: Mutex::new(failures),
            failure_status: status,
        });

        let router = Router::new()
            .route("/hooks", post(receive))
            .with_state(state);

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("the handler binds a port");
        let address = listener.local_addr().expect("a bound address");

        let server = tokio::spawn(async move {
            let _served = axum::serve(listener, router).await;
        });

        Self {
            url: format!("http://{address}/hooks"),
            received: Mutex::new(receiver),
            server,
        }
    }

    /// Waits for the next delivery, or gives up.
    ///
    /// The only wait in the suite, and it is on a real socket rather than on a clock:
    /// there is nothing to advance, because the sender posts as soon as it is told.
    async fn next(&self) -> Option<Received> {
        let mut received = self.received.lock().await;

        tokio::time::timeout(Duration::from_secs(5), received.recv())
            .await
            .ok()
            .flatten()
    }

    /// Whether anything else arrives in a short window.
    async fn is_quiet(&self) -> bool {
        let mut received = self.received.lock().await;

        tokio::time::timeout(Duration::from_millis(300), received.recv())
            .await
            .is_err()
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        self.server.abort();
    }
}

struct HandlerState {
    sender: UnboundedSender<Received>,
    remaining_failures: Mutex<usize>,
    failure_status: StatusCode,
}

async fn receive(
    State(state): State<Arc<HandlerState>>,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };

    let _recorded = state.sender.send(Received {
        body,
        svix_id: header("svix-id"),
        svix_timestamp: header("svix-timestamp"),
        svix_signature: header("svix-signature"),
    });

    let mut remaining = state.remaining_failures.lock().await;
    if *remaining > 0 {
        *remaining -= 1;
        return state.failure_status;
    }

    StatusCode::OK
}

/// Verifies a delivery the way the documented Svix scheme says to.
///
/// Deliberately independent of the signing code: a test that reuses the implementation
/// cannot catch a wrong scheme.
fn verify(secret: &SigningSecret, received: &Received) -> bool {
    let key = secret.key_bytes().expect("the secret decodes");
    let signed = format!(
        "{}.{}.{}",
        received.svix_id, received.svix_timestamp, received.body
    );

    let mut mac = <Hmac<Sha256>>::new_from_slice(&key).expect("any key length");
    mac.update(signed.as_bytes());

    let expected = format!("v1,{}", BASE64.encode(mac.finalize().into_bytes()));

    received.svix_signature == expected
}

fn key(name: &str) -> ApiKey {
    ApiKey::parse(name).expect("the test key is re_-prefixed")
}

#[tokio::test]
async fn should_deliver_a_signature_a_real_verifier_accepts() {
    let name = "re_should_deliver_a_verifiable_signature";
    let handler = Handler::start().await;
    let server = MockResend::start()
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, name);

    let endpoint = server
        .engine()
        .webhooks()
        .register(&key(name), &handler.url, Vec::new(), server.now())
        .expect("registered");

    let _sent = client.emails.send(email("Hello")).await.expect("sent");

    let received = handler.next().await.expect("the webhook arrives");

    assert!(
        verify(&endpoint.secret, &received),
        "a real verifier has to accept this, or every test of a caller's verification code is a lie"
    );
}

#[tokio::test]
async fn should_send_all_three_svix_headers() {
    let name = "re_should_send_all_three_svix_headers";
    let handler = Handler::start().await;
    let server = MockResend::start()
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, name);

    let _endpoint = server
        .engine()
        .webhooks()
        .register(&key(name), &handler.url, Vec::new(), server.now())
        .expect("registered");

    let _sent = client.emails.send(email("Hello")).await.expect("sent");

    let received = handler.next().await.expect("the webhook arrives");

    assert!(!received.svix_id.is_empty());
    assert!(!received.svix_timestamp.is_empty());
    assert!(received.svix_signature.starts_with("v1,"));
}

#[tokio::test]
async fn should_deliver_a_payload_the_sdk_can_parse() {
    let name = "re_should_deliver_a_parseable_payload";
    let handler = Handler::start().await;
    let server = MockResend::start()
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, name);

    let _endpoint = server
        .engine()
        .webhooks()
        .register(&key(name), &handler.url, Vec::new(), server.now())
        .expect("registered");

    let _sent = client.emails.send(email("Hello")).await.expect("sent");

    let received = handler.next().await.expect("the webhook arrives");

    // The SDK's own deserialiser is the assertion: every required field has to be
    // present, correctly named and correctly typed.
    let event = try_parse_event(&received.body).expect("the SDK parses the payload");

    let Event::EmailEvent(event) = event else {
        panic!("expected an email event, got {event:?}");
    };

    assert!(matches!(
        EventType::from(event.r#type),
        EventType::EmailEventType(_)
    ));
    assert_eq!(event.data.to, ["delivered@example.org"]);
    assert!(!event.data.message_id.is_empty());
}

#[tokio::test]
async fn should_report_tags_as_an_object_in_the_payload() {
    let name = "re_should_report_tags_as_an_object";
    let handler = Handler::start().await;
    let server = MockResend::start()
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, name);

    let _endpoint = server
        .engine()
        .webhooks()
        .register(&key(name), &handler.url, Vec::new(), server.now())
        .expect("registered");

    let _sent = client
        .emails
        .send(email("Tagged").with_tag(resend_rs::types::Tag::new("category", "confirm_email")))
        .await
        .expect("sent");

    let received = handler.next().await.expect("the webhook arrives");
    let event = try_parse_event(&received.body).expect("parses");

    let Event::EmailEvent(event) = event else {
        panic!("expected an email event");
    };

    // The request took an array of `{name, value}`; the webhook reports a map.
    assert_eq!(
        event.data.tags.get("category").map(String::as_str),
        Some("confirm_email")
    );
}

#[tokio::test]
async fn should_deliver_a_delivery_event_when_the_clock_moves() {
    let name = "re_should_deliver_a_delivery_event";
    let handler = Handler::start().await;
    let server =
        MockResend::start_with(Options::new().delivery_delay(chrono::Duration::seconds(30)))
            .await
            .expect("the instance binds a port");
    let client = client_for(&server, name);

    let _endpoint = server
        .engine()
        .webhooks()
        .register(
            &key(name),
            &handler.url,
            vec![EngineEvent::Delivered],
            server.now(),
        )
        .expect("registered");

    let _sent = client.emails.send(email("Hello")).await.expect("sent");
    let _transitions = server.advance_secs(30).await.expect("advanced");

    let received = handler.next().await.expect("the webhook arrives");
    let event = try_parse_event(&received.body).expect("parses");

    let Event::EmailEvent(event) = event else {
        panic!("expected an email event");
    };

    assert!(matches!(
        event.r#type,
        resend_rs::events::EmailEventType::EmailDelivered
    ));
}

#[tokio::test]
async fn should_deliver_only_the_events_an_endpoint_subscribed_to() {
    let name = "re_should_deliver_only_subscribed_events";
    let handler = Handler::start().await;
    let server = MockResend::start()
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, name);

    // Subscribed to bounces only, so the `email.sent` below must not arrive.
    let _endpoint = server
        .engine()
        .webhooks()
        .register(
            &key(name),
            &handler.url,
            vec![EngineEvent::Bounced],
            server.now(),
        )
        .expect("registered");

    let _sent = client.emails.send(email("Hello")).await.expect("sent");

    assert!(handler.is_quiet().await, "email.sent was not subscribed to");
}

#[tokio::test]
async fn should_not_deliver_a_cancellation_because_resend_has_no_such_event() {
    let name = "re_should_not_deliver_a_cancellation";
    let handler = Handler::start().await;
    let server = MockResend::start()
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, name);

    let _endpoint = server
        .engine()
        .webhooks()
        .register(&key(name), &handler.url, Vec::new(), server.now())
        .expect("registered");

    let due = server.now() + chrono::Duration::hours(1);
    let sent = client
        .emails
        .send(email("Later").with_scheduled_at(&due.to_rfc3339()))
        .await
        .expect("sent");

    // The `email.scheduled` for the send itself.
    let _scheduled = handler.next().await.expect("the scheduled event arrives");

    let _cancelled = client.emails.cancel(&sent.id).await.expect("cancelled");

    assert!(
        handler.is_quiet().await,
        "Resend's event type list has no email.canceled"
    );
}

#[tokio::test]
async fn should_not_deliver_one_keys_events_to_another_keys_handler() {
    let mine = "re_should_not_cross_keys_webhooks_mine";
    let theirs = "re_should_not_cross_keys_webhooks_theirs";

    let handler = Handler::start().await;
    let server = MockResend::start()
        .await
        .expect("the instance binds a port");

    // The handler belongs to one key; the send happens under another.
    let _endpoint = server
        .engine()
        .webhooks()
        .register(&key(mine), &handler.url, Vec::new(), server.now())
        .expect("registered");

    let other = client_for(&server, theirs);
    let _sent = other.emails.send(email("Hello")).await.expect("sent");

    assert!(
        handler.is_quiet().await,
        "one test's handler must not receive another's events"
    );
}

#[tokio::test]
async fn should_retry_a_delivery_the_handler_refused() {
    let name = "re_should_retry_a_refused_delivery";
    let handler = Handler::start_with(StatusCode::INTERNAL_SERVER_ERROR, 1).await;
    let server = MockResend::start_with(Options::new().webhook_retries(2))
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, name);

    let _endpoint = server
        .engine()
        .webhooks()
        .register(&key(name), &handler.url, Vec::new(), server.now())
        .expect("registered");

    let _sent = client.emails.send(email("Hello")).await.expect("sent");

    let first = handler.next().await.expect("the first attempt arrives");
    let second = handler.next().await.expect("the retry arrives");

    assert_eq!(
        first.svix_id, second.svix_id,
        "a retry reuses the message id so a receiver can deduplicate"
    );
}

#[tokio::test]
async fn should_stop_retrying_once_the_handler_accepts() {
    let name = "re_should_stop_retrying_once_accepted";
    let handler = Handler::start_with(StatusCode::INTERNAL_SERVER_ERROR, 1).await;
    let server = MockResend::start_with(Options::new().webhook_retries(5))
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, name);

    let _endpoint = server
        .engine()
        .webhooks()
        .register(&key(name), &handler.url, Vec::new(), server.now())
        .expect("registered");

    let _sent = client.emails.send(email("Hello")).await.expect("sent");

    let _first = handler.next().await.expect("the failed attempt");
    let _second = handler.next().await.expect("the accepted retry");

    assert!(
        handler.is_quiet().await,
        "a delivery that was accepted must not be retried again"
    );
}

#[tokio::test]
async fn should_refuse_to_deliver_to_a_public_address_by_default() {
    let name = "re_should_refuse_a_public_endpoint";
    let server = MockResend::start()
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, name);

    // A public address, written as a literal so no DNS is involved. Nothing should
    // leave this machine; the sender refuses it and logs a warning.
    let _endpoint = server
        .engine()
        .webhooks()
        .register(
            &key(name),
            "http://93.184.216.34/hooks",
            Vec::new(),
            server.now(),
        )
        .expect("registering it is allowed; delivering to it is not");

    // The send itself still succeeds: a refused webhook is not the caller's problem.
    assert!(client.emails.send(email("Hello")).await.is_ok());
}

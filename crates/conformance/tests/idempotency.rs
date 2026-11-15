//! `Idempotency-Key`, driven through the real `resend-rs` SDK.
//!
//! The SDK sets the header itself through `with_idempotency_key`, so these also prove
//! the header name matches what a client actually sends.

mod support;

use mock_resend_core::{ApiKey, FaultRule, FaultTarget, ResendError};
use resend_rs::idempotent::IdempotentTrait as _;
use resend_rs::types::CreateEmailBaseOptions;
use support::{client_for, email, instance};

#[tokio::test]
async fn should_replay_a_send_repeated_under_the_same_key() {
    let (_server, client) = instance("re_should_replay_a_repeated_send").await;

    let request = email("Hello").with_idempotency_key("order-1234");

    let first = client
        .emails
        .send(request.clone())
        .await
        .expect("the first send is captured");
    let second = client
        .emails
        .send(request)
        .await
        .expect("the retry replays rather than capturing again");

    assert_eq!(first.id, second.id);
}

#[tokio::test]
async fn should_capture_only_one_message_for_a_replayed_send() {
    let (server, client) = instance("re_should_capture_one_for_a_replayed_send").await;

    let request = email("Hello").with_idempotency_key("order-1234");

    let _first = client.emails.send(request.clone()).await.expect("captured");
    let _second = client.emails.send(request).await.expect("replayed");

    let key = mock_resend_core::ApiKey::parse("re_should_capture_one_for_a_replayed_send")
        .expect("valid");

    assert_eq!(
        server.engine().list_emails(&key).await.expect("list").len(),
        1
    );
}

#[tokio::test]
async fn should_reject_a_key_reused_with_a_different_payload() {
    let (_server, client) = instance("re_should_reject_a_reused_key_sdk").await;

    let _first = client
        .emails
        .send(email("Hello").with_idempotency_key("order-1234"))
        .await
        .expect("captured");

    let error = client
        .emails
        .send(email("Something else").with_idempotency_key("order-1234"))
        .await
        .expect_err("the payload changed under the same key");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "invalid_idempotent_request");
    assert_eq!(error.status_code, 409);
}

#[tokio::test]
async fn should_reject_an_overlong_idempotency_key() {
    let (_server, client) = instance("re_should_reject_an_overlong_key").await;

    let error = client
        .emails
        .send(email("Hello").with_idempotency_key(&"a".repeat(257)))
        .await
        .expect_err("257 characters is over the limit");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "invalid_idempotency_key");
    assert_eq!(error.status_code, 400);
}

#[tokio::test]
async fn should_capture_twice_without_an_idempotency_key() {
    let (_server, client) = instance("re_should_capture_twice_without_a_key_sdk").await;

    let first = client.emails.send(email("Hello")).await.expect("captured");
    let second = client.emails.send(email("Hello")).await.expect("captured");

    assert_ne!(first.id, second.id);
}

#[tokio::test]
async fn should_keep_idempotency_keys_separate_across_api_keys() {
    let (server, mine) = instance("re_should_keep_keys_separate_mine").await;
    let theirs = client_for(&server, "re_should_keep_keys_separate_theirs");

    let request = email("Hello").with_idempotency_key("order-1234");

    let first = mine.emails.send(request.clone()).await.expect("captured");
    let second = theirs
        .emails
        .send(request)
        .await
        .expect("a different api key is a different namespace");

    assert_ne!(
        first.id, second.id,
        "one test's idempotency key must not replay into another's"
    );
}

#[tokio::test]
async fn should_replay_a_batch_repeated_under_the_same_key() {
    let (_server, client) = instance("re_should_replay_a_repeated_batch").await;

    let batch: Vec<CreateEmailBaseOptions> = vec![email("one"), email("two")];

    let first = client
        .batch
        .send(batch.clone().with_idempotency_key("batch-1234"))
        .await
        .expect("captured");
    let second = client
        .batch
        .send(batch.with_idempotency_key("batch-1234"))
        .await
        .expect("replayed");

    let first: Vec<String> = first.iter().map(|entry| entry.id.to_string()).collect();
    let second: Vec<String> = second.iter().map(|entry| entry.id.to_string()).collect();

    assert_eq!(first, second);
}

#[tokio::test]
async fn should_let_a_retry_proceed_after_the_first_attempt_was_rejected() {
    let (_server, client) = instance("re_should_let_a_retry_proceed_sdk").await;

    let broken = CreateEmailBaseOptions::new(
        "Acme <onboarding@example.com>",
        ["not-an-address"],
        "broken",
    )
    .with_text("It works")
    .with_idempotency_key("order-1234");

    let _rejected = client
        .emails
        .send(broken)
        .await
        .expect_err("an invalid recipient is refused");

    assert!(
        client
            .emails
            .send(email("Fixed").with_idempotency_key("order-1234"))
            .await
            .is_ok(),
        "a rejected request must not burn the key"
    );
}

#[tokio::test]
async fn should_let_a_retry_proceed_after_an_injected_fault_refused_the_send() {
    let name = "re_should_let_a_retry_proceed_past_a_fault";
    let (server, client) = instance(name).await;

    server
        .engine()
        .faults()
        .add_rule(
            &ApiKey::parse(name).expect("the test key is re_-prefixed"),
            FaultRule {
                target: FaultTarget::Send,
                error: ResendError {
                    name: "application_error".to_owned(),
                    status: 500,
                    message: "Injected by a fault rule.".to_owned(),
                },
                remaining: Some(1),
            },
        )
        .expect("the rule is accepted");

    let request = email("Hello").with_idempotency_key("order-1234");

    let _refused = client
        .emails
        .send(request.clone())
        .await
        .expect_err("the fault fires");

    // The case a client's retry logic actually hits: the server was down, so the same
    // key is sent again. A fault that burned the key would answer the retry with
    // `concurrent_idempotent_requests` and nothing could clear it.
    let sent = client
        .emails
        .send(request)
        .await
        .expect("the retry is served");

    let key = ApiKey::parse(name).expect("valid");
    let captured = server.engine().list_emails(&key).await.expect("list");

    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].id.to_string(), sent.id.to_string());
}

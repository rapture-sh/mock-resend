//! `POST /emails/batch`, driven through the real `resend-rs` SDK.

mod support;

use resend_rs::types::{BatchValidation, CreateEmailBaseOptions};
use support::{email, instance};

/// A request real Resend would refuse: the recipient is not an address.
fn invalid() -> CreateEmailBaseOptions {
    CreateEmailBaseOptions::new(
        "Acme <onboarding@example.com>",
        ["not-an-address"],
        "broken",
    )
    .with_text("It works")
}

#[tokio::test]
async fn should_accept_a_batch_from_a_real_sdk() {
    let (_server, client) = instance("re_should_accept_a_batch_from_a_real_sdk").await;

    let sent = client
        .batch
        .send(vec![email("one"), email("two"), email("three")])
        .await
        .expect("the SDK deserialises the batch response");

    assert_eq!(sent.len(), 3);
    assert!(sent.iter().all(|entry| entry.id.as_ref().len() == 36));
}

#[tokio::test]
async fn should_make_every_batched_message_retrievable() {
    let (_server, client) = instance("re_should_make_batched_messages_retrievable").await;

    let sent = client
        .batch
        .send(vec![email("first"), email("second")])
        .await
        .expect("sent");

    for (entry, expected) in sent.iter().zip(["first", "second"]) {
        let retrieved = client.emails.get(&entry.id).await.expect("retrieved");

        assert_eq!(retrieved.subject, expected);
    }
}

#[tokio::test]
async fn should_reject_a_whole_strict_batch_when_one_entry_is_invalid() {
    let (_server, client) = instance("re_should_reject_a_strict_batch_sdk").await;

    let error = client
        .batch
        .send_with_batch_validation(vec![email("fine"), invalid()], BatchValidation::Strict)
        .await
        .expect_err("strict mode rejects the whole batch");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "validation_error");
}

#[tokio::test]
async fn should_capture_nothing_from_a_rejected_strict_batch() {
    let (server, client) = instance("re_should_capture_nothing_from_a_strict_batch").await;

    let _refused = client
        .batch
        .send_with_batch_validation(vec![email("fine"), invalid()], BatchValidation::Strict)
        .await;

    let key = mock_resend_core::ApiKey::parse("re_should_capture_nothing_from_a_strict_batch")
        .expect("valid");

    assert!(
        server
            .engine()
            .list_emails(&key)
            .await
            .expect("list")
            .is_empty(),
        "strict mode is atomic"
    );
}

#[tokio::test]
async fn should_report_per_entry_errors_in_permissive_mode() {
    let (_server, client) = instance("re_should_report_permissive_batch_errors").await;

    let outcome = client
        .batch
        .send_with_batch_validation(
            vec![email("fine"), invalid(), email("also fine")],
            BatchValidation::Permissive,
        )
        .await
        .expect("permissive mode allows partial success");

    assert_eq!(outcome.data.len(), 2);
    assert_eq!(outcome.errors.len(), 1);
    assert_eq!(outcome.errors[0].index, 1);
}

#[tokio::test]
async fn should_accept_a_batch_at_the_hundred_message_limit() {
    let (_server, client) = instance("re_should_accept_a_batch_at_the_limit").await;

    let batch: Vec<_> = (0..100)
        .map(|index| email(&format!("message {index}")))
        .collect();

    assert_eq!(client.batch.send(batch).await.expect("sent").len(), 100);
}

#[tokio::test]
async fn should_reject_a_batch_over_the_hundred_message_limit() {
    let (_server, client) = instance("re_should_reject_a_batch_over_the_limit").await;

    let batch: Vec<_> = (0..101)
        .map(|index| email(&format!("message {index}")))
        .collect();

    let error = client
        .batch
        .send(batch)
        .await
        .expect_err("101 is over Resend's limit");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "validation_error");
}

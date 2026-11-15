//! `/emails`, driven through the real `resend-rs` SDK.
//!
//! Hand-written HTTP proves the handler matches our understanding of Resend. Driving a
//! real SDK proves it matches what users actually send — the string-or-array fields, the
//! headers the client sets, and the exact response shape the client deserialises. A
//! mock that only satisfies its own tests is a mock that agrees with itself.

mod support;

use mock_resend_testkit::MockResend;
use resend_rs::types::{CreateAttachment, CreateEmailBaseOptions, EmailEvent, Tag};
use resend_rs::{Config, Resend};
use support::{client_for, email, instance};

#[tokio::test]
async fn should_accept_a_send_from_a_real_sdk() {
    let (_server, client) = instance("re_should_accept_a_send_from_a_real_sdk").await;

    let sent = client
        .emails
        .send(email("Hello World"))
        .await
        .expect("the SDK accepts the response");

    assert_eq!(sent.id.as_ref().len(), 36);
}

#[tokio::test]
async fn should_return_a_sent_email_in_the_shape_the_sdk_expects() {
    let (_server, client) = instance("re_should_return_a_sent_email_in_sdk_shape").await;

    let sent = client
        .emails
        .send(email("Hello World"))
        .await
        .expect("sent");

    // The SDK's own deserialisation is the assertion: every required field has to be
    // present, correctly named and correctly typed, or this call fails.
    let retrieved = client
        .emails
        .get(&sent.id)
        .await
        .expect("the SDK deserialises the retrieve response");

    assert_eq!(retrieved.id, sent.id);
    assert_eq!(retrieved.from, "Acme <onboarding@example.com>");
    assert_eq!(retrieved.to, ["delivered@example.org"]);
    assert_eq!(retrieved.subject, "Hello World");
    assert_eq!(retrieved.text.as_deref(), Some("It works"));
    assert_eq!(retrieved.last_event, EmailEvent::Sent);
    assert!(retrieved.html.is_none());
}

#[tokio::test]
async fn should_report_empty_recipient_lists_the_sdk_can_parse() {
    let (_server, client) = instance("re_should_report_empty_recipient_lists").await;

    let sent = client.emails.send(email("No cc")).await.expect("sent");

    // `bcc` and `cc` are non-optional `Vec<String>` on the SDK's type, parsed through a
    // nullable helper. Whatever this server reports for "none" has to satisfy that.
    let retrieved = client.emails.get(&sent.id).await.expect("retrieved");

    assert!(retrieved.cc.is_empty());
    assert!(retrieved.bcc.is_empty());
}

#[tokio::test]
async fn should_round_trip_every_recipient_field() {
    let (_server, client) = instance("re_should_round_trip_every_recipient_field").await;

    let request = email("Everyone")
        .with_cc("cc@example.org")
        .with_bcc("bcc@example.org")
        .with_reply("reply@example.org");

    let sent = client.emails.send(request).await.expect("sent");
    let retrieved = client.emails.get(&sent.id).await.expect("retrieved");

    assert_eq!(retrieved.cc, ["cc@example.org"]);
    assert_eq!(retrieved.bcc, ["bcc@example.org"]);
    assert_eq!(
        retrieved.reply_to.as_deref(),
        Some(["reply@example.org".to_owned()].as_slice())
    );
}

#[tokio::test]
async fn should_accept_an_html_body() {
    let (_server, client) = instance("re_should_accept_an_html_body").await;

    let request = CreateEmailBaseOptions::new(
        "onboarding@example.com",
        ["delivered@example.org"],
        "HTML only",
    )
    .with_html("<strong>It works</strong>");

    let sent = client.emails.send(request).await.expect("sent");
    let retrieved = client.emails.get(&sent.id).await.expect("retrieved");

    assert_eq!(retrieved.html.as_deref(), Some("<strong>It works</strong>"));
    assert!(retrieved.text.is_none());
}

#[tokio::test]
async fn should_accept_a_tag_and_custom_headers() {
    let (_server, client) = instance("re_should_accept_a_tag_and_custom_headers").await;

    let request = email("Tagged")
        .with_tag(Tag::new("category", "confirm_email"))
        .with_header("X-Entity-Ref-ID", "abc-123");

    assert!(client.emails.send(request).await.is_ok());
}

#[tokio::test]
async fn should_accept_an_attachment_sent_as_bytes() {
    let (_server, client) = instance("re_should_accept_an_attachment_as_bytes").await;

    // `resend-rs` serialises attachment content as a JSON array of byte values, while
    // the Node SDK sends base64. Both forms have to be accepted.
    let request = email("With a file").with_attachment(
        CreateAttachment::from_content(b"%PDF-1.4 not really".to_vec())
            .with_filename("invoice.pdf"),
    );

    assert!(client.emails.send(request).await.is_ok());
}

#[tokio::test]
async fn should_reject_a_send_with_no_api_key_as_missing_api_key() {
    let server = MockResend::start()
        .await
        .expect("the instance binds a port");

    let base_url = server.base_url().parse().expect("a valid url");
    // An empty credential, which is what a client with no key configured sends.
    let client = Resend::with_config(Config::builder("").base_url(base_url).build());

    let error = client
        .emails
        .send(email("Unauthorised"))
        .await
        .expect_err("an unkeyed send is refused");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "missing_api_key");
    assert_eq!(error.status_code, 401);
}

#[tokio::test]
async fn should_reject_a_retrieve_of_an_unknown_id_as_not_found() {
    let (_server, client) = instance("re_should_reject_an_unknown_id").await;

    let error = client
        .emails
        .get("9b7a0f6e-0000-4000-8000-000000000000")
        .await
        .expect_err("nothing was captured under this key");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "not_found");
    assert_eq!(error.status_code, 404);
}

#[tokio::test]
async fn should_reject_a_malformed_id_as_an_invalid_parameter() {
    let (_server, client) = instance("re_should_reject_a_malformed_id").await;

    let error = client
        .emails
        .get("not-a-uuid")
        .await
        .expect_err("a non-uuid id is refused");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "invalid_parameter");
    assert_eq!(error.status_code, 422);
}

#[tokio::test]
async fn should_not_return_an_email_sent_under_another_key() {
    let (server, first) = instance("re_should_not_leak_across_keys_first").await;
    let second = client_for(&server, "re_should_not_leak_across_keys_second");

    let sent = first.emails.send(email("Private")).await.expect("sent");

    let error = second
        .emails
        .get(&sent.id)
        .await
        .expect_err("the second key cannot see the first key's mail");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(
        error.name, "not_found",
        "one instance has to be safe for a parallel suite"
    );
}

#[tokio::test]
async fn should_hold_a_future_send_as_scheduled() {
    let (server, client) = instance("re_should_hold_a_future_send_as_scheduled").await;

    // The instance's clock, not the wall clock. Nothing here sleeps.
    let due = server.now() + chrono::Duration::hours(1);

    let request = email("Later").with_scheduled_at(&due.to_rfc3339());

    let sent = client.emails.send(request).await.expect("sent");
    let retrieved = client.emails.get(&sent.id).await.expect("retrieved");

    assert_eq!(retrieved.last_event, EmailEvent::Scheduled);
    assert!(retrieved.scheduled_at.is_some());
    assert!(
        retrieved.message_id.is_some(),
        "Resend's documented email.scheduled payload carries a message_id"
    );
}

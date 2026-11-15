//! Scheduling, rescheduling and cancellation, driven through the real `resend-rs` SDK.
//!
//! Nothing in here sleeps. The instance's clock is a dependency, so a send due in a day
//! goes out the moment the test advances past it.

mod support;

use resend_rs::types::{EmailEvent, UpdateEmailOptions};
use support::{email, instance};

#[tokio::test]
async fn should_send_a_scheduled_email_once_the_clock_passes_its_time() {
    let (server, client) = instance("re_should_send_a_scheduled_email_when_due").await;

    let due = server.now() + chrono::Duration::hours(24);
    let sent = client
        .emails
        .send(email("Tomorrow").with_scheduled_at(&due.to_rfc3339()))
        .await
        .expect("sent");

    assert_eq!(
        client
            .emails
            .get(&sent.id)
            .await
            .expect("retrieved")
            .last_event,
        EmailEvent::Scheduled
    );

    let transitions = server
        .advance_secs(24 * 60 * 60)
        .await
        .expect("the clock moves and the engine catches up");

    assert_eq!(transitions.len(), 1);
    assert_eq!(
        client
            .emails
            .get(&sent.id)
            .await
            .expect("retrieved")
            .last_event,
        EmailEvent::Sent
    );
}

#[tokio::test]
async fn should_keep_a_scheduled_email_waiting_until_its_time() {
    let (server, client) = instance("re_should_keep_a_scheduled_email_waiting").await;

    let due = server.now() + chrono::Duration::hours(24);
    let sent = client
        .emails
        .send(email("Tomorrow").with_scheduled_at(&due.to_rfc3339()))
        .await
        .expect("sent");

    let _transitions = server
        .advance_secs(60 * 60)
        .await
        .expect("advanced an hour");

    assert_eq!(
        client
            .emails
            .get(&sent.id)
            .await
            .expect("retrieved")
            .last_event,
        EmailEvent::Scheduled
    );
}

#[tokio::test]
async fn should_reschedule_a_pending_send_through_the_sdk() {
    let (server, client) = instance("re_should_reschedule_a_pending_send").await;

    let due = server.now() + chrono::Duration::hours(1);
    let sent = client
        .emails
        .send(email("Soon").with_scheduled_at(&due.to_rfc3339()))
        .await
        .expect("sent");

    let later = server.now() + chrono::Duration::hours(10);
    let updated = client
        .emails
        .update(
            &sent.id,
            UpdateEmailOptions::new().with_scheduled_at(&later.to_rfc3339()),
        )
        .await
        .expect("the SDK deserialises the update response");

    assert_eq!(updated.id, sent.id);

    // Past the original time, before the new one.
    let transitions = server.advance_secs(2 * 60 * 60).await.expect("advanced");

    assert!(
        transitions.is_empty(),
        "the original time should no longer send it"
    );
    assert_eq!(
        client
            .emails
            .get(&sent.id)
            .await
            .expect("retrieved")
            .last_event,
        EmailEvent::Scheduled
    );
}

#[tokio::test]
async fn should_cancel_a_scheduled_send_through_the_sdk() {
    let (server, client) = instance("re_should_cancel_a_scheduled_send").await;

    let due = server.now() + chrono::Duration::hours(1);
    let sent = client
        .emails
        .send(email("Never mind").with_scheduled_at(&due.to_rfc3339()))
        .await
        .expect("sent");

    let cancelled = client
        .emails
        .cancel(&sent.id)
        .await
        .expect("the SDK deserialises the cancel response");

    assert_eq!(cancelled.id, sent.id);
    assert_eq!(
        client
            .emails
            .get(&sent.id)
            .await
            .expect("retrieved")
            .last_event,
        EmailEvent::Canceled
    );
}

#[tokio::test]
async fn should_not_send_a_cancelled_message_when_its_time_arrives() {
    let (server, client) = instance("re_should_not_send_a_cancelled_message").await;

    let due = server.now() + chrono::Duration::hours(1);
    let sent = client
        .emails
        .send(email("Never mind").with_scheduled_at(&due.to_rfc3339()))
        .await
        .expect("sent");

    let _cancelled = client.emails.cancel(&sent.id).await.expect("cancelled");

    let transitions = server.advance_secs(2 * 60 * 60).await.expect("advanced");

    assert!(transitions.is_empty());
    assert_eq!(
        client
            .emails
            .get(&sent.id)
            .await
            .expect("retrieved")
            .last_event,
        EmailEvent::Canceled
    );
}

#[tokio::test]
async fn should_refuse_to_cancel_an_email_that_already_went_out() {
    let (_server, client) = instance("re_should_refuse_to_cancel_a_sent_email").await;

    let sent = client
        .emails
        .send(email("Already gone"))
        .await
        .expect("sent");

    let error = client
        .emails
        .cancel(&sent.id)
        .await
        .expect_err("a sent email cannot be cancelled");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "validation_error");
}

#[tokio::test]
async fn should_refuse_to_reschedule_an_email_that_already_went_out() {
    let (server, client) = instance("re_should_refuse_to_reschedule_a_sent_email").await;

    let sent = client
        .emails
        .send(email("Already gone"))
        .await
        .expect("sent");

    let later = server.now() + chrono::Duration::hours(1);
    let error = client
        .emails
        .update(
            &sent.id,
            UpdateEmailOptions::new().with_scheduled_at(&later.to_rfc3339()),
        )
        .await
        .expect_err("a sent email cannot be rescheduled");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "validation_error");
}

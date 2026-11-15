//! Fault injection and rate limiting, driven through the real `resend-rs` SDK.
//!
//! These are the tests that prove the point of the project: a suite can make its own
//! error handling run without a broken mail server, and the real SDK has to recognise
//! what comes back.

mod support;

use chrono::Duration;
use mock_resend_core::{
    ApiKey, DeliveryRule, EmailEvent as EngineEvent, FaultRule, FaultTarget, ResendError,
};
use mock_resend_testkit::{MockResend, Options};
use resend_rs::types::EmailEvent;
use support::{client_for, email, instance};

fn key(name: &str) -> ApiKey {
    ApiKey::parse(name).expect("the test key is re_-prefixed")
}

fn fault(name: &str, status: u16, target: FaultTarget, remaining: Option<u32>) -> FaultRule {
    FaultRule {
        target,
        error: ResendError {
            name: name.to_owned(),
            status,
            message: "Injected by a fault rule.".to_owned(),
        },
        remaining,
    }
}

#[tokio::test]
async fn should_make_a_send_fail_with_the_injected_resend_error() {
    let name = "re_should_make_a_send_fail_injected";
    let (server, client) = instance(name).await;

    server
        .engine()
        .faults()
        .add_rule(
            &key(name),
            fault("application_error", 500, FaultTarget::Send, None),
        )
        .expect("the rule is accepted");

    let error = client
        .emails
        .send(email("Hello"))
        .await
        .expect_err("the fault fires");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "application_error");
    assert_eq!(error.status_code, 500);
}

#[tokio::test]
async fn should_turn_an_injected_rate_limit_into_the_sdks_own_rate_limit_error() {
    let name = "re_should_inject_a_rate_limit_error";
    let (server, client) = instance(name).await;

    server
        .engine()
        .faults()
        .add_rule(
            &key(name),
            fault("rate_limit_exceeded", 429, FaultTarget::Send, None),
        )
        .expect("the rule is accepted");

    let error = client
        .emails
        .send(email("Hello"))
        .await
        .expect_err("the fault fires");

    // The SDK lifts a 429 out of the Resend error type into its own variant, which is
    // what a caller's retry logic actually matches on.
    assert!(
        matches!(error, resend_rs::Error::RateLimit { .. }),
        "expected the SDK's own rate limit error, got {error:?}"
    );
}

#[tokio::test]
async fn should_capture_nothing_when_a_send_is_faulted() {
    let name = "re_should_capture_nothing_when_faulted";
    let (server, client) = instance(name).await;

    server
        .engine()
        .faults()
        .add_rule(
            &key(name),
            fault("application_error", 500, FaultTarget::Send, None),
        )
        .expect("the rule is accepted");

    let _refused = client.emails.send(email("Hello")).await;

    assert!(
        server
            .engine()
            .list_emails(&key(name))
            .await
            .expect("list")
            .is_empty()
    );
}

#[tokio::test]
async fn should_fail_once_then_succeed_for_a_counted_rule() {
    let name = "re_should_fail_once_then_succeed";
    let (server, client) = instance(name).await;

    server
        .engine()
        .faults()
        .add_rule(
            &key(name),
            fault("application_error", 500, FaultTarget::Send, Some(1)),
        )
        .expect("the rule is accepted");

    let _first = client
        .emails
        .send(email("Hello"))
        .await
        .expect_err("the first send is faulted");

    assert!(
        client.emails.send(email("Hello")).await.is_ok(),
        "a rule set to fire once must not fire twice"
    );
}

#[tokio::test]
async fn should_fault_a_retrieve_without_faulting_a_send() {
    let name = "re_should_fault_a_retrieve_only";
    let (server, client) = instance(name).await;

    server
        .engine()
        .faults()
        .add_rule(
            &key(name),
            fault("application_error", 500, FaultTarget::Retrieve, None),
        )
        .expect("the rule is accepted");

    let sent = client
        .emails
        .send(email("Hello"))
        .await
        .expect("the send is not faulted");

    let error = client
        .emails
        .get(&sent.id)
        .await
        .expect_err("the retrieve is");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "application_error");
}

#[tokio::test]
async fn should_not_let_one_keys_fault_reach_another_key() {
    let faulted_name = "re_should_not_let_a_fault_cross_keys_faulted";
    let (server, faulted) = instance(faulted_name).await;
    let healthy = client_for(&server, "re_should_not_let_a_fault_cross_keys_healthy");

    server
        .engine()
        .faults()
        .add_rule(
            &key(faulted_name),
            fault("application_error", 500, FaultTarget::Any, None),
        )
        .expect("the rule is accepted");

    assert!(faulted.emails.send(email("Hello")).await.is_err());
    assert!(
        healthy.emails.send(email("Hello")).await.is_ok(),
        "one instance has to be safe for a parallel suite"
    );
}

#[tokio::test]
async fn should_refuse_to_inject_an_error_name_resend_does_not_have() {
    let name = "re_should_refuse_an_invented_error_name";
    let (server, _client) = instance(name).await;

    assert!(
        server
            .engine()
            .faults()
            .add_rule(
                &key(name),
                fault("everything_is_broken", 500, FaultTarget::Send, None),
            )
            .is_err(),
        "injecting an invented name would let a test assert against an error real Resend never sends"
    );
}

#[tokio::test]
async fn should_bounce_a_delivery_when_a_rule_says_so() {
    let name = "re_should_bounce_a_delivery_sdk";
    let server = MockResend::start_with(Options::new().delivery_delay(Duration::seconds(1)))
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, name);

    server
        .engine()
        .faults()
        .set_delivery(
            &key(name),
            Some(DeliveryRule {
                outcome: EngineEvent::Bounced,
            }),
        )
        .expect("bounced is reachable from sent");

    let sent = client.emails.send(email("Hello")).await.expect("sent");

    let _transitions = server.advance_secs(5).await.expect("advanced");

    assert_eq!(
        client
            .emails
            .get(&sent.id)
            .await
            .expect("retrieved")
            .last_event,
        EmailEvent::Bounced
    );
}

#[tokio::test]
async fn should_deliver_a_message_once_the_delay_elapses() {
    let name = "re_should_deliver_after_the_delay_sdk";
    let server = MockResend::start_with(Options::new().delivery_delay(Duration::seconds(30)))
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, name);

    let sent = client.emails.send(email("Hello")).await.expect("sent");

    assert_eq!(
        client
            .emails
            .get(&sent.id)
            .await
            .expect("retrieved")
            .last_event,
        EmailEvent::Sent
    );

    let _transitions = server.advance_secs(30).await.expect("advanced");

    assert_eq!(
        client
            .emails
            .get(&sent.id)
            .await
            .expect("retrieved")
            .last_event,
        EmailEvent::Delivered
    );
}

#[tokio::test]
async fn should_refuse_a_send_past_the_configured_rate_limit() {
    let name = "re_should_refuse_past_the_rate_limit_sdk";
    let server = MockResend::start_with(Options::new().rate_limit(1))
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, name);

    let _first = client
        .emails
        .send(email("Hello"))
        .await
        .expect("the first request is inside the allowance");

    let error = client
        .emails
        .send(email("Hello"))
        .await
        .expect_err("the second is not");

    // The SDK reads `ratelimit-limit`, `ratelimit-remaining` and `ratelimit-reset` off
    // the 429 to build this, so the assertion covers the headers as well as the status.
    let resend_rs::Error::RateLimit {
        ratelimit_limit,
        ratelimit_remaining,
        ratelimit_reset,
    } = error
    else {
        panic!("expected the SDK's own rate limit error, got {error:?}");
    };

    assert_eq!(ratelimit_limit, Some(1));
    assert_eq!(ratelimit_remaining, Some(0));
    assert!(ratelimit_reset.is_some_and(|reset| reset >= 1));
}

#[tokio::test]
async fn should_not_rate_limit_at_all_without_a_configured_limit() {
    let (_server, client) = instance("re_should_not_rate_limit_by_default").await;

    for _attempt in 0..12 {
        assert!(
            client.emails.send(email("Hello")).await.is_ok(),
            "a suite that trips a limit it did not ask for is a flaky suite"
        );
    }
}

#[tokio::test]
async fn should_limit_each_key_separately() {
    let server = MockResend::start_with(Options::new().rate_limit(1))
        .await
        .expect("the instance binds a port");

    let busy = client_for(&server, "re_should_limit_each_key_separately_busy_sdk");
    let idle = client_for(&server, "re_should_limit_each_key_separately_idle_sdk");

    let _first = busy.emails.send(email("Hello")).await.expect("allowed");
    let _refused = busy
        .emails
        .send(email("Hello"))
        .await
        .expect_err("over the allowance");

    assert!(
        idle.emails.send(email("Hello")).await.is_ok(),
        "one key exhausting its allowance must not shut out another"
    );
}

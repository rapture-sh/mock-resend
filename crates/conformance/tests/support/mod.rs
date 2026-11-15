//! Shared setup for the conformance suite.
//!
//! Each test gets its own instance and its own key. The key is the isolation mechanism,
//! so sharing one would let these tests see each other's mail.

// Each integration test binary compiles this module separately and uses a different
// subset of it, so anything one binary does not call is dead code from that binary's
// point of view. Narrower than this would mean an attribute on every item.
#![allow(dead_code)]

use mock_resend_testkit::MockResend;
use resend_rs::types::CreateEmailBaseOptions;
use resend_rs::{Config, Resend};

/// A fresh instance plus a client pointed at it, under a key named after the test.
pub async fn instance(key: &str) -> (MockResend, Resend) {
    let server = MockResend::start()
        .await
        .expect("the instance binds a port");
    let client = client_for(&server, key);

    (server, client)
}

/// Another client on the same instance, under a different key.
///
/// Used by the isolation tests: two keys on one server is the case a parallel suite
/// actually hits.
pub fn client_for(server: &MockResend, key: &str) -> Resend {
    let base_url = server
        .base_url()
        .parse()
        .expect("the testkit reports a valid url");

    Resend::with_config(Config::builder(key).base_url(base_url).build())
}

/// A minimal valid send request.
pub fn email(subject: &str) -> CreateEmailBaseOptions {
    CreateEmailBaseOptions::new(
        "Acme <onboarding@example.com>",
        ["delivered@example.org"],
        subject,
    )
    .with_text("It works")
}

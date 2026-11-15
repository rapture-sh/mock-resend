//! The `/_admin` surface, driven over HTTP.
//!
//! Deliberately not through a Rust helper: the admin API exists so a suite in any
//! language can reset a key, move the clock and inject a fault, so these tests speak to
//! it the way a `curl` or a `fetch` would.

mod support;

use resend_rs::types::EmailEvent;
use support::{client_for, email, instance};

/// A plain HTTP client, with no Resend SDK involved.
fn http() -> reqwest::Client {
    reqwest::Client::new()
}

async fn post(url: &str, body: serde_json::Value) -> (reqwest::StatusCode, serde_json::Value) {
    let response = http()
        .post(url)
        .json(&body)
        .send()
        .await
        .expect("the admin API answers");

    let status = response.status();
    let json = response.json().await.unwrap_or(serde_json::Value::Null);

    (status, json)
}

async fn get(url: &str) -> (reqwest::StatusCode, serde_json::Value) {
    let response = http().get(url).send().await.expect("the admin API answers");

    let status = response.status();
    let json = response.json().await.unwrap_or(serde_json::Value::Null);

    (status, json)
}

#[tokio::test]
async fn should_report_health_with_the_engines_own_time() {
    let (server, _client) = instance("re_should_report_health").await;

    let (status, body) = get(&format!("{}/_admin/health", server.base_url())).await;

    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");
    assert!(body["now"].as_str().is_some_and(|now| now.ends_with('Z')));
}

#[tokio::test]
async fn should_list_captured_mail_with_the_parts_resend_does_not_report() {
    let name = "re_should_list_captured_mail_in_full";
    let (server, client) = instance(name).await;

    let _sent = client
        .emails
        .send(
            email("Hello")
                .with_header("X-Entity-Ref-ID", "abc-123")
                .with_tag(resend_rs::types::Tag::new("category", "confirm_email")),
        )
        .await
        .expect("sent");

    let (status, body) = get(&format!(
        "{}/_admin/emails?api_key={name}",
        server.base_url()
    ))
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["count"], 1);

    let captured = &body["data"][0];

    // Resend's own retrieve response reports none of these three.
    assert_eq!(captured["headers"]["X-Entity-Ref-ID"], "abc-123");
    assert_eq!(captured["tags"][0]["name"], "category");
    assert_eq!(captured["last_event"], "sent");
}

#[tokio::test]
async fn should_report_an_attachment_without_its_bytes() {
    let name = "re_should_report_an_attachment_without_bytes";
    let (server, client) = instance(name).await;

    let _sent = client
        .emails
        .send(
            email("With a file").with_attachment(
                resend_rs::types::CreateAttachment::from_content(b"%PDF-1.4".to_vec())
                    .with_filename("invoice.pdf"),
            ),
        )
        .await
        .expect("sent");

    let (_status, body) = get(&format!(
        "{}/_admin/emails?api_key={name}",
        server.base_url()
    ))
    .await;

    let attachment = &body["data"][0]["attachments"][0];

    assert_eq!(attachment["filename"], "invoice.pdf");
    assert_eq!(attachment["source"], "content");
    assert_eq!(attachment["bytes"], 8);
}

#[tokio::test]
async fn should_report_an_attachment_path_without_fetching_it() {
    let name = "re_should_report_a_path_without_fetching";
    let (server, client) = instance(name).await;

    let _sent = client
        .emails
        .send(
            email("With a remote file").with_attachment(
                resend_rs::types::CreateAttachment::from_path(
                    "http://169.254.169.254/latest/meta-data/",
                )
                .with_filename("nope.txt"),
            ),
        )
        .await
        .expect("sent");

    let (_status, body) = get(&format!(
        "{}/_admin/emails?api_key={name}",
        server.base_url()
    ))
    .await;

    let attachment = &body["data"][0]["attachments"][0];

    assert_eq!(attachment["source"], "path");
    assert_eq!(
        attachment["path"],
        "http://169.254.169.254/latest/meta-data/"
    );
    assert!(
        attachment.get("bytes").is_none(),
        "there are no bytes, because nothing fetched them"
    );
}

#[tokio::test]
async fn should_reset_one_key_and_leave_another_alone() {
    let mine = "re_should_reset_one_key_mine";
    let theirs = "re_should_reset_one_key_theirs";

    let (server, first) = instance(mine).await;
    let second = client_for(&server, theirs);

    let _a = first.emails.send(email("Hello")).await.expect("sent");
    let _b = second.emails.send(email("Hello")).await.expect("sent");

    let (status, body) = post(
        &format!("{}/_admin/reset", server.base_url()),
        serde_json::json!({ "api_key": mine }),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["scope"], mine);

    let (_status, mine_list) = get(&format!(
        "{}/_admin/emails?api_key={mine}",
        server.base_url()
    ))
    .await;
    let (_status, theirs_list) = get(&format!(
        "{}/_admin/emails?api_key={theirs}",
        server.base_url()
    ))
    .await;

    assert_eq!(mine_list["count"], 0);
    assert_eq!(theirs_list["count"], 1);
}

#[tokio::test]
async fn should_reset_every_key_when_no_key_is_named() {
    let name = "re_should_reset_every_key";
    let (server, client) = instance(name).await;

    let _sent = client.emails.send(email("Hello")).await.expect("sent");

    let (status, body) = post(
        &format!("{}/_admin/reset", server.base_url()),
        serde_json::json!({}),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["scope"], "all");

    let (_status, list) = get(&format!(
        "{}/_admin/emails?api_key={name}",
        server.base_url()
    ))
    .await;

    assert_eq!(list["count"], 0);
}

#[tokio::test]
async fn should_advance_a_message_to_another_delivery_event() {
    let name = "re_should_advance_a_message_by_admin";
    let (server, client) = instance(name).await;

    let sent = client.emails.send(email("Hello")).await.expect("sent");

    let (status, body) = post(
        &format!("{}/_admin/emails/{}/events", server.base_url(), sent.id),
        serde_json::json!({ "api_key": name, "event": "delivered" }),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["from"], "sent");
    assert_eq!(body["to"], "delivered");

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
async fn should_refuse_to_advance_a_message_into_an_impossible_state() {
    let name = "re_should_refuse_an_impossible_advance";
    let (server, client) = instance(name).await;

    let sent = client.emails.send(email("Hello")).await.expect("sent");

    let _bounced = post(
        &format!("{}/_admin/emails/{}/events", server.base_url(), sent.id),
        serde_json::json!({ "api_key": name, "event": "bounced" }),
    )
    .await;

    let (status, body) = post(
        &format!("{}/_admin/emails/{}/events", server.base_url(), sent.id),
        serde_json::json!({ "api_key": name, "event": "delivered" }),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(
        body["name"], "validation_error",
        "a mock that let a caller drive an impossible state would make broken handling look correct"
    );
}

#[tokio::test]
async fn should_refuse_an_event_name_resend_does_not_report() {
    let name = "re_should_refuse_an_unknown_event_name";
    let (server, client) = instance(name).await;

    let sent = client.emails.send(email("Hello")).await.expect("sent");

    let (status, body) = post(
        &format!("{}/_admin/emails/{}/events", server.base_url(), sent.id),
        serde_json::json!({ "api_key": name, "event": "exploded" }),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(body["name"], "validation_error");
}

#[tokio::test]
async fn should_move_the_clock_and_send_what_that_made_due() {
    let name = "re_should_move_the_clock_by_admin";
    let (server, client) = instance(name).await;

    let due = server.now() + chrono::Duration::hours(24);
    let sent = client
        .emails
        .send(email("Tomorrow").with_scheduled_at(&due.to_rfc3339()))
        .await
        .expect("sent");

    let (status, body) = post(
        &format!("{}/_admin/clock", server.base_url()),
        serde_json::json!({ "seconds": 86_400 }),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["transitions"][0]["to"], "sent");

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
async fn should_refuse_to_move_the_clock_backwards() {
    let (server, _client) = instance("re_should_refuse_a_backwards_clock").await;

    let (status, body) = post(
        &format!("{}/_admin/clock", server.base_url()),
        serde_json::json!({ "seconds": -60 }),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(body["name"], "validation_error");
}

#[tokio::test]
async fn should_refuse_a_clock_request_that_says_nothing() {
    let (server, _client) = instance("re_should_refuse_an_empty_clock_request").await;

    let (status, _body) = post(
        &format!("{}/_admin/clock", server.base_url()),
        serde_json::json!({}),
    )
    .await;

    assert_eq!(status, 400);
}

#[tokio::test]
async fn should_inject_a_fault_through_the_admin_api() {
    let name = "re_should_inject_a_fault_by_admin";
    let (server, client) = instance(name).await;

    let (status, _body) = post(
        &format!("{}/_admin/faults", server.base_url()),
        serde_json::json!({ "api_key": name, "error": "application_error", "target": "send" }),
    )
    .await;

    assert_eq!(status, 201);

    let error = client
        .emails
        .send(email("Hello"))
        .await
        .expect_err("the fault fires");

    let resend_rs::Error::Resend(error) = error else {
        panic!("the SDK should parse this as a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "application_error");
    assert_eq!(
        error.status_code, 500,
        "the status defaults to the one Resend serves that error with"
    );
}

#[tokio::test]
async fn should_inject_a_fault_a_limited_number_of_times() {
    let name = "re_should_inject_a_fault_n_times";
    let (server, client) = instance(name).await;

    let _added = post(
        &format!("{}/_admin/faults", server.base_url()),
        serde_json::json!({
            "api_key": name,
            "error": "rate_limit_exceeded",
            "target": "send",
            "times": 1
        }),
    )
    .await;

    assert!(client.emails.send(email("Hello")).await.is_err());
    assert!(
        client.emails.send(email("Hello")).await.is_ok(),
        "a rule set to fire once must not fire twice"
    );
}

#[tokio::test]
async fn should_refuse_to_inject_an_error_name_resend_does_not_have() {
    let name = "re_should_refuse_an_invented_error_by_admin";
    let (server, _client) = instance(name).await;

    let (status, body) = post(
        &format!("{}/_admin/faults", server.base_url()),
        serde_json::json!({ "api_key": name, "error": "everything_is_broken" }),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(body["name"], "validation_error");
}

#[tokio::test]
async fn should_clear_faults_for_a_key() {
    let name = "re_should_clear_faults_by_admin";
    let (server, client) = instance(name).await;

    let _added = post(
        &format!("{}/_admin/faults", server.base_url()),
        serde_json::json!({ "api_key": name, "error": "application_error" }),
    )
    .await;

    let response = http()
        .delete(format!(
            "{}/_admin/faults?api_key={name}",
            server.base_url()
        ))
        .send()
        .await
        .expect("answers");

    assert_eq!(response.status(), 200);
    assert!(client.emails.send(email("Hello")).await.is_ok());
}

#[tokio::test]
async fn should_make_delivery_bounce_through_the_admin_api() {
    let name = "re_should_make_delivery_bounce_by_admin";
    let (server, client) = instance(name).await;

    let (status, _body) = post(
        &format!("{}/_admin/delivery", server.base_url()),
        serde_json::json!({ "api_key": name, "outcome": "bounced", "delay_ms": 1_000 }),
    )
    .await;

    assert_eq!(status, 200);

    let sent = client.emails.send(email("Hello")).await.expect("sent");

    let _advanced = post(
        &format!("{}/_admin/clock", server.base_url()),
        serde_json::json!({ "seconds": 5 }),
    )
    .await;

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
async fn should_register_a_webhook_endpoint_and_report_its_secret() {
    let name = "re_should_register_a_webhook_by_admin";
    let (server, _client) = instance(name).await;

    let (status, body) = post(
        &format!("{}/_admin/webhooks", server.base_url()),
        serde_json::json!({
            "api_key": name,
            "url": "http://127.0.0.1:3999/hooks",
            "events": ["email.delivered", "bounced"]
        }),
    )
    .await;

    assert_eq!(status, 201);
    assert!(
        body["signing_secret"]
            .as_str()
            .is_some_and(|secret| secret.starts_with("whsec_")),
        "a test that cannot read the secret cannot check a signature"
    );
    // Both `email.delivered` and the bare `bounced` are accepted, and both come back in
    // the webhook form.
    assert_eq!(
        body["events"],
        serde_json::json!(["email.delivered", "email.bounced"])
    );
}

#[tokio::test]
async fn should_list_and_remove_a_webhook_endpoint() {
    let name = "re_should_list_and_remove_a_webhook";
    let (server, _client) = instance(name).await;

    let (_status, created) = post(
        &format!("{}/_admin/webhooks", server.base_url()),
        serde_json::json!({ "api_key": name, "url": "http://127.0.0.1:3999/hooks" }),
    )
    .await;

    let id = created["id"].as_str().expect("an id");

    let (_status, list) = get(&format!(
        "{}/_admin/webhooks?api_key={name}",
        server.base_url()
    ))
    .await;
    assert_eq!(list["count"], 1);

    let response = http()
        .delete(format!(
            "{}/_admin/webhooks/{id}?api_key={name}",
            server.base_url()
        ))
        .send()
        .await
        .expect("answers");
    assert_eq!(response.status(), 200);

    let (_status, list) = get(&format!(
        "{}/_admin/webhooks?api_key={name}",
        server.base_url()
    ))
    .await;
    assert_eq!(list["count"], 0);
}

#[tokio::test]
async fn should_refuse_a_webhook_url_that_is_not_http() {
    let name = "re_should_refuse_a_non_http_webhook";
    let (server, _client) = instance(name).await;

    let (status, _body) = post(
        &format!("{}/_admin/webhooks", server.base_url()),
        serde_json::json!({ "api_key": name, "url": "file:///etc/passwd" }),
    )
    .await;

    assert_eq!(status, 400);
}

#[tokio::test]
async fn should_report_statistics_for_a_key() {
    let name = "re_should_report_statistics";
    let (server, client) = instance(name).await;

    for subject in ["one", "two", "three"] {
        let _sent = client.emails.send(email(subject)).await.expect("sent");
    }

    let _registered = post(
        &format!("{}/_admin/webhooks", server.base_url()),
        serde_json::json!({ "api_key": name, "url": "http://127.0.0.1:3999/hooks" }),
    )
    .await;

    let (status, body) = get(&format!(
        "{}/_admin/stats?api_key={name}",
        server.base_url()
    ))
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["captured"], 3);
    assert_eq!(body["by_event"]["sent"], 3);
    assert_eq!(body["webhook_endpoints"], 1);
}

#[tokio::test]
async fn should_not_rate_limit_the_admin_api() {
    let name = "re_should_not_rate_limit_admin";
    let server = mock_resend_testkit::MockResend::start_with(
        mock_resend_testkit::Options::new().rate_limit(1),
    )
    .await
    .expect("the instance binds a port");

    // A suite that has shut itself out still needs a working way to reset.
    for _attempt in 0..5 {
        let (status, _body) = post(
            &format!("{}/_admin/reset", server.base_url()),
            serde_json::json!({ "api_key": name }),
        )
        .await;

        assert_eq!(status, 200);
    }
}

#[tokio::test]
async fn should_refuse_an_admin_request_with_a_key_that_is_not_a_resend_key() {
    let (server, _client) = instance("re_should_refuse_a_bad_admin_key").await;

    let (status, body) = get(&format!(
        "{}/_admin/emails?api_key=not-a-resend-key",
        server.base_url()
    ))
    .await;

    assert_eq!(status, 401);
    assert_eq!(body["name"], "missing_api_key");
}

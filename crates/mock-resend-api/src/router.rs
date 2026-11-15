//! The route table.

use axum::Router;
use axum::routing::{get, post};
use mock_resend_core::Engine;

use crate::error::ApiError;
use crate::routes::emails;

/// Builds the Resend API router over an engine.
///
/// Both fallbacks matter for fidelity. Axum's own 404 and 405 responses have empty
/// bodies, and the Rust SDK treats a 4xx it cannot deserialise as a transport failure,
/// so a typo in a base URL would surface as a parse error rather than as `not_found`.
pub fn router(engine: Engine) -> Router {
    Router::new()
        .route("/emails", post(emails::send_email))
        .route("/emails/{id}", get(emails::get_email))
        .fallback(|| async { ApiError::unknown_endpoint() })
        .method_not_allowed_fallback(|| async { ApiError::method_not_allowed() })
        .with_state(engine)
}

#[cfg(test)]
mod router {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use mock_resend_core::Engine;
    use tower::ServiceExt as _;

    use super::router;

    /// The largest body a test reads back, which is far more than any of these produce.
    const BODY_LIMIT: usize = 64 * 1024;

    async fn response_to(request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = router(Engine::in_memory())
            .oneshot(request)
            .await
            .expect("the router is infallible");

        let status = response.status();
        let body = to_bytes(response.into_body(), BODY_LIMIT)
            .await
            .expect("the body is small");

        let json = if body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&body).expect("every response body is json")
        };

        (status, json)
    }

    fn send(key: Option<&str>, body: &str) -> Request<Body> {
        let mut builder = Request::post("/emails").header("content-type", "application/json");

        if let Some(key) = key {
            builder = builder.header("authorization", format!("Bearer {key}"));
        }

        builder
            .body(Body::from(body.to_owned()))
            .expect("a well formed test request")
    }

    const VALID_BODY: &str = r#"{
        "from": "Acme <onboarding@example.com>",
        "to": "delivered@example.org",
        "subject": "Hello World",
        "text": "It works"
    }"#;

    #[tokio::test]
    async fn should_answer_a_send_with_two_hundred_and_an_id() {
        let (status, body) = response_to(send(
            Some("re_should_answer_a_send_with_two_hundred"),
            VALID_BODY,
        ))
        .await;

        assert_eq!(
            status,
            StatusCode::OK,
            "Resend reports a successful send as 200, not 201"
        );
        assert!(
            body["id"].as_str().is_some_and(|id| id.len() == 36),
            "the response carries a uuid"
        );
    }

    #[tokio::test]
    async fn should_serve_a_json_error_body_for_an_unknown_endpoint() {
        let request = Request::get("/audiences")
            .header("authorization", "Bearer re_unknown_endpoint")
            .body(Body::empty())
            .expect("a well formed test request");

        let (status, body) = response_to(request).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["name"], "not_found");
        assert_eq!(body["statusCode"], 404);
    }

    #[tokio::test]
    async fn should_serve_a_json_error_body_for_the_wrong_method() {
        let request = Request::delete("/emails")
            .header("authorization", "Bearer re_wrong_method")
            .body(Body::empty())
            .expect("a well formed test request");

        let (status, body) = response_to(request).await;

        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(body["name"], "method_not_allowed");
    }

    #[tokio::test]
    async fn should_reject_a_send_without_an_api_key() {
        let (status, body) = response_to(send(None, VALID_BODY)).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["name"], "missing_api_key");
    }

    #[tokio::test]
    async fn should_reject_a_body_that_is_not_json() {
        let (status, body) = response_to(send(
            Some("re_should_reject_a_non_json_body"),
            "not json at all",
        ))
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["name"], "validation_error");
    }

    #[tokio::test]
    async fn should_reject_an_empty_body() {
        let (status, body) = response_to(send(Some("re_should_reject_an_empty_body"), "")).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["name"], "validation_error");
    }

    #[tokio::test]
    async fn should_report_a_missing_required_field_as_four_twenty_two() {
        let (status, body) = response_to(send(
            Some("re_should_report_a_missing_field"),
            r#"{"to": "b@example.org"}"#,
        ))
        .await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["name"], "missing_required_field");
    }

    #[tokio::test]
    async fn should_report_an_unparseable_id_as_an_invalid_parameter() {
        let request = Request::get("/emails/not-a-uuid")
            .header("authorization", "Bearer re_should_report_an_unparseable_id")
            .body(Body::empty())
            .expect("a well formed test request");

        let (status, body) = response_to(request).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["name"], "invalid_parameter");
    }

    #[tokio::test]
    async fn should_report_an_unknown_but_well_formed_id_as_not_found() {
        let request = Request::get("/emails/9b7a0f6e-0000-4000-8000-000000000000")
            .header("authorization", "Bearer re_should_report_an_unknown_id")
            .body(Body::empty())
            .expect("a well formed test request");

        let (status, body) = response_to(request).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["name"], "not_found");
    }

    #[tokio::test]
    async fn should_serve_every_error_body_as_json() {
        // The Rust SDK gives up on an error body whose content type contains `html`, and
        // reports a parse failure instead of the Resend error. Every path has to be JSON.
        let request = Request::get("/not-a-resend-path")
            .body(Body::empty())
            .expect("a well formed test request");

        let response = router(Engine::in_memory())
            .oneshot(request)
            .await
            .expect("the router is infallible");

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .expect("an error response declares its content type");

        assert!(
            content_type.contains("application/json"),
            "error bodies must be json, got {content_type}"
        );
    }
}

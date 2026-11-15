//! The rate limit layer, and the `ratelimit-*` headers.
//!
//! Applied to the Resend surface only. The admin API is never rate limited: a suite
//! that has shut itself out still needs a working way to reset.

use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use mock_resend_core::{Allowance, Engine, ResendError};

use crate::auth::bearer_key;
use crate::error::ApiError;

/// The ceiling for the current window.
const LIMIT: HeaderName = HeaderName::from_static("ratelimit-limit");

/// What is left in the current window.
const REMAINING: HeaderName = HeaderName::from_static("ratelimit-remaining");

/// Seconds until the window resets.
const RESET: HeaderName = HeaderName::from_static("ratelimit-reset");

/// Counts the request against the key's allowance, and reports the headers.
///
/// The headers go on every response once a limit is configured, not just on a `429`:
/// the Rust SDK reads them when it is refused, and a client pacing itself wants them
/// while it is still being served. With no limit configured nothing is added at all,
/// rather than headers describing a limit that does not exist.
pub async fn enforce(State(engine): State<Engine>, request: Request, next: Next) -> Response {
    // An unauthenticated request has no key to charge, so it goes through to the
    // handler, which refuses it with `missing_api_key`.
    let Some(key) = bearer_key(request.headers()) else {
        return next.run(request).await;
    };

    let allowance = match engine.check_rate_limit(&key) {
        Ok(allowance) => allowance,
        Err(error) => return ApiError::from(error).into_response(),
    };

    let Some(allowance) = allowance else {
        return next.run(request).await;
    };

    let mut response = if allowance.allowed {
        next.run(request).await
    } else {
        // `rate_limit_exceeded` with a `429`, which is what Resend sends and what the
        // SDK turns into its own typed rate limit error.
        ApiError::from(mock_resend_core::Error::Injected(ResendError {
            name: "rate_limit_exceeded".to_owned(),
            status: 429,
            message: "Too many requests. Please limit the number of requests per second."
                .to_owned(),
        }))
        .into_response()
    };

    apply_headers(response.headers_mut(), allowance);

    response
}

/// Writes the three `ratelimit-*` headers.
fn apply_headers(headers: &mut axum::http::HeaderMap, allowance: Allowance) {
    for (name, value) in [
        (LIMIT, allowance.limit.to_string()),
        (REMAINING, allowance.remaining.to_string()),
        (RESET, allowance.reset.to_string()),
    ] {
        // Every value here is a decimal number, so it is always a valid header value;
        // the branch keeps the promise that nothing in this crate panics.
        if let Ok(value) = HeaderValue::from_str(&value) {
            let _replaced = headers.insert(name, value);
        }
    }
}

#[cfg(test)]
mod enforce {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use mock_resend_core::Engine;
    use tower::ServiceExt as _;

    use crate::router::router;

    /// A header's value as text, or `None` if the header is absent.
    fn header(response: &axum::response::Response, name: &str) -> Option<String> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }

    const VALID_BODY: &str = r#"{
        "from": "a@example.com",
        "to": "b@example.org",
        "subject": "s",
        "text": "t"
    }"#;

    fn send(key: &str) -> Request<Body> {
        Request::post("/emails")
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .body(Body::from(VALID_BODY))
            .expect("a well formed test request")
    }

    #[tokio::test]
    async fn should_not_add_headers_when_no_limit_is_configured() {
        let response = router(Engine::in_memory())
            .oneshot(send("re_should_not_add_headers_unlimited"))
            .await
            .expect("infallible");

        assert!(
            response.headers().get("ratelimit-limit").is_none(),
            "headers describing a limit that does not exist would mislead a client"
        );
    }

    #[tokio::test]
    async fn should_report_the_headers_on_a_served_request() {
        let response = router(Engine::in_memory().with_rate_limit(Some(5)))
            .oneshot(send("re_should_report_headers_when_served"))
            .await
            .expect("infallible");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header(&response, "ratelimit-limit").as_deref(), Some("5"));
        assert_eq!(
            header(&response, "ratelimit-remaining").as_deref(),
            Some("4")
        );
        assert!(
            header(&response, "ratelimit-reset").is_some(),
            "the SDK reads all three"
        );
    }

    #[tokio::test]
    async fn should_refuse_with_rate_limit_exceeded_past_the_allowance() {
        let app = router(Engine::in_memory().with_rate_limit(Some(1)));

        let _first = app
            .clone()
            .oneshot(send("re_should_refuse_past_the_allowance"))
            .await
            .expect("infallible");

        let response = app
            .oneshot(send("re_should_refuse_past_the_allowance"))
            .await
            .expect("infallible");

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            header(&response, "ratelimit-remaining").as_deref(),
            Some("0")
        );
    }

    #[tokio::test]
    async fn should_let_an_unauthenticated_request_reach_its_own_error() {
        let request = Request::post("/emails")
            .header("content-type", "application/json")
            .body(Body::from(VALID_BODY))
            .expect("a well formed test request");

        let response = router(Engine::in_memory().with_rate_limit(Some(0)))
            .oneshot(request)
            .await
            .expect("infallible");

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "a missing key is missing_api_key, not a rate limit"
        );
    }
}

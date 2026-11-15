//! `Bearer` parsing.
//!
//! Nothing is authenticated. The key is extracted because it is the namespace every
//! request works in, not because it grants anything: any `re_`-prefixed string is
//! accepted and creates its namespace on first use. See `SECURITY.md`.

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use mock_resend_core::{ApiKey, Error as EngineError};

use crate::error::ApiError;

/// Reads the API key from a header map, without producing an error.
///
/// Used by middleware that has to run before the handler rejects an unauthenticated
/// request: there is no key to charge a rate limit against, and refusing here would
/// hide Resend's `missing_api_key` behind a different error.
#[must_use]
pub fn bearer_key(headers: &axum::http::HeaderMap) -> Option<ApiKey> {
    let header = headers.get(AUTHORIZATION)?.to_str().ok()?;

    let credential = header
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, credential)| credential.trim())?;

    ApiKey::parse(credential).ok()
}

/// The API key a request works under, pulled from `Authorization: Bearer <key>`.
///
/// Rejects with `401 missing_api_key` when the header is absent, is not a `Bearer`
/// credential, or does not look like a Resend key — which is the same error Resend
/// returns, so a caller that forgot to configure a key sees what it would see in
/// production rather than a silent success under an empty namespace.
#[derive(Debug, Clone)]
pub struct AuthenticatedKey(pub ApiKey);

impl<S> FromRequestParts<S> for AuthenticatedKey
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // `Bearer` is case-insensitive per RFC 7235, and a client that sends `bearer`
        // should not get a different answer here than it gets from the real API.
        bearer_key(&parts.headers)
            .map(Self)
            .ok_or_else(|| ApiError::from(EngineError::MissingApiKey))
    }
}

#[cfg(test)]
mod authenticated_key {
    use axum::extract::FromRequestParts;
    use axum::http::{Request, StatusCode};

    use super::AuthenticatedKey;

    async fn extract(header: Option<&str>) -> Result<AuthenticatedKey, crate::error::ApiError> {
        let mut builder = Request::builder().uri("/emails");

        if let Some(header) = header {
            builder = builder.header("authorization", header);
        }

        let request = builder.body(()).expect("a well formed test request");
        let (mut parts, ()) = request.into_parts();

        AuthenticatedKey::from_request_parts(&mut parts, &()).await
    }

    #[tokio::test]
    async fn should_accept_a_bearer_key() {
        let extracted = extract(Some("Bearer re_should_accept_a_bearer_key"))
            .await
            .expect("a re_ key is accepted");

        assert_eq!(extracted.0.as_str(), "re_should_accept_a_bearer_key");
    }

    #[tokio::test]
    async fn should_accept_a_lowercase_bearer_scheme() {
        let extracted = extract(Some("bearer re_should_accept_lowercase_bearer"))
            .await
            .expect("the scheme is case-insensitive");

        assert_eq!(extracted.0.as_str(), "re_should_accept_lowercase_bearer");
    }

    #[tokio::test]
    async fn should_reject_a_request_with_no_authorization_header() {
        let error = extract(None).await.expect_err("no header");

        assert_eq!(error.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(error.name(), "missing_api_key");
    }

    #[tokio::test]
    async fn should_reject_a_bare_key_without_the_bearer_scheme() {
        let error = extract(Some("re_no_scheme")).await.expect_err("no scheme");

        assert_eq!(error.name(), "missing_api_key");
    }

    #[tokio::test]
    async fn should_reject_a_scheme_that_is_not_bearer() {
        let error = extract(Some("Basic cmVfdGVzdA=="))
            .await
            .expect_err("not bearer");

        assert_eq!(error.name(), "missing_api_key");
    }

    #[tokio::test]
    async fn should_reject_a_key_without_the_re_prefix() {
        let error = extract(Some("Bearer sk_live_nope"))
            .await
            .expect_err("not a resend key");

        assert_eq!(error.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(error.name(), "missing_api_key");
    }

    #[tokio::test]
    async fn should_reject_an_empty_bearer_credential() {
        let error = extract(Some("Bearer "))
            .await
            .expect_err("nothing after bearer");

        assert_eq!(error.name(), "missing_api_key");
    }
}

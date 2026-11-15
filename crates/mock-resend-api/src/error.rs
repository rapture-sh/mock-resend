//! The one place engine errors become Resend error names and statuses.
//!
//! A handler never writes an error body itself. The
//! [errors reference](https://resend.com/docs/api-reference/errors) is the source of
//! truth, and the mapping is not one-to-one: `validation_error` appears under several
//! statuses depending on what failed, so each variant names its own.
//!
//! The body shape is `{"statusCode", "message", "name"}`, confirmed against
//! `resend_rs::types::ErrorResponse`, which is what a real SDK deserialises. Note that
//! it is served as JSON on every path, including 404 and 405: the Rust SDK treats an
//! HTML error body as an unparseable response, so an empty framework 404 surfaces to a
//! caller as a transport failure rather than as `not_found`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use mock_resend_core::Error as EngineError;
use serde::Serialize;

/// A Resend error, ready to serialise.
#[derive(Debug, Clone)]
pub struct ApiError {
    status: StatusCode,
    /// Resend's error `name`. This is the contract callers branch on.
    name: &'static str,
    message: String,
}

/// The wire form of an error.
#[derive(Debug, Serialize)]
struct ErrorBody<'a> {
    /// Resend repeats the status in the body, in camelCase while every other field in
    /// the API is snake_case.
    #[serde(rename = "statusCode")]
    status_code: u16,
    message: &'a str,
    name: &'a str,
}

impl ApiError {
    /// Builds an error with an explicit status and name.
    #[must_use]
    pub fn new(status: StatusCode, name: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            name,
            message: message.into(),
        }
    }

    /// `400 validation_error`, for a body that did not parse or a field that did not
    /// pass.
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "validation_error", message)
    }

    /// `404 not_found`, for a path that is not part of the supported surface.
    #[must_use]
    pub fn unknown_endpoint() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "The requested endpoint does not exist.",
        )
    }

    /// `405 method_not_allowed`, for a supported path reached with the wrong method.
    #[must_use]
    pub fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "Method is not allowed for the requested path.",
        )
    }

    /// Resend's error `name`.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The status this error is served with.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl From<EngineError> for ApiError {
    fn from(error: EngineError) -> Self {
        // The message is always the engine's `Display`, so the wording lives next to the
        // rule that produced it rather than here.
        let message = error.to_string();

        let (status, name) = match error {
            EngineError::MissingRequiredField { .. } => {
                (StatusCode::UNPROCESSABLE_ENTITY, "missing_required_field")
            }
            EngineError::Validation { .. } => (StatusCode::BAD_REQUEST, "validation_error"),
            EngineError::InvalidAttachment => {
                (StatusCode::UNPROCESSABLE_ENTITY, "invalid_attachment")
            }
            EngineError::InvalidParameter => {
                (StatusCode::UNPROCESSABLE_ENTITY, "invalid_parameter")
            }
            EngineError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            EngineError::MissingApiKey => (StatusCode::UNAUTHORIZED, "missing_api_key"),
            // A storage failure is a bug here, not something the caller did.
            EngineError::Store(_) => (StatusCode::INTERNAL_SERVER_ERROR, "application_error"),
        };

        Self::new(status, name, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            status_code: self.status.as_u16(),
            message: &self.message,
            name: self.name,
        };

        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod api_error {
    use axum::http::StatusCode;
    use mock_resend_core::Error as EngineError;

    use super::ApiError;

    /// Every name and status pairing this crate can produce, against the Resend errors
    /// reference. A wrong pairing cannot be caught by a conformance test, because the
    /// real API is the thing being imitated.
    #[test]
    fn should_map_each_engine_error_to_its_resend_name_and_status() {
        let cases: Vec<(EngineError, StatusCode, &str)> = vec![
            (
                EngineError::MissingRequiredField {
                    fields: vec!["from".to_owned()],
                },
                StatusCode::UNPROCESSABLE_ENTITY,
                "missing_required_field",
            ),
            (
                EngineError::validation("nope"),
                StatusCode::BAD_REQUEST,
                "validation_error",
            ),
            (
                EngineError::InvalidAttachment,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_attachment",
            ),
            (
                EngineError::InvalidParameter,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_parameter",
            ),
            (EngineError::NotFound, StatusCode::NOT_FOUND, "not_found"),
            (
                EngineError::MissingApiKey,
                StatusCode::UNAUTHORIZED,
                "missing_api_key",
            ),
            (
                EngineError::Store("disk went away".to_owned()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "application_error",
            ),
        ];

        for (engine_error, status, name) in cases {
            let mapped = ApiError::from(engine_error);

            assert_eq!(mapped.status(), status, "status for {name}");
            assert_eq!(mapped.name(), name);
        }
    }

    #[test]
    fn should_carry_the_engine_message_through_unchanged() {
        let mapped = ApiError::from(EngineError::validation("`to` is not a valid email address"));

        assert_eq!(mapped.message, "`to` is not a valid email address");
    }

    #[test]
    fn should_name_the_missing_fields_in_the_message() {
        let mapped = ApiError::from(EngineError::MissingRequiredField {
            fields: vec!["from".to_owned(), "subject".to_owned()],
        });

        assert!(mapped.message.contains("from"));
        assert!(mapped.message.contains("subject"));
    }
}

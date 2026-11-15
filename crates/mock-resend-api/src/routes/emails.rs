//! The `/emails` handlers.
//!
//! Parse the request, call the engine, map the error. Nothing else: a rule that lives
//! here rather than in the engine is a rule the SMTP listener does not get.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, body::Bytes};
use mock_resend_core::{EmailId, Engine};

use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::wire::emails::{EmailResponse, SendEmailRequest, SendEmailResponse};

/// `POST /emails`
///
/// Captures a message and reports its id. Nothing is delivered.
///
/// Returns `200`, not `201`: Resend reports a successful send as `200 OK` even though
/// it creates a resource.
///
/// <https://resend.com/docs/api-reference/emails/send-email>
pub async fn send_email(
    State(engine): State<Engine>,
    AuthenticatedKey(key): AuthenticatedKey,
    body: Bytes,
) -> Result<Response, ApiError> {
    // The body is taken as bytes and parsed here rather than through `Json<T>`, so that
    // a malformed body produces a Resend error body instead of the framework's own
    // rejection, which no SDK can deserialise.
    let request: SendEmailRequest = parse_body(&body)?;

    let id = engine
        .send_email(&key, request.into_engine_request()?)
        .await?;

    // An id and the key prefix. Never a subject, a recipient or a body: captured mail is
    // sometimes real data a developer pointed at this by mistake.
    tracing::info!(email_id = %id, "captured a message");

    let response = SendEmailResponse { id: id.to_string() };

    Ok((StatusCode::OK, Json(response)).into_response())
}

/// `GET /emails/{id}`
///
/// Returns a captured message, including its most recent delivery event.
///
/// <https://resend.com/docs/api-reference/emails/retrieve-email>
pub async fn get_email(
    State(engine): State<Engine>,
    AuthenticatedKey(key): AuthenticatedKey,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    // A non-UUID is `invalid_parameter`, not `not_found`. Getting this backwards hides a
    // caller that is passing the wrong value entirely.
    let id: EmailId = id.parse()?;

    let message = engine.get_email(&key, &id).await?;

    Ok((StatusCode::OK, Json(EmailResponse::from(message))).into_response())
}

/// Parses a JSON request body into `T`.
///
/// A parse failure is `400 validation_error`, with the serde message included: it names
/// the offending field and line, which is exactly what someone debugging a payload
/// needs.
fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, ApiError> {
    if body.is_empty() {
        return Err(ApiError::validation("The request body is empty."));
    }

    serde_json::from_slice(body).map_err(|error| {
        ApiError::validation(format!("The request body could not be parsed: {error}."))
    })
}

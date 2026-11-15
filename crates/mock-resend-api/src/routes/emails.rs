//! The `/emails` handlers.
//!
//! Parse the request, call the engine, map the error. Nothing else: a rule that lives
//! here rather than in the engine is a rule the SMTP listener does not get.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, body::Bytes};
use mock_resend_core::{BatchValidation, EmailId, Engine, FaultTarget, IdempotencyKey};

use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::wire::emails::{
    EmailIdResponse, EmailResponse, SendBatchResponse, SendEmailRequest, SendEmailResponse,
    UpdateEmailRequest,
};

/// The header Resend reads to choose a batch's validation mode.
const BATCH_VALIDATION_HEADER: &str = "x-batch-validation";

/// The header that makes a send replayable.
const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// Parses `Idempotency-Key`, if the caller sent one.
///
/// An absent header is `None` rather than an error: idempotency is opt-in. A present
/// but unusable one is `invalid_idempotency_key`, because silently ignoring it would
/// let a caller believe a retry was deduplicated when it was not.
fn idempotency_key(headers: &HeaderMap) -> Result<Option<IdempotencyKey>, ApiError> {
    let Some(value) = headers.get(IDEMPOTENCY_HEADER) else {
        return Ok(None);
    };

    let value = value
        .to_str()
        .map_err(|_| ApiError::from(mock_resend_core::Error::InvalidIdempotencyKey))?;

    Ok(Some(IdempotencyKey::parse(value)?))
}

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
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    // Faults are consulted first, before the body is even parsed: a rule that only
    // fired for well-formed requests could not be used to test a client's handling of a
    // server that is simply down.
    engine.check_fault(&key, FaultTarget::Send)?;

    let idempotency = idempotency_key(&headers)?;

    // The body is taken as bytes and parsed here rather than through `Json<T>`, so that
    // a malformed body produces a Resend error body instead of the framework's own
    // rejection, which no SDK can deserialise.
    let request: SendEmailRequest = parse_body(&body)?;

    let id = engine
        .send_email_idempotent(&key, request.into_engine_request()?, idempotency.as_ref())
        .await?;

    // An id and the key prefix. Never a subject, a recipient or a body: captured mail is
    // sometimes real data a developer pointed at this by mistake.
    tracing::info!(email_id = %id, "captured a message");

    let response = SendEmailResponse { id: id.to_string() };

    Ok((StatusCode::OK, Json(response)).into_response())
}

/// `POST /emails/batch`
///
/// Captures up to 100 messages in one call. The body is a bare JSON array, not an
/// object wrapping one, which is what both the Node and Rust SDKs send.
///
/// <https://resend.com/docs/api-reference/emails/send-batch-emails>
pub async fn send_batch(
    State(engine): State<Engine>,
    AuthenticatedKey(key): AuthenticatedKey,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    engine.check_fault(&key, FaultTarget::Send)?;

    let requests: Vec<SendEmailRequest> = parse_body(&body)?;

    let validation = BatchValidation::parse(
        headers
            .get(BATCH_VALIDATION_HEADER)
            .and_then(|value| value.to_str().ok()),
    );

    let idempotency = idempotency_key(&headers)?;

    // A body that will not convert at all fails the whole call whatever the mode: a
    // shape error is not a per-entry validation failure, and permissive mode is about
    // the latter.
    let mut engine_requests = Vec::with_capacity(requests.len());
    for request in requests {
        engine_requests.push(request.into_engine_request()?);
    }

    let outcome = engine
        .send_batch_idempotent(&key, engine_requests, validation, idempotency.as_ref())
        .await?;

    tracing::info!(
        captured = outcome.ids.len(),
        refused = outcome.failures.len(),
        "captured a batch"
    );

    Ok((StatusCode::OK, Json(SendBatchResponse::from(outcome))).into_response())
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
    engine.check_fault(&key, FaultTarget::Retrieve)?;

    let id = parse_id(&id)?;
    let message = engine.get_email(&key, &id).await?;

    Ok((StatusCode::OK, Json(EmailResponse::from(message))).into_response())
}

/// `PATCH /emails/{id}`
///
/// Moves a scheduled send to a new time.
///
/// <https://resend.com/docs/api-reference/emails/update-email>
pub async fn update_email(
    State(engine): State<Engine>,
    AuthenticatedKey(key): AuthenticatedKey,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    // `FaultTarget::Any` says every Resend endpoint, so a suite has to be able to make
    // a reschedule fail too. Rescheduling and cancelling are grouped under `Send`:
    // they act on an outgoing message rather than reading one.
    engine.check_fault(&key, FaultTarget::Send)?;

    let id = parse_id(&id)?;
    let request: UpdateEmailRequest = parse_body(&body)?;

    let updated = engine
        .reschedule_email(&key, &id, request.scheduled_at()?)
        .await?;

    tracing::info!(email_id = %updated, "rescheduled a message");

    Ok((StatusCode::OK, Json(EmailIdResponse::new(&updated))).into_response())
}

/// `POST /emails/{id}/cancel`
///
/// Cancels a scheduled send. Resend has no `email.canceled` webhook, so this changes
/// the retrieved state without notifying a handler.
///
/// <https://resend.com/docs/api-reference/emails/cancel-email>
pub async fn cancel_email(
    State(engine): State<Engine>,
    AuthenticatedKey(key): AuthenticatedKey,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    engine.check_fault(&key, FaultTarget::Send)?;

    let id = parse_id(&id)?;
    let cancelled = engine.cancel_email(&key, &id).await?;

    tracing::info!(email_id = %cancelled, "cancelled a scheduled message");

    Ok((StatusCode::OK, Json(EmailIdResponse::new(&cancelled))).into_response())
}

/// Parses an id from the path.
///
/// A non-UUID is `invalid_parameter`, not `not_found`. Getting this backwards hides a
/// caller that is passing the wrong value entirely.
fn parse_id(id: &str) -> Result<EmailId, ApiError> {
    id.parse().map_err(ApiError::from)
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

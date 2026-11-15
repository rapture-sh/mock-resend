//! The `/_admin` handlers.
//!
//! Thin, like the Resend handlers: parse, call the engine, map the error. Every
//! behaviour they expose is a method on the engine, so moving the clock from here and
//! moving it from a Rust test do the same thing.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, body::Bytes};
use mock_resend_api::ApiError;
use mock_resend_core::{
    DeliveryRule, EmailEvent, EmailId, Engine, Error as EngineError, FaultRule, ResendError,
    Transition, WebhookId, fault,
};

use crate::timestamp;
use crate::wire::{
    AdvanceClockRequest, AdvanceClockResponse, AdvanceEventRequest, CapturedEmail, DeliveryRequest,
    EmailListResponse, FaultRequest, HealthResponse, KeyQuery, ResetResponse, ScopeRequest,
    StatsResponse, TransitionResponse, WebhookListResponse, WebhookRequest, WebhookView,
};

/// `GET /_admin/health`
///
/// Liveness, plus the engine's idea of the time, which is the thing most likely to
/// surprise someone whose scheduled send has not gone out.
pub async fn health(State(engine): State<Engine>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        now: timestamp(engine.now()),
    })
}

/// `POST /_admin/reset`
///
/// Wipes a key, or everything. Far faster than restarting a container, and it clears
/// captured mail, replay records, fault rules, rate limit windows and webhook endpoints
/// together: a key half-reset is worse than one not reset at all.
pub async fn reset(
    State(engine): State<Engine>,
    body: Bytes,
) -> Result<Json<ResetResponse>, ApiError> {
    let request: ScopeRequest = parse_optional_body(&body)?;
    let key = request.key()?;

    engine.reset(key.as_ref()).await?;

    let scope = key
        .as_ref()
        .map_or_else(|| "all".to_owned(), |key| key.as_str().to_owned());

    tracing::info!(scope = %scope, "reset");

    Ok(Json(ResetResponse { scope }))
}

/// `GET /_admin/emails?api_key=re_...`
///
/// Everything captured under a key, oldest first, including the parts Resend does not
/// report back.
pub async fn list_emails(
    State(engine): State<Engine>,
    Query(query): Query<KeyQuery>,
) -> Result<Json<EmailListResponse>, ApiError> {
    let key = query.key()?;
    let messages = engine.list_emails(&key).await?;

    let data: Vec<CapturedEmail> = messages.into_iter().map(CapturedEmail::from).collect();

    Ok(Json(EmailListResponse {
        count: data.len(),
        data,
    }))
}

/// `GET /_admin/emails/{id}?api_key=re_...`
///
/// One captured message, in full.
pub async fn get_email(
    State(engine): State<Engine>,
    Path(id): Path<String>,
    Query(query): Query<KeyQuery>,
) -> Result<Json<CapturedEmail>, ApiError> {
    let key = query.key()?;
    let id: EmailId = id.parse()?;

    let message = engine.get_email(&key, &id).await?;

    Ok(Json(CapturedEmail::from(message)))
}

/// `POST /_admin/emails/{id}/events`
///
/// Moves a message to another delivery event, if the lifecycle allows it. This is how a
/// bounce or an open is driven on demand, and the engine refuses an impossible move: a
/// mock that let a caller drive a message into a state real Resend cannot reach would
/// make broken event handling look correct.
pub async fn advance_event(
    State(engine): State<Engine>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<TransitionResponse>, ApiError> {
    let request: AdvanceEventRequest = parse_body(&body)?;
    let (key, event) = request.parse()?;
    let id: EmailId = id.parse()?;

    let transition = engine.advance_event(&key, &id, event).await?;

    Ok(Json(transition_view(&transition)))
}

/// `POST /_admin/clock`
///
/// Moves the engine's clock forward and reports what that sent. A test for "this email
/// goes out in 24 hours" takes a millisecond instead of a day.
pub async fn advance_clock(
    State(engine): State<Engine>,
    body: Bytes,
) -> Result<Json<AdvanceClockResponse>, ApiError> {
    let request: AdvanceClockRequest = parse_body(&body)?;
    let by = request.duration()?;

    if !engine.advance_clock(by) {
        return Err(ApiError::validation(
            "This instance's clock cannot be moved.",
        ));
    }

    // Moving the clock is only half of it: the tick is what actually sends what the jump
    // made due, and a caller who had to remember to ask separately would eventually
    // forget.
    let transitions = engine.tick().await?;

    tracing::info!(seconds = by.num_seconds(), "advanced the clock");

    Ok(Json(AdvanceClockResponse {
        now: timestamp(engine.now()),
        transitions: transitions.iter().map(transition_view).collect(),
    }))
}

/// `POST /_admin/faults`
///
/// Adds a rule that makes matching requests fail with a given Resend error.
pub async fn add_fault(State(engine): State<Engine>, body: Bytes) -> Result<Response, ApiError> {
    let request: FaultRequest = parse_body(&body)?;
    let (key, target) = request.parse()?;

    // The status defaults to the one Resend serves that error with, so a caller only has
    // to know the name they want to test against.
    let status = match request.status {
        Some(status) => status,
        None => fault::status_for(&request.error).ok_or_else(|| {
            ApiError::from(EngineError::validation(format!(
                "`{}` is not a Resend error name.",
                request.error
            )))
        })?,
    };

    let message = request
        .message
        .clone()
        .unwrap_or_else(|| format!("`{}` injected by a mock-resend fault rule.", request.error));

    engine.faults().add_rule(
        &key,
        FaultRule {
            target,
            error: ResendError {
                name: request.error.clone(),
                status,
                message,
            },
            remaining: request.times,
        },
    )?;

    tracing::info!(error = %request.error, target = target.as_str(), "added a fault rule");

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "status": "added" })),
    )
        .into_response())
}

/// `DELETE /_admin/faults?api_key=re_...`
///
/// Removes every rule under a key.
pub async fn clear_faults(
    State(engine): State<Engine>,
    Query(query): Query<KeyQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let key = query.key()?;

    engine.faults().reset(Some(&key))?;

    Ok(Json(serde_json::json!({ "status": "cleared" })))
}

/// `POST /_admin/delivery`
///
/// Changes what delivery does under a key: the event a sent message lands on, and how
/// long after sending it gets there.
pub async fn set_delivery(
    State(engine): State<Engine>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let request: DeliveryRequest = parse_body(&body)?;
    let key = mock_resend_core::ApiKey::parse(&request.api_key)?;

    let rule = match request.outcome.as_deref() {
        None => None,
        Some(outcome) => {
            let outcome = EmailEvent::parse(outcome.trim()).ok_or_else(|| {
                ApiError::from(EngineError::validation(format!(
                    "`{outcome}` is not a delivery event Resend reports."
                )))
            })?;

            Some(DeliveryRule { outcome })
        }
    };

    engine.faults().set_delivery(&key, rule)?;

    if let Some(delay) = request.delay_ms {
        engine
            .faults()
            .set_delivery_delay(&key, Some(chrono::Duration::milliseconds(delay)))?;
    }

    Ok(Json(serde_json::json!({ "status": "set" })))
}

/// `POST /_admin/webhooks`
///
/// Registers an endpoint and reports its signing secret, so a test can verify what it
/// receives.
pub async fn register_webhook(
    State(engine): State<Engine>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let request: WebhookRequest = parse_body(&body)?;
    let (key, events) = request.parse()?;

    let endpoint = engine
        .webhooks()
        .register(&key, &request.url, events, engine.now())?;

    // The URL, not the mail. An endpoint is configuration a developer typed, not
    // captured data.
    tracing::info!(webhook_id = %endpoint.id, url = %endpoint.url, "registered a webhook endpoint");

    Ok((StatusCode::CREATED, Json(WebhookView::from(endpoint))).into_response())
}

/// `GET /_admin/webhooks?api_key=re_...`
///
/// The endpoints registered under a key.
pub async fn list_webhooks(
    State(engine): State<Engine>,
    Query(query): Query<KeyQuery>,
) -> Result<Json<WebhookListResponse>, ApiError> {
    let key = query.key()?;

    let data: Vec<WebhookView> = engine
        .webhooks()
        .for_key(&key)?
        .into_iter()
        .map(WebhookView::from)
        .collect();

    Ok(Json(WebhookListResponse {
        count: data.len(),
        data,
    }))
}

/// `DELETE /_admin/webhooks/{id}?api_key=re_...`
///
/// Removes one endpoint.
pub async fn remove_webhook(
    State(engine): State<Engine>,
    Path(id): Path<String>,
    Query(query): Query<KeyQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let key = query.key()?;
    let id = WebhookId::parse(&id)?;

    engine.webhooks().remove(&key, &id)?;

    Ok(Json(serde_json::json!({ "status": "removed" })))
}

/// `GET /_admin/stats?api_key=re_...`
///
/// What a key has done, so a suite can assert on counts without retrieving every
/// message.
pub async fn stats(
    State(engine): State<Engine>,
    Query(query): Query<KeyQuery>,
) -> Result<Json<StatsResponse>, ApiError> {
    let key = query.key()?;
    let stats = engine.stats(&key).await?;

    Ok(Json(StatsResponse::new(&key, stats)))
}

/// Renders a transition for the wire.
fn transition_view(transition: &Transition) -> TransitionResponse {
    TransitionResponse {
        id: transition.id.to_string(),
        from: transition.from.as_str().to_owned(),
        to: transition.to.as_str().to_owned(),
        at: timestamp(transition.at),
    }
}

/// Parses a required JSON body.
fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, ApiError> {
    if body.is_empty() {
        return Err(ApiError::validation("The request body is empty."));
    }

    serde_json::from_slice(body).map_err(|error| {
        ApiError::validation(format!("The request body could not be parsed: {error}."))
    })
}

/// Parses an optional JSON body, treating an empty one as the default.
///
/// Used by `reset`, where no body means "everything" and a curl with no `-d` should not
/// be an error.
fn parse_optional_body<T: serde::de::DeserializeOwned + Default>(
    body: &Bytes,
) -> Result<T, ApiError> {
    if body.is_empty() || body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }

    parse_body(body)
}

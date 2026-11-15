//! The `/_admin` route table.

use axum::Router;
use axum::routing::{get, post};
use mock_resend_core::Engine;

use crate::routes;

/// Builds the admin router, already nested under `/_admin`.
///
/// Merged onto the Resend router by the binary rather than mounted from inside it: the
/// Resend surface must not depend on the admin surface, and merging this way also keeps
/// the rate limit layer off these routes.
pub fn router(engine: Engine) -> Router {
    let admin = Router::new()
        .route("/health", get(routes::health))
        .route("/reset", post(routes::reset))
        .route("/emails", get(routes::list_emails))
        .route("/emails/{id}", get(routes::get_email))
        .route("/emails/{id}/events", post(routes::advance_event))
        .route("/clock", post(routes::advance_clock))
        .route(
            "/faults",
            post(routes::add_fault).delete(routes::clear_faults),
        )
        .route("/delivery", post(routes::set_delivery))
        .route(
            "/webhooks",
            post(routes::register_webhook).get(routes::list_webhooks),
        )
        .route(
            "/webhooks/{id}",
            axum::routing::delete(routes::remove_webhook),
        )
        .route("/stats", get(routes::stats))
        .with_state(engine);

    Router::new().nest("/_admin", admin)
}

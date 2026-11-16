//! The `/_admin` route table.

use axum::Router;
use axum::extract::Path;
use axum::routing::{get, post};
use mock_resend_core::Engine;

use crate::{console, routes};

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
        // The console's own reads. Separate from `/emails`, which stays scoped to one
        // key: these deliberately cross keys, because nothing hands a console a key.
        .route("/keys", get(routes::list_keys))
        .route("/mailbox", get(routes::mailbox))
        .route("/mailbox/{id}", get(routes::mailbox_email))
        .route("/mailbox/{id}/raw", get(routes::mailbox_raw))
        .route(
            "/mailbox/{id}/attachments/{index}",
            get(routes::mailbox_attachment),
        )
        .with_state(engine);

    Router::new()
        .nest("/_admin", admin)
        // The console, and a way into it. `GET /` is not a Resend endpoint, so there is
        // nothing to collide with, and typing a port into a browser is how most people
        // will first meet this.
        .route("/", get(|| async { console::redirect_to_console() }))
        .route(
            "/_admin/console",
            get(|| async { console::redirect_to_console() }),
        )
        .route("/_admin/console/", get(|| async { console::index() }))
        .route(
            "/_admin/console/{*path}",
            get(|Path(path): Path<String>| async move { console::serve(&path) }),
        )
}

//! Webhook endpoints and their signing secrets, scoped to an API key.
//!
//! This module holds the registry only. Nothing here makes an outbound request:
//! delivery, signing and retries live in `mock-resend-webhooks`, which is a crate
//! boundary rather than a module precisely so that there is exactly one place to audit
//! for rule 5.

use std::collections::HashMap;
use std::fmt;
use std::sync::RwLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::events::EmailEvent;
use crate::model::ApiKey;

/// The prefix Resend's webhook signing secrets carry, following Svix.
pub const SECRET_PREFIX: &str = "whsec_";

/// The id of a registered endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WebhookId(Uuid);

impl WebhookId {
    /// Mints a fresh id.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parses an id.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidParameter`] for anything that is not a UUID.
    pub fn parse(value: &str) -> Result<Self> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| Error::InvalidParameter)
    }
}

impl fmt::Display for WebhookId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.as_hyphenated())
    }
}

/// A webhook signing secret.
///
/// `whsec_` followed by base64. The base64 part is the HMAC key, and it has to be
/// *valid* base64 rather than merely opaque: a real Svix verifier decodes it before
/// computing the signature, so a secret it cannot decode fails verification for a
/// reason the caller cannot see.
#[derive(Clone, PartialEq, Eq)]
pub struct SigningSecret(String);

impl SigningSecret {
    /// Mints a secret from 32 random bytes.
    ///
    /// Two v4 UUIDs' worth of randomness, which is what is already available here;
    /// nothing in this project needs a cryptographically strong secret, because nothing
    /// is authenticated. The signature exists so a caller's real verification code can
    /// run unmodified.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0_u8; 32];
        bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());

        Self(format!("{SECRET_PREFIX}{}", BASE64.encode(bytes)))
    }

    /// Wraps a caller-supplied secret.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when it is not `whsec_` followed by valid base64.
    pub fn parse(value: &str) -> Result<Self> {
        let Some(encoded) = value.strip_prefix(SECRET_PREFIX) else {
            return Err(Error::validation(format!(
                "A webhook signing secret starts with `{SECRET_PREFIX}`."
            )));
        };

        if encoded.is_empty() || BASE64.decode(encoded).is_err() {
            return Err(Error::validation(
                "The part of a webhook signing secret after the prefix must be base64, because a real Svix verifier decodes it.",
            ));
        }

        Ok(Self(value.to_owned()))
    }

    /// The secret as a caller configures it, prefix included.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The HMAC key: the base64 part, decoded.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] if the secret somehow holds invalid base64, which
    /// [`SigningSecret::parse`] and [`SigningSecret::generate`] both rule out.
    pub fn key_bytes(&self) -> Result<Vec<u8>> {
        let encoded = self.0.strip_prefix(SECRET_PREFIX).unwrap_or(&self.0);

        BASE64
            .decode(encoded)
            .map_err(|_| Error::validation("The webhook signing secret is not valid base64."))
    }
}

impl fmt::Debug for SigningSecret {
    /// Redacts the secret.
    ///
    /// It is not protecting anything here, but a signing secret in a log is a habit
    /// worth not forming.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SigningSecret({SECRET_PREFIX}***)")
    }
}

/// A registered endpoint.
#[derive(Debug, Clone)]
pub struct WebhookEndpoint {
    /// The endpoint's id.
    pub id: WebhookId,
    /// Where the payload is posted. A URL the caller chose, and therefore checked
    /// against the outbound policy before anything is sent to it.
    pub url: String,
    /// Which events it wants. Empty means every event that has a webhook name.
    pub events: Vec<EmailEvent>,
    /// The secret its payloads are signed with.
    pub secret: SigningSecret,
    /// When it was registered.
    pub created_at: DateTime<Utc>,
}

impl WebhookEndpoint {
    /// Whether this endpoint wants `event`.
    #[must_use]
    pub fn wants(&self, event: EmailEvent) -> bool {
        // An event with no webhook name is never delivered, however it was subscribed:
        // Resend has no `email.canceled`, so neither do we.
        if event.webhook_name().is_none() {
            return false;
        }

        self.events.is_empty() || self.events.contains(&event)
    }
}

/// The per-key endpoint registry.
#[derive(Debug, Default)]
pub struct Webhooks {
    keys: RwLock<HashMap<ApiKey, Vec<WebhookEndpoint>>>,
}

/// Turns a poisoned lock into a storage failure.
fn poisoned() -> Error {
    Error::Store("the webhook registry's lock was poisoned by an earlier panic".to_owned())
}

impl Webhooks {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an endpoint under `key`, minting a signing secret.
    ///
    /// The URL is stored as given. Whether it may actually be posted to is decided by
    /// the outbound policy in `mock-resend-webhooks` at delivery time, not here: this
    /// module must stay free of any notion of making a request.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a blank URL or one without an `http` scheme, and
    /// [`Error::Store`] if the registry's lock is poisoned.
    pub fn register(
        &self,
        key: &ApiKey,
        url: &str,
        events: Vec<EmailEvent>,
        now: DateTime<Utc>,
    ) -> Result<WebhookEndpoint> {
        let url = url.trim();

        if url.is_empty() {
            return Err(Error::validation("A webhook endpoint needs a URL."));
        }

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(Error::validation(
                "A webhook endpoint URL must start with `http://` or `https://`.",
            ));
        }

        let endpoint = WebhookEndpoint {
            id: WebhookId::generate(),
            url: url.to_owned(),
            events,
            secret: SigningSecret::generate(),
            created_at: now,
        };

        let mut keys = self.keys.write().map_err(|_| poisoned())?;
        keys.entry(key.clone()).or_default().push(endpoint.clone());

        Ok(endpoint)
    }

    /// Returns `key`'s endpoints, in registration order.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the registry's lock is poisoned.
    pub fn for_key(&self, key: &ApiKey) -> Result<Vec<WebhookEndpoint>> {
        let keys = self.keys.read().map_err(|_| poisoned())?;

        Ok(keys.get(key).cloned().unwrap_or_default())
    }

    /// Returns the endpoints under `key` that want `event`.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the registry's lock is poisoned.
    pub fn subscribed_to(&self, key: &ApiKey, event: EmailEvent) -> Result<Vec<WebhookEndpoint>> {
        Ok(self
            .for_key(key)?
            .into_iter()
            .filter(|endpoint| endpoint.wants(event))
            .collect())
    }

    /// Removes one endpoint.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when `key` has no endpoint with that id, and
    /// [`Error::Store`] if the registry's lock is poisoned.
    pub fn remove(&self, key: &ApiKey, id: &WebhookId) -> Result<()> {
        let mut keys = self.keys.write().map_err(|_| poisoned())?;

        let Some(endpoints) = keys.get_mut(key) else {
            return Err(Error::NotFound);
        };

        let Some(position) = endpoints.iter().position(|endpoint| endpoint.id == *id) else {
            return Err(Error::NotFound);
        };

        let _removed = endpoints.remove(position);

        Ok(())
    }

    /// Removes every endpoint under `key`, or all of them.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the registry's lock is poisoned.
    pub fn reset(&self, key: Option<&ApiKey>) -> Result<()> {
        let mut keys = self.keys.write().map_err(|_| poisoned())?;

        match key {
            Some(key) => {
                let _removed = keys.remove(key);
            }
            None => keys.clear(),
        }

        Ok(())
    }
}

#[cfg(test)]
mod signing_secret {
    use super::{SECRET_PREFIX, SigningSecret};

    #[test]
    fn should_generate_a_secret_with_the_resend_prefix() {
        assert!(
            SigningSecret::generate()
                .as_str()
                .starts_with(SECRET_PREFIX)
        );
    }

    #[test]
    fn should_generate_a_secret_whose_body_is_decodable_base64() {
        // A real Svix verifier decodes this before signing, so a secret it cannot decode
        // fails verification for a reason the caller cannot see.
        assert_eq!(
            SigningSecret::generate()
                .key_bytes()
                .expect("decodes")
                .len(),
            32
        );
    }

    #[test]
    fn should_generate_a_different_secret_each_time() {
        assert_ne!(
            SigningSecret::generate().as_str(),
            SigningSecret::generate().as_str()
        );
    }

    #[test]
    fn should_accept_a_well_formed_caller_supplied_secret() {
        let generated = SigningSecret::generate();

        assert!(SigningSecret::parse(generated.as_str()).is_ok());
    }

    #[test]
    fn should_reject_a_secret_without_the_prefix() {
        assert!(SigningSecret::parse("c2VjcmV0").is_err());
    }

    #[test]
    fn should_reject_a_secret_whose_body_is_not_base64() {
        assert!(SigningSecret::parse("whsec_not base64!!").is_err());
    }

    #[test]
    fn should_reject_a_bare_prefix() {
        assert!(SigningSecret::parse(SECRET_PREFIX).is_err());
    }

    #[test]
    fn should_redact_the_secret_when_debugged() {
        let secret = SigningSecret::generate();
        let debugged = format!("{secret:?}");

        assert_eq!(debugged, "SigningSecret(whsec_***)");
        assert!(
            !debugged.contains(
                secret
                    .as_str()
                    .strip_prefix(SECRET_PREFIX)
                    .expect("it has the prefix")
            )
        );
    }
}

#[cfg(test)]
mod webhooks {
    use chrono::{DateTime, Utc};

    use super::{WebhookId, Webhooks};
    use crate::error::Error;
    use crate::events::EmailEvent;
    use crate::model::ApiKey;

    fn key(name: &str) -> ApiKey {
        ApiKey::parse(name).expect("the test key is re_-prefixed")
    }

    fn now() -> DateTime<Utc> {
        DateTime::UNIX_EPOCH
    }

    #[test]
    fn should_register_an_endpoint_and_report_its_secret() {
        let webhooks = Webhooks::new();
        let key = key("re_should_register_an_endpoint");

        let endpoint = webhooks
            .register(&key, "http://localhost:3000/hooks", Vec::new(), now())
            .expect("registered");

        assert_eq!(endpoint.url, "http://localhost:3000/hooks");
        assert!(endpoint.secret.as_str().starts_with("whsec_"));
    }

    #[test]
    fn should_list_endpoints_in_registration_order() {
        let webhooks = Webhooks::new();
        let key = key("re_should_list_endpoints_in_order");

        for path in ["first", "second"] {
            let _registered = webhooks
                .register(
                    &key,
                    &format!("http://localhost:3000/{path}"),
                    Vec::new(),
                    now(),
                )
                .expect("registered");
        }

        let urls: Vec<String> = webhooks
            .for_key(&key)
            .expect("list")
            .into_iter()
            .map(|endpoint| endpoint.url)
            .collect();

        assert_eq!(
            urls,
            [
                "http://localhost:3000/first",
                "http://localhost:3000/second"
            ]
        );
    }

    #[test]
    fn should_reject_a_url_without_an_http_scheme() {
        let webhooks = Webhooks::new();

        for url in ["localhost:3000", "ftp://example.com", "file:///etc/passwd"] {
            assert!(
                matches!(
                    webhooks.register(
                        &key("re_should_reject_a_non_http_url"),
                        url,
                        Vec::new(),
                        now()
                    ),
                    Err(Error::Validation { .. })
                ),
                "{url} should be refused"
            );
        }
    }

    #[test]
    fn should_reject_a_blank_url() {
        let webhooks = Webhooks::new();

        assert!(matches!(
            webhooks.register(
                &key("re_should_reject_a_blank_url"),
                "   ",
                Vec::new(),
                now()
            ),
            Err(Error::Validation { .. })
        ));
    }

    #[test]
    fn should_give_each_endpoint_its_own_secret() {
        let webhooks = Webhooks::new();
        let key = key("re_should_give_each_endpoint_its_own_secret");

        let first = webhooks
            .register(&key, "http://localhost:3000/a", Vec::new(), now())
            .expect("registered");
        let second = webhooks
            .register(&key, "http://localhost:3000/b", Vec::new(), now())
            .expect("registered");

        assert_ne!(first.secret.as_str(), second.secret.as_str());
    }

    #[test]
    fn should_deliver_every_event_to_an_endpoint_that_names_none() {
        let webhooks = Webhooks::new();
        let key = key("re_should_deliver_everything_by_default");

        let _registered = webhooks
            .register(&key, "http://localhost:3000/hooks", Vec::new(), now())
            .expect("registered");

        assert_eq!(
            webhooks
                .subscribed_to(&key, EmailEvent::Delivered)
                .expect("read")
                .len(),
            1
        );
    }

    #[test]
    fn should_deliver_only_the_events_an_endpoint_named() {
        let webhooks = Webhooks::new();
        let key = key("re_should_deliver_only_named_events");

        let _registered = webhooks
            .register(
                &key,
                "http://localhost:3000/hooks",
                vec![EmailEvent::Bounced],
                now(),
            )
            .expect("registered");

        assert_eq!(
            webhooks
                .subscribed_to(&key, EmailEvent::Bounced)
                .expect("read")
                .len(),
            1
        );
        assert!(
            webhooks
                .subscribed_to(&key, EmailEvent::Delivered)
                .expect("read")
                .is_empty()
        );
    }

    #[test]
    fn should_never_deliver_an_event_resend_has_no_webhook_for() {
        let webhooks = Webhooks::new();
        let key = key("re_should_never_deliver_a_cancellation");

        let _registered = webhooks
            .register(
                &key,
                "http://localhost:3000/hooks",
                vec![EmailEvent::Canceled],
                now(),
            )
            .expect("registered");

        assert!(
            webhooks
                .subscribed_to(&key, EmailEvent::Canceled)
                .expect("read")
                .is_empty(),
            "Resend has no email.canceled, so subscribing to it cannot produce one"
        );
    }

    #[test]
    fn should_remove_one_endpoint_and_leave_the_rest() {
        let webhooks = Webhooks::new();
        let key = key("re_should_remove_one_endpoint");

        let first = webhooks
            .register(&key, "http://localhost:3000/a", Vec::new(), now())
            .expect("registered");
        let _second = webhooks
            .register(&key, "http://localhost:3000/b", Vec::new(), now())
            .expect("registered");

        webhooks.remove(&key, &first.id).expect("removed");

        let remaining = webhooks.for_key(&key).expect("list");

        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].url, "http://localhost:3000/b");
    }

    #[test]
    fn should_not_remove_an_endpoint_that_does_not_exist() {
        let webhooks = Webhooks::new();

        assert!(matches!(
            webhooks.remove(
                &key("re_should_not_remove_an_unknown_endpoint"),
                &WebhookId::generate()
            ),
            Err(Error::NotFound)
        ));
    }

    #[test]
    fn should_not_remove_another_keys_endpoint() {
        let webhooks = Webhooks::new();
        let mine = key("re_should_not_remove_another_keys_endpoint_mine");
        let theirs = key("re_should_not_remove_another_keys_endpoint_theirs");

        let endpoint = webhooks
            .register(&mine, "http://localhost:3000/hooks", Vec::new(), now())
            .expect("registered");

        assert!(matches!(
            webhooks.remove(&theirs, &endpoint.id),
            Err(Error::NotFound)
        ));
        assert_eq!(webhooks.for_key(&mine).expect("list").len(), 1);
    }

    #[test]
    fn should_keep_one_keys_endpoints_away_from_another() {
        let webhooks = Webhooks::new();
        let mine = key("re_should_keep_endpoints_separate_mine");
        let theirs = key("re_should_keep_endpoints_separate_theirs");

        let _registered = webhooks
            .register(&mine, "http://localhost:3000/hooks", Vec::new(), now())
            .expect("registered");

        assert!(
            webhooks.for_key(&theirs).expect("list").is_empty(),
            "one test's handler must not receive another's events"
        );
    }

    #[test]
    fn should_forget_only_the_key_it_was_asked_to_reset() {
        let webhooks = Webhooks::new();
        let reset = key("re_should_forget_only_the_reset_key_hooks");
        let kept = key("re_should_forget_only_the_kept_key_hooks");

        for key in [&reset, &kept] {
            let _registered = webhooks
                .register(key, "http://localhost:3000/hooks", Vec::new(), now())
                .expect("registered");
        }

        webhooks.reset(Some(&reset)).expect("resets");

        assert!(webhooks.for_key(&reset).expect("list").is_empty());
        assert_eq!(webhooks.for_key(&kept).expect("list").len(), 1);
    }
}

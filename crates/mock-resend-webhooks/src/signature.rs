//! Svix-compatible webhook signatures.
//!
//! Resend signs with Svix, so a caller's real verification code — the `svix` library,
//! or whatever they wrote against its documented scheme — has to accept what this
//! produces. That is the point of signing at all: stubbing verification out in tests is
//! exactly how a broken verifier reaches production.
//!
//! The scheme: HMAC-SHA256 over `{id}.{timestamp}.{body}`, with the key being the
//! base64-decoded part of the `whsec_` secret, and the result base64-encoded and
//! prefixed with the `v1,` version marker.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use hmac::{Hmac, Mac};
use mock_resend_core::SigningSecret;
use sha2::Sha256;

/// The signature version Svix uses for symmetric keys.
pub const VERSION: &str = "v1";

/// The `svix-id` header: the message's unique id.
pub const ID_HEADER: &str = "svix-id";

/// The `svix-timestamp` header: Unix seconds.
pub const TIMESTAMP_HEADER: &str = "svix-timestamp";

/// The `svix-signature` header: one or more space-separated `v1,<base64>` values.
pub const SIGNATURE_HEADER: &str = "svix-signature";

/// The three headers a signed delivery carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Headers {
    /// The value for `svix-id`.
    pub id: String,
    /// The value for `svix-timestamp`, in Unix seconds.
    pub timestamp: String,
    /// The value for `svix-signature`.
    pub signature: String,
}

/// Signs a payload the way Svix does.
///
/// `id` is the delivery's own id, not the email's: Svix identifies the *message*, and a
/// retry of the same delivery reuses it so a receiver can deduplicate.
///
/// # Errors
///
/// Propagates a secret whose base64 will not decode, which
/// [`SigningSecret::parse`](mock_resend_core::SigningSecret::parse) already rules out.
pub fn sign(
    secret: &SigningSecret,
    id: &str,
    timestamp: i64,
    body: &[u8],
) -> mock_resend_core::Result<Headers> {
    let key = secret.key_bytes()?;

    // `{id}.{timestamp}.{body}`, with the body appended as raw bytes rather than
    // re-encoded: a verifier signs the bytes it received, so anything that re-serialises
    // the JSON in between produces a different signature.
    let mut signed = Vec::with_capacity(id.len() + 24 + body.len());
    signed.extend_from_slice(id.as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(timestamp.to_string().as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(body);

    // `new_from_slice` only fails for a key length HMAC cannot take, and HMAC-SHA256
    // accepts any length, so this cannot fail in practice. It is still handled rather
    // than unwrapped, because nothing in this workspace panics outside tests.
    let mut mac = <Hmac<Sha256>>::new_from_slice(&key).map_err(|_| {
        mock_resend_core::Error::Store("the signing key was not usable for HMAC".to_owned())
    })?;

    mac.update(&signed);
    let digest = mac.finalize().into_bytes();

    Ok(Headers {
        id: id.to_owned(),
        timestamp: timestamp.to_string(),
        signature: format!("{VERSION},{}", BASE64.encode(digest)),
    })
}

#[cfg(test)]
mod sign {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use hmac::{Hmac, Mac};
    use mock_resend_core::SigningSecret;
    use sha2::Sha256;

    use super::{VERSION, sign};

    fn secret() -> SigningSecret {
        // A fixed secret, so the expected signature below is a constant rather than
        // something recomputed by the same code under test.
        SigningSecret::parse("whsec_dGhpcyBpcyBhIHRlc3Qgc2VjcmV0IGZvciBzaWduaW5n").expect("valid")
    }

    /// An independent implementation of the documented Svix scheme.
    ///
    /// Deliberately not a call into `sign`: a test that computes the expected value the
    /// same way as the code cannot catch a wrong scheme, only a wrong refactor.
    fn expected(secret: &SigningSecret, id: &str, timestamp: i64, body: &str) -> String {
        let key = secret.key_bytes().expect("decodes");
        let signed = format!("{id}.{timestamp}.{body}");

        let mut mac = <Hmac<Sha256>>::new_from_slice(&key).expect("any key length");
        mac.update(signed.as_bytes());

        format!("{VERSION},{}", BASE64.encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn should_sign_the_documented_svix_payload() {
        let body = r#"{"type":"email.sent"}"#;
        let headers = sign(&secret(), "msg_123", 1_700_000_000, body.as_bytes()).expect("signs");

        assert_eq!(
            headers.signature,
            expected(&secret(), "msg_123", 1_700_000_000, body)
        );
    }

    #[test]
    fn should_report_the_id_and_timestamp_it_signed() {
        let headers = sign(&secret(), "msg_123", 1_700_000_000, b"{}").expect("signs");

        assert_eq!(headers.id, "msg_123");
        assert_eq!(headers.timestamp, "1700000000");
    }

    #[test]
    fn should_prefix_the_signature_with_the_version() {
        let headers = sign(&secret(), "msg_123", 1, b"{}").expect("signs");

        assert!(headers.signature.starts_with("v1,"));
    }

    #[test]
    fn should_produce_a_different_signature_for_a_different_body() {
        let first = sign(&secret(), "msg_123", 1, b"{\"a\":1}").expect("signs");
        let second = sign(&secret(), "msg_123", 1, b"{\"a\":2}").expect("signs");

        assert_ne!(first.signature, second.signature);
    }

    #[test]
    fn should_produce_a_different_signature_for_a_different_timestamp() {
        let first = sign(&secret(), "msg_123", 1, b"{}").expect("signs");
        let second = sign(&secret(), "msg_123", 2, b"{}").expect("signs");

        assert_ne!(first.signature, second.signature);
    }

    #[test]
    fn should_produce_a_different_signature_for_a_different_id() {
        let first = sign(&secret(), "msg_123", 1, b"{}").expect("signs");
        let second = sign(&secret(), "msg_124", 1, b"{}").expect("signs");

        assert_ne!(first.signature, second.signature);
    }

    #[test]
    fn should_produce_a_different_signature_for_a_different_secret() {
        let other = SigningSecret::generate();

        let mine = sign(&secret(), "msg_123", 1, b"{}").expect("signs");
        let theirs = sign(&other, "msg_123", 1, b"{}").expect("signs");

        assert_ne!(mine.signature, theirs.signature);
    }

    #[test]
    fn should_sign_the_bytes_it_was_given() {
        // Two JSON documents that mean the same thing sign differently, which is the
        // behaviour a verifier depends on: it signs what arrived on the socket.
        let compact = sign(&secret(), "msg_123", 1, br#"{"a":1}"#).expect("signs");
        let spaced = sign(&secret(), "msg_123", 1, br#"{"a": 1}"#).expect("signs");

        assert_ne!(compact.signature, spaced.signature);
    }

    #[test]
    fn should_produce_a_signature_of_the_expected_length() {
        let headers = sign(&secret(), "msg_123", 1, b"{}").expect("signs");
        let encoded = headers
            .signature
            .strip_prefix("v1,")
            .expect("it has the version prefix");

        // SHA-256 is 32 bytes, which is 44 base64 characters with padding.
        assert_eq!(encoded.len(), 44);
        assert_eq!(BASE64.decode(encoded).expect("decodes").len(), 32);
    }
}

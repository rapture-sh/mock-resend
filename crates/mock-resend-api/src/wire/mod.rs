//! Request and response types, using Resend's field names verbatim.
//!
//! Never hand-format JSON in a handler, and never reach for `serde_json::json!` to
//! build a response that has a type.

pub mod emails;

use serde::Deserialize;

/// A field Resend accepts as either a bare string or an array of strings.
///
/// `to`, `cc`, `bcc` and `reply_to` are all like this, and getting it wrong is the
/// single most common way a mock diverges without anyone noticing: the Node SDK sends
/// `"to": "a@example.com"` for one recipient while the Rust SDK always sends an array,
/// so a mock that accepts only one form passes one suite and fails the other.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    /// A single address.
    One(String),
    /// Several addresses.
    Many(Vec<String>),
}

impl OneOrMany {
    /// Collapses both forms into a list, dropping blank entries.
    #[must_use]
    pub fn into_vec(self) -> Vec<String> {
        let addresses = match self {
            Self::One(address) => vec![address],
            Self::Many(addresses) => addresses,
        };

        addresses
            .into_iter()
            .filter(|address| !address.trim().is_empty())
            .collect()
    }
}

/// Collapses an optional string-or-array field into a list.
#[must_use]
pub fn addresses(field: Option<OneOrMany>) -> Vec<String> {
    field.map(OneOrMany::into_vec).unwrap_or_default()
}

#[cfg(test)]
mod one_or_many {
    use super::{OneOrMany, addresses};

    fn parsed(json: &str) -> Vec<String> {
        let field: Option<OneOrMany> =
            serde_json::from_str(json).expect("the fixture is valid json");

        addresses(field)
    }

    #[test]
    fn should_accept_a_single_recipient_as_a_bare_string() {
        assert_eq!(parsed(r#""one@example.org""#), ["one@example.org"]);
    }

    #[test]
    fn should_accept_recipients_as_an_array() {
        assert_eq!(
            parsed(r#"["one@example.org","two@example.org"]"#),
            ["one@example.org", "two@example.org"]
        );
    }

    #[test]
    fn should_accept_a_single_element_array() {
        assert_eq!(parsed(r#"["one@example.org"]"#), ["one@example.org"]);
    }

    #[test]
    fn should_treat_an_absent_field_as_empty() {
        let field: Option<OneOrMany> = None;

        assert!(addresses(field).is_empty());
    }

    #[test]
    fn should_treat_null_as_empty() {
        assert!(parsed("null").is_empty());
    }

    #[test]
    fn should_treat_an_empty_array_as_empty() {
        assert!(parsed("[]").is_empty());
    }

    #[test]
    fn should_drop_a_blank_entry() {
        assert_eq!(
            parsed(r#"["one@example.org","  ",""]"#),
            ["one@example.org"]
        );
    }
}

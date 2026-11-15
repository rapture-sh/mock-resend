//! Parsing an SMTP command line.
//!
//! Only the subset a mail client actually needs to hand over a message. Anything else
//! is answered with `502`, which is what a server that does not implement a command is
//! supposed to say.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

/// A command a client sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `EHLO <domain>`, which asks for the extension list.
    Ehlo(String),
    /// `HELO <domain>`, the older form, which does not.
    Helo(String),
    /// `AUTH PLAIN [credentials]`.
    AuthPlain(Option<String>),
    /// `AUTH LOGIN [username]`.
    AuthLogin(Option<String>),
    /// `MAIL FROM:<address>`.
    MailFrom(String),
    /// `RCPT TO:<address>`.
    RcptTo(String),
    /// `DATA`.
    Data,
    /// `RSET`, which abandons the transaction in progress.
    Rset,
    /// `NOOP`.
    Noop,
    /// `QUIT`.
    Quit,
    /// `VRFY`, which this server always answers vaguely: confirming whether an address
    /// exists is a feature only a spammer wants, and there are no addresses here anyway.
    Vrfy,
    /// Something recognised but not implemented.
    Unsupported(String),
}

impl Command {
    /// Parses a command line.
    ///
    /// Returns `None` for a line that is not a command at all, which the session
    /// answers with `500`.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();

        if line.is_empty() {
            return None;
        }

        // A verb is case-insensitive per RFC 5321, and clients really do vary.
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        let verb = verb.to_ascii_uppercase();

        Some(match verb.as_str() {
            "EHLO" => Self::Ehlo(rest.to_owned()),
            "HELO" => Self::Helo(rest.to_owned()),
            "MAIL" => Self::MailFrom(address_from(rest, "FROM")?),
            "RCPT" => Self::RcptTo(address_from(rest, "TO")?),
            "DATA" => Self::Data,
            "RSET" => Self::Rset,
            "NOOP" => Self::Noop,
            "QUIT" => Self::Quit,
            "VRFY" | "EXPN" => Self::Vrfy,
            "AUTH" => {
                let (mechanism, credentials) = rest.split_once(' ').unwrap_or((rest, ""));
                let credentials = credentials.trim();
                let credentials = (!credentials.is_empty()).then(|| credentials.to_owned());

                match mechanism.to_ascii_uppercase().as_str() {
                    "PLAIN" => Self::AuthPlain(credentials),
                    "LOGIN" => Self::AuthLogin(credentials),
                    other => Self::Unsupported(format!("AUTH {other}")),
                }
            }
            other => Self::Unsupported(other.to_owned()),
        })
    }
}

/// Pulls the address out of `FROM:<addr>` or `TO:<addr>`.
///
/// `MAIL FROM:<>` is the null sender, which is legal and arrives as an empty string.
/// Trailing `SIZE=` and similar parameters are ignored rather than refused: a client
/// that announces a size is not doing anything this server needs to act on.
fn address_from(rest: &str, keyword: &str) -> Option<String> {
    let rest = rest.trim();

    let after_keyword = rest
        .get(..keyword.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(keyword))
        .map(|_| &rest[keyword.len()..])?;

    let after_colon = after_keyword.trim_start().strip_prefix(':')?.trim_start();

    // Angle brackets are required by the grammar but plenty of clients omit them, and
    // every real server accepts both.
    let address = match (after_colon.find('<'), after_colon.find('>')) {
        (Some(open), Some(close)) if close > open => &after_colon[open + 1..close],
        _ => after_colon.split_whitespace().next().unwrap_or(""),
    };

    Some(address.trim().to_owned())
}

/// What `AUTH PLAIN` carried.
///
/// The payload is `\0username\0password`, base64-encoded. An authorisation identity in
/// the first field is accepted and ignored, which is what the mechanism allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainCredentials {
    /// The username.
    pub username: String,
    /// The password.
    pub password: String,
}

/// Decodes an `AUTH PLAIN` payload.
///
/// # Errors
///
/// `None` when the payload is not base64, or does not have the three NUL-separated
/// fields the mechanism requires.
#[must_use]
pub fn decode_plain(encoded: &str) -> Option<PlainCredentials> {
    let decoded = BASE64.decode(encoded.trim().as_bytes()).ok()?;
    let text = String::from_utf8(decoded).ok()?;

    let mut fields = text.split('\0');
    let _authorisation_identity = fields.next()?;
    let username = fields.next()?.to_owned();
    let password = fields.next()?.to_owned();

    // A fourth field means this is not an AUTH PLAIN payload.
    if fields.next().is_some() {
        return None;
    }

    Some(PlainCredentials { username, password })
}

/// Decodes one base64 field of an `AUTH LOGIN` exchange.
#[must_use]
pub fn decode_login_field(encoded: &str) -> Option<String> {
    let decoded = BASE64.decode(encoded.trim().as_bytes()).ok()?;

    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod parse {
    use super::Command;

    #[test]
    fn should_parse_ehlo_with_its_domain() {
        assert_eq!(
            Command::parse("EHLO mail.example.com"),
            Some(Command::Ehlo("mail.example.com".to_owned()))
        );
    }

    #[test]
    fn should_treat_a_verb_as_case_insensitive() {
        for line in ["quit", "Quit", "QUIT"] {
            assert_eq!(Command::parse(line), Some(Command::Quit), "{line}");
        }
    }

    #[test]
    fn should_parse_mail_from_with_angle_brackets() {
        assert_eq!(
            Command::parse("MAIL FROM:<sender@example.com>"),
            Some(Command::MailFrom("sender@example.com".to_owned()))
        );
    }

    #[test]
    fn should_parse_mail_from_without_angle_brackets() {
        // Not strictly legal, and accepted by every real server.
        assert_eq!(
            Command::parse("MAIL FROM: sender@example.com"),
            Some(Command::MailFrom("sender@example.com".to_owned()))
        );
    }

    #[test]
    fn should_parse_the_null_sender() {
        assert_eq!(
            Command::parse("MAIL FROM:<>"),
            Some(Command::MailFrom(String::new()))
        );
    }

    #[test]
    fn should_ignore_trailing_mail_parameters() {
        assert_eq!(
            Command::parse("MAIL FROM:<a@example.com> SIZE=1024 BODY=8BITMIME"),
            Some(Command::MailFrom("a@example.com".to_owned()))
        );
    }

    #[test]
    fn should_parse_rcpt_to() {
        assert_eq!(
            Command::parse("RCPT TO:<recipient@example.org>"),
            Some(Command::RcptTo("recipient@example.org".to_owned()))
        );
    }

    #[test]
    fn should_parse_rcpt_to_with_odd_spacing() {
        assert_eq!(
            Command::parse("RCPT   TO :  <recipient@example.org>"),
            Some(Command::RcptTo("recipient@example.org".to_owned()))
        );
    }

    #[test]
    fn should_not_parse_mail_without_the_from_keyword() {
        assert_eq!(Command::parse("MAIL sender@example.com"), None);
    }

    #[test]
    fn should_parse_auth_plain_with_inline_credentials() {
        assert_eq!(
            Command::parse("AUTH PLAIN dGVzdA=="),
            Some(Command::AuthPlain(Some("dGVzdA==".to_owned())))
        );
    }

    #[test]
    fn should_parse_auth_plain_awaiting_credentials() {
        assert_eq!(Command::parse("AUTH PLAIN"), Some(Command::AuthPlain(None)));
    }

    #[test]
    fn should_parse_auth_login() {
        assert_eq!(Command::parse("AUTH LOGIN"), Some(Command::AuthLogin(None)));
    }

    #[test]
    fn should_report_an_unsupported_auth_mechanism() {
        assert_eq!(
            Command::parse("AUTH CRAM-MD5"),
            Some(Command::Unsupported("AUTH CRAM-MD5".to_owned()))
        );
    }

    #[test]
    fn should_report_an_unsupported_verb() {
        assert_eq!(
            Command::parse("STARTTLS"),
            Some(Command::Unsupported("STARTTLS".to_owned()))
        );
    }

    #[test]
    fn should_answer_address_verification_vaguely() {
        // Confirming whether an address exists is a feature only a spammer wants.
        assert_eq!(Command::parse("VRFY postmaster"), Some(Command::Vrfy));
        assert_eq!(Command::parse("EXPN staff"), Some(Command::Vrfy));
    }

    #[test]
    fn should_not_parse_an_empty_line() {
        assert_eq!(Command::parse(""), None);
        assert_eq!(Command::parse("   "), None);
    }
}

#[cfg(test)]
mod credentials {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;

    use super::{decode_login_field, decode_plain};

    fn plain(payload: &str) -> String {
        BASE64.encode(payload.as_bytes())
    }

    #[test]
    fn should_decode_an_auth_plain_payload() {
        let decoded =
            decode_plain(&plain("\0resend\0re_test")).expect("the payload has all three fields");

        assert_eq!(decoded.username, "resend");
        assert_eq!(decoded.password, "re_test");
    }

    #[test]
    fn should_ignore_an_authorisation_identity() {
        let decoded = decode_plain(&plain("someone\0resend\0re_test")).expect("decodes");

        assert_eq!(decoded.username, "resend");
    }

    #[test]
    fn should_keep_a_password_containing_unusual_characters() {
        let decoded = decode_plain(&plain("\0resend\0re_a-b_c.1")).expect("decodes");

        assert_eq!(decoded.password, "re_a-b_c.1");
    }

    #[test]
    fn should_not_decode_a_payload_with_too_few_fields() {
        assert!(decode_plain(&plain("resend\0re_test")).is_none());
    }

    #[test]
    fn should_not_decode_a_payload_with_too_many_fields() {
        assert!(decode_plain(&plain("\0a\0b\0c")).is_none());
    }

    #[test]
    fn should_not_decode_something_that_is_not_base64() {
        assert!(decode_plain("not base64!!").is_none());
    }

    #[test]
    fn should_not_decode_base64_that_is_not_utf8() {
        assert!(decode_plain(&BASE64.encode([0_u8, 0xff, 0, 0xfe])).is_none());
    }

    #[test]
    fn should_decode_a_login_field() {
        assert_eq!(
            decode_login_field(&plain("resend")).as_deref(),
            Some("resend")
        );
    }

    #[test]
    fn should_not_decode_a_login_field_that_is_not_base64() {
        assert!(decode_login_field("not base64!!").is_none());
    }
}

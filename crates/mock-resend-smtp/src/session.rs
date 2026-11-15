//! One SMTP conversation.
//!
//! A second way into the same engine, not a second mailbox. A message that arrives here
//! is retrievable through `GET /emails/{id}` and produces the same events as one sent
//! through the API, because it goes through the same engine method.
//!
//! Everything in this file reads input a stranger controls, so: no `unwrap`, every read
//! bounded, every buffer capped, and an idle connection dropped rather than held.

use std::time::Duration;

use mock_resend_core::{ApiKey, Engine, Envelope, mime};
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

use crate::command::{Command, decode_login_field, decode_plain};
use crate::line::{ReadError, read_line};

/// The username a client has to send. The password is the API key.
pub const USERNAME: &str = "resend";

/// How the listener behaves.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// The longest command line accepted, including its terminator.
    ///
    /// RFC 5321 sets 512 for a command; this is generous so a long `MAIL FROM` with
    /// parameters is not refused, while still bounded.
    pub max_command_bytes: usize,
    /// The longest line accepted inside `DATA`.
    ///
    /// RFC 5321 sets 1000. Real mail breaks it constantly, so this is large enough not
    /// to reject a message a client actually sent.
    pub max_data_line_bytes: usize,
    /// The largest message accepted, in bytes.
    pub max_message_bytes: usize,
    /// The most recipients one transaction may name. Resend's own limit is 50.
    pub max_recipients: usize,
    /// How long a connection may sit idle before it is dropped.
    pub idle_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_command_bytes: 4 * 1024,
            max_data_line_bytes: 64 * 1024,
            // Resend's own attachment ceiling is 40MB per email, and a MIME body is
            // base64 so it is larger than the file it carries.
            max_message_bytes: 60 * 1024 * 1024,
            max_recipients: 50,
            idle_timeout: Duration::from_secs(120),
        }
    }
}

/// What the session is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expecting {
    /// An ordinary command.
    Command,
    /// The base64 payload of an `AUTH PLAIN` that arrived without one.
    PlainCredentials,
    /// The base64 username of an `AUTH LOGIN`.
    LoginUsername,
    /// The base64 password of an `AUTH LOGIN`, with the username already in hand.
    LoginPassword(String),
}

/// The transaction being built up.
#[derive(Debug, Default)]
struct Transaction {
    sender: Option<String>,
    recipients: Vec<String>,
}

impl Transaction {
    fn reset(&mut self) {
        self.sender = None;
        self.recipients.clear();
    }
}

/// Runs one conversation to completion.
///
/// Returns when the client quits, the connection closes, or the peer does something
/// that means it is not speaking SMTP.
///
/// # Errors
///
/// Only a write failure, which means the connection has gone.
pub async fn run<R, W>(
    engine: Engine,
    config: Config,
    reader: &mut R,
    writer: &mut W,
) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    reply(
        writer,
        "220 mock-resend ESMTP ready; nothing here is delivered",
    )
    .await?;

    let mut buffer = Vec::new();
    let mut expecting = Expecting::Command;
    let mut authenticated: Option<ApiKey> = None;
    let mut transaction = Transaction::default();
    let mut greeted = false;

    loop {
        let read = timeout(
            config.idle_timeout,
            read_line(reader, &mut buffer, config.max_command_bytes),
        )
        .await;

        let line = match read {
            Ok(Ok(())) => String::from_utf8_lossy(&buffer).into_owned(),
            Ok(Err(ReadError::Closed)) => return Ok(()),
            Ok(Err(ReadError::TooLong)) => {
                reply(writer, "500 Line too long").await?;
                return Ok(());
            }
            Ok(Err(ReadError::Io(_))) => return Ok(()),
            Err(_elapsed) => {
                reply(writer, "421 Timeout waiting for a command").await?;
                return Ok(());
            }
        };

        // The credential exchanges are not commands, so they are handled before the
        // command parser ever sees the line.
        match std::mem::replace(&mut expecting, Expecting::Command) {
            Expecting::PlainCredentials => {
                authenticate_plain(writer, &line, &mut authenticated).await?;
                continue;
            }
            Expecting::LoginUsername => {
                match decode_login_field(&line) {
                    Some(username) => {
                        expecting = Expecting::LoginPassword(username);
                        // `UGFzc3dvcmQ6` is base64 for `Password:`, which is what the
                        // mechanism expects to see as the prompt.
                        reply(writer, "334 UGFzc3dvcmQ6").await?;
                    }
                    None => reply(writer, "501 Could not decode the username").await?,
                }
                continue;
            }
            Expecting::LoginPassword(username) => {
                match decode_login_field(&line) {
                    Some(password) => {
                        accept_credentials(writer, &username, &password, &mut authenticated)
                            .await?;
                    }
                    None => reply(writer, "501 Could not decode the password").await?,
                }
                continue;
            }
            Expecting::Command => {}
        }

        let Some(command) = Command::parse(&line) else {
            reply(writer, "500 Command not recognised").await?;
            continue;
        };

        match command {
            Command::Ehlo(_) => {
                greeted = true;
                // No STARTTLS. Its absence is deliberate: see SECURITY.md.
                writer
                    .write_all(
                        b"250-mock-resend\r\n250-AUTH PLAIN LOGIN\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n",
                    )
                    .await?;
                writer.flush().await?;
            }
            Command::Helo(_) => {
                greeted = true;
                reply(writer, "250 mock-resend").await?;
            }
            Command::AuthPlain(credentials) => {
                if authenticated.is_some() {
                    reply(writer, "503 Already authenticated").await?;
                    continue;
                }

                match credentials {
                    Some(credentials) => {
                        authenticate_plain(writer, &credentials, &mut authenticated).await?;
                    }
                    None => {
                        expecting = Expecting::PlainCredentials;
                        reply(writer, "334 ").await?;
                    }
                }
            }
            Command::AuthLogin(username) => {
                if authenticated.is_some() {
                    reply(writer, "503 Already authenticated").await?;
                    continue;
                }

                match username.as_deref().and_then(decode_login_field) {
                    Some(username) => {
                        expecting = Expecting::LoginPassword(username);
                        reply(writer, "334 UGFzc3dvcmQ6").await?;
                    }
                    None => {
                        expecting = Expecting::LoginUsername;
                        // `VXNlcm5hbWU6` is base64 for `Username:`.
                        reply(writer, "334 VXNlcm5hbWU6").await?;
                    }
                }
            }
            Command::MailFrom(sender) => {
                if !greeted {
                    reply(writer, "503 Send EHLO first").await?;
                    continue;
                }
                if authenticated.is_none() {
                    reply(writer, "530 Authentication required").await?;
                    continue;
                }

                transaction.reset();
                transaction.sender = Some(sender);
                reply(writer, "250 Sender accepted").await?;
            }
            Command::RcptTo(recipient) => {
                if transaction.sender.is_none() {
                    reply(writer, "503 Send MAIL FROM first").await?;
                    continue;
                }
                if transaction.recipients.len() >= config.max_recipients {
                    reply(
                        writer,
                        "452 Too many recipients; Resend allows at most 50 per message",
                    )
                    .await?;
                    continue;
                }
                if recipient.is_empty() {
                    reply(writer, "501 A recipient address is required").await?;
                    continue;
                }

                transaction.recipients.push(recipient);
                reply(writer, "250 Recipient accepted").await?;
            }
            Command::Data => {
                let Some(key) = authenticated.clone() else {
                    reply(writer, "530 Authentication required").await?;
                    continue;
                };
                if transaction.sender.is_none() || transaction.recipients.is_empty() {
                    reply(writer, "503 Send MAIL FROM and RCPT TO first").await?;
                    continue;
                }

                reply(writer, "354 Go ahead; end with <CRLF>.<CRLF>").await?;

                match read_data(reader, &mut buffer, &config).await {
                    Ok(raw) => {
                        let envelope = Envelope {
                            sender: transaction.sender.clone().unwrap_or_default(),
                            recipients: transaction.recipients.clone(),
                        };

                        match capture(&engine, &key, &raw, &envelope).await {
                            Ok(id) => {
                                transaction.reset();
                                reply(writer, &format!("250 Captured as {id}")).await?;
                            }
                            Err(error) => {
                                transaction.reset();
                                // 554 rather than 4xx: the message itself is wrong, so
                                // retrying it unchanged will not help.
                                reply(writer, &format!("554 {error}")).await?;
                            }
                        }
                    }
                    Err(DataError::TooLarge) => {
                        transaction.reset();
                        reply(writer, "552 Message too large").await?;
                        return Ok(());
                    }
                    Err(DataError::LineTooLong) => {
                        transaction.reset();
                        reply(writer, "500 Line too long").await?;
                        return Ok(());
                    }
                    Err(DataError::Closed) => return Ok(()),
                }
            }
            Command::Rset => {
                transaction.reset();
                reply(writer, "250 Transaction abandoned").await?;
            }
            Command::Noop => reply(writer, "250 OK").await?,
            Command::Quit => {
                reply(writer, "221 Goodbye").await?;
                return Ok(());
            }
            // Deliberately uninformative. There are no real addresses here, and
            // confirming whether one exists is a feature only a spammer wants.
            Command::Vrfy => {
                reply(writer, "252 Cannot verify, but will accept the message").await?;
            }
            Command::Unsupported(verb) => {
                reply(writer, &format!("502 {verb} is not implemented")).await?;
            }
        }
    }
}

/// Why `DATA` could not be read.
enum DataError {
    /// The message went over the size limit.
    TooLarge,
    /// A single line went over the limit.
    LineTooLong,
    /// The connection closed mid-message.
    Closed,
}

/// Reads the body of a `DATA` command, up to the lone `.` that ends it.
async fn read_data<R>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    config: &Config,
) -> Result<Vec<u8>, DataError>
where
    R: AsyncBufRead + Unpin,
{
    let mut raw = Vec::new();

    loop {
        match timeout(
            config.idle_timeout,
            read_line(reader, buffer, config.max_data_line_bytes),
        )
        .await
        {
            Ok(Ok(())) => {}
            // An overlong line means the peer is not following the protocol, so the
            // connection ends — but it ends with an answer. A dropped socket and a
            // refusal look identical from the client's side, and only one of them tells
            // the developer what they did.
            Ok(Err(ReadError::TooLong)) => return Err(DataError::LineTooLong),
            Ok(Err(_)) | Err(_) => return Err(DataError::Closed),
        }

        if buffer.as_slice() == b"." {
            return Ok(raw);
        }

        // Transparency, per RFC 5321 §4.5.2: a line the client sent starting with `.`
        // arrives with an extra one, and leaving it in corrupts the message.
        let line = match buffer.first() {
            Some(b'.') => &buffer[1..],
            _ => buffer.as_slice(),
        };

        if raw.len() + line.len() + 2 > config.max_message_bytes {
            return Err(DataError::TooLarge);
        }

        // Lines are joined by CRLF rather than each being followed by one, so the body
        // does not end with a trailing newline. The terminator is `<CRLF>.<CRLF>`, and
        // treating its leading CRLF as the last line's terminator rather than as body is
        // what makes `text: "It works"` through the API and `It works` over SMTP capture
        // as the same message. SMTP is a second way into the same engine, so it has to
        // agree with the other way in.
        if !raw.is_empty() {
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(line);
    }
}

/// Hands a received message to the engine.
async fn capture(
    engine: &Engine,
    key: &ApiKey,
    raw: &[u8],
    envelope: &Envelope,
) -> mock_resend_core::Result<mock_resend_core::EmailId> {
    let request = mime::inbound_to_send(raw, envelope)?;

    // The same method an API send uses, so the same validation runs and the same events
    // come out. A rule that lived in a handler would not reach here.
    engine.send_email(key, request).await
}

/// Checks an `AUTH PLAIN` payload.
async fn authenticate_plain<W>(
    writer: &mut W,
    encoded: &str,
    authenticated: &mut Option<ApiKey>,
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    match decode_plain(encoded) {
        Some(credentials) => {
            accept_credentials(
                writer,
                &credentials.username,
                &credentials.password,
                authenticated,
            )
            .await
        }
        None => reply(writer, "501 Could not decode the credentials").await,
    }
}

/// Accepts a username and password, or refuses them.
///
/// Nothing is authenticated: the username is fixed so a misconfigured client gets a
/// clear answer, and the password is the API key, which is the namespace the message
/// will be captured in rather than a secret.
async fn accept_credentials<W>(
    writer: &mut W,
    username: &str,
    password: &str,
    authenticated: &mut Option<ApiKey>,
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    if !username.eq_ignore_ascii_case(USERNAME) {
        return reply(
            writer,
            &format!("535 The username is `{USERNAME}`; the password is your API key"),
        )
        .await;
    }

    match ApiKey::parse(password) {
        Ok(key) => {
            *authenticated = Some(key);
            reply(writer, "235 Authenticated").await
        }
        Err(_) => {
            reply(
                writer,
                "535 The password has to be a `re_`-prefixed API key",
            )
            .await
        }
    }
}

/// Writes one reply line.
async fn reply<W>(writer: &mut W, line: &str) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\r\n").await?;
    writer.flush().await
}

#[cfg(test)]
mod run {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use mock_resend_core::{ApiKey, Engine};
    use tokio::io::BufReader;

    use super::{Config, run};

    /// Drives a whole conversation from a script and returns what the server wrote.
    ///
    /// No socket: the session is generic over the stream, so a byte slice in and a
    /// `Vec` out is the whole harness. The script ends and the session sees end of
    /// stream, which is what a client hanging up looks like.
    async fn converse(engine: &Engine, config: Config, script: &[&str]) -> String {
        let input = script.join("\r\n") + "\r\n";
        let mut reader = BufReader::new(input.as_bytes());
        let mut writer: Vec<u8> = Vec::new();

        run(engine.clone(), config, &mut reader, &mut writer)
            .await
            .expect("writing to a Vec cannot fail");

        String::from_utf8_lossy(&writer).into_owned()
    }

    /// The reply code of every reply, with multi-line continuations skipped.
    fn codes(conversation: &str) -> Vec<&str> {
        conversation
            .lines()
            .filter(|line| line.as_bytes().get(3) != Some(&b'-'))
            .filter_map(|line| line.get(..3))
            .collect()
    }

    fn auth_plain(key: &str) -> String {
        BASE64.encode(format!("\0resend\0{key}"))
    }

    fn key(name: &str) -> ApiKey {
        ApiKey::parse(name).expect("the test key is re_-prefixed")
    }

    async fn captured(engine: &Engine, name: &str) -> usize {
        engine
            .list_emails(&key(name))
            .await
            .expect("the in-memory store lists")
            .len()
    }

    #[tokio::test]
    async fn should_refuse_a_second_authentication_on_one_connection() {
        let name = "re_should_refuse_a_second_authentication";
        let engine = Engine::in_memory();

        let conversation = converse(
            &engine,
            Config::default(),
            &[
                "EHLO test.example.com",
                &format!("AUTH PLAIN {}", auth_plain(name)),
                &format!("AUTH PLAIN {}", auth_plain(name)),
            ],
        )
        .await;

        assert_eq!(
            codes(&conversation),
            ["220", "250", "235", "503"],
            "a key is chosen once per connection, so the second attempt is out of order"
        );
    }

    #[tokio::test]
    async fn should_refuse_a_message_over_the_size_limit() {
        let name = "re_should_refuse_an_oversized_message";
        let engine = Engine::in_memory();
        let config = Config {
            max_message_bytes: 128,
            ..Config::default()
        };

        let conversation = converse(
            &engine,
            config,
            &[
                "EHLO test.example.com",
                &format!("AUTH PLAIN {}", auth_plain(name)),
                "MAIL FROM:<a@example.com>",
                "RCPT TO:<b@example.org>",
                "DATA",
                "From: a@example.com",
                "To: b@example.org",
                "Subject: Too big",
                "",
                &"x".repeat(200),
                ".",
                "QUIT",
            ],
        )
        .await;

        assert_eq!(
            codes(&conversation).last(),
            Some(&"552"),
            "the connection ends on the refusal, so the QUIT is never answered"
        );
        assert_eq!(captured(&engine, name).await, 0);
    }

    #[tokio::test]
    async fn should_refuse_an_overlong_data_line_and_say_so() {
        let name = "re_should_refuse_an_overlong_data_line";
        let engine = Engine::in_memory();
        let config = Config {
            max_data_line_bytes: 64,
            ..Config::default()
        };

        let conversation = converse(
            &engine,
            config,
            &[
                "EHLO test.example.com",
                &format!("AUTH PLAIN {}", auth_plain(name)),
                "MAIL FROM:<a@example.com>",
                "RCPT TO:<b@example.org>",
                "DATA",
                &"x".repeat(200),
                ".",
                "QUIT",
            ],
        )
        .await;

        // The connection still ends — the peer is not following the protocol — but it
        // ends with an answer. A dropped socket and a refusal look identical from the
        // client's side, and only one of them tells the developer what they did.
        assert_eq!(codes(&conversation).last(), Some(&"500"));
        assert_eq!(captured(&engine, name).await, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn should_drop_an_idle_connection_rather_than_holding_it() {
        // The clock is the runtime's, advanced by the runtime because nothing is ready
        // to run. Waiting out two minutes of wall clock would be the same test, slowly.
        let (client, server) = tokio::io::duplex(64);
        let mut reader = BufReader::new(server);
        let mut writer: Vec<u8> = Vec::new();

        run(
            Engine::in_memory(),
            Config::default(),
            &mut reader,
            &mut writer,
        )
        .await
        .expect("writing to a Vec cannot fail");

        let conversation = String::from_utf8_lossy(&writer).into_owned();

        assert_eq!(codes(&conversation), ["220", "421"]);

        drop(client);
    }

    #[tokio::test]
    async fn should_refuse_mail_from_before_a_greeting() {
        let name = "re_should_refuse_mail_from_before_a_greeting";
        let engine = Engine::in_memory();

        let conversation = converse(
            &engine,
            Config::default(),
            &[
                &format!("AUTH PLAIN {}", auth_plain(name)),
                "MAIL FROM:<a@example.com>",
            ],
        )
        .await;

        assert_eq!(codes(&conversation), ["220", "235", "503"]);
    }

    #[tokio::test]
    async fn should_accept_the_older_helo_greeting() {
        let name = "re_should_accept_the_helo_greeting";
        let engine = Engine::in_memory();

        let conversation = converse(
            &engine,
            Config::default(),
            &[
                "HELO test.example.com",
                &format!("AUTH PLAIN {}", auth_plain(name)),
                "MAIL FROM:<a@example.com>",
                "RCPT TO:<b@example.org>",
                "DATA",
                "From: a@example.com",
                "To: b@example.org",
                "Subject: Older client",
                "",
                "It works",
                ".",
                "QUIT",
            ],
        )
        .await;

        assert_eq!(codes(&conversation).last(), Some(&"221"));
        assert_eq!(captured(&engine, name).await, 1);
    }

    #[tokio::test]
    async fn should_refuse_data_before_a_recipient() {
        let name = "re_should_refuse_data_before_a_recipient";
        let engine = Engine::in_memory();

        let conversation = converse(
            &engine,
            Config::default(),
            &[
                "EHLO test.example.com",
                &format!("AUTH PLAIN {}", auth_plain(name)),
                "MAIL FROM:<a@example.com>",
                "DATA",
            ],
        )
        .await;

        assert_eq!(codes(&conversation), ["220", "250", "235", "250", "503"]);
    }

    #[tokio::test]
    async fn should_refuse_an_empty_recipient_address() {
        let name = "re_should_refuse_an_empty_recipient";
        let engine = Engine::in_memory();

        let conversation = converse(
            &engine,
            Config::default(),
            &[
                "EHLO test.example.com",
                &format!("AUTH PLAIN {}", auth_plain(name)),
                "MAIL FROM:<a@example.com>",
                "RCPT TO:<>",
            ],
        )
        .await;

        assert_eq!(codes(&conversation).last(), Some(&"501"));
    }

    #[tokio::test]
    async fn should_not_recognise_a_mail_command_without_an_address() {
        let name = "re_should_not_recognise_a_bare_mail";
        let engine = Engine::in_memory();

        let conversation = converse(
            &engine,
            Config::default(),
            &[
                "EHLO test.example.com",
                &format!("AUTH PLAIN {}", auth_plain(name)),
                "MAIL",
            ],
        )
        .await;

        assert_eq!(codes(&conversation).last(), Some(&"500"));
    }

    #[tokio::test]
    async fn should_report_a_message_the_engine_refuses_as_permanent() {
        let name = "re_should_report_an_invalid_message";
        let engine = Engine::in_memory();

        // The null sender and no `From` header, so the engine has no sender to record.
        // 554 rather than a 4xx: retrying this unchanged cannot help.
        let conversation = converse(
            &engine,
            Config::default(),
            &[
                "EHLO test.example.com",
                &format!("AUTH PLAIN {}", auth_plain(name)),
                "MAIL FROM:<>",
                "RCPT TO:<b@example.org>",
                "DATA",
                "Subject: No sender",
                "",
                "It works",
                ".",
            ],
        )
        .await;

        assert_eq!(codes(&conversation).last(), Some(&"554"));
        assert_eq!(captured(&engine, name).await, 0);
    }

    #[tokio::test]
    async fn should_accept_a_message_after_one_the_engine_refused() {
        let name = "re_should_accept_after_a_refusal";
        let engine = Engine::in_memory();

        let conversation = converse(
            &engine,
            Config::default(),
            &[
                "EHLO test.example.com",
                &format!("AUTH PLAIN {}", auth_plain(name)),
                "MAIL FROM:<>",
                "RCPT TO:<b@example.org>",
                "DATA",
                "Subject: No sender",
                "",
                "It works",
                ".",
                "MAIL FROM:<a@example.com>",
                "RCPT TO:<b@example.org>",
                "DATA",
                "From: a@example.com",
                "To: b@example.org",
                "Subject: Second try",
                "",
                "It works",
                ".",
            ],
        )
        .await;

        assert_eq!(codes(&conversation).last(), Some(&"250"));
        assert_eq!(
            captured(&engine, name).await,
            1,
            "a refused message leaves the session usable, with nothing of it kept"
        );
    }

    #[tokio::test]
    async fn should_treat_the_line_after_auth_plain_as_credentials_and_not_as_a_command() {
        let engine = Engine::in_memory();

        // `QUIT` arrives where the base64 payload belongs, so it is credentials that
        // cannot be decoded rather than a command. A session that parsed it as one
        // would hang up on a client that is merely confused.
        let conversation = converse(
            &engine,
            Config::default(),
            &["EHLO test.example.com", "AUTH PLAIN", "QUIT", "NOOP"],
        )
        .await;

        assert_eq!(codes(&conversation), ["220", "250", "334", "501", "250"]);
    }

    #[tokio::test]
    async fn should_refuse_a_login_password_that_is_not_an_api_key() {
        let engine = Engine::in_memory();

        let conversation = converse(
            &engine,
            Config::default(),
            &[
                "EHLO test.example.com",
                "AUTH LOGIN",
                &BASE64.encode("resend"),
                &BASE64.encode("hunter2"),
            ],
        )
        .await;

        assert_eq!(codes(&conversation), ["220", "250", "334", "334", "535"]);
    }

    #[tokio::test]
    async fn should_report_a_login_field_that_is_not_base64() {
        let engine = Engine::in_memory();

        let conversation = converse(
            &engine,
            Config::default(),
            &["EHLO test.example.com", "AUTH LOGIN", "not base64 at all"],
        )
        .await;

        assert_eq!(codes(&conversation).last(), Some(&"501"));
    }
}

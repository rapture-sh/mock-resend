//! The SMTP listener, driven over a real socket.
//!
//! The point of these is that a message arriving over SMTP lands in the *same* mailbox:
//! every one of them captures over port 1025 and then reads the result back through the
//! Resend SDK.

mod support;

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use resend_rs::types::EmailEvent;
use support::client_for;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

/// A plain SMTP client, hand-rolled so the test speaks the protocol rather than a
/// library's idea of it.
struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    async fn connect(address: std::net::SocketAddr) -> Self {
        let stream = TcpStream::connect(address)
            .await
            .expect("the listener accepts");
        let (read, writer) = stream.into_split();

        let mut client = Self {
            reader: BufReader::new(read),
            writer,
        };

        assert!(
            client.read_reply().await.starts_with("220"),
            "the server greets first"
        );

        client
    }

    /// Reads one reply, following any multi-line continuation.
    async fn read_reply(&mut self) -> String {
        let mut collected = String::new();

        loop {
            let mut line = String::new();

            let read =
                tokio::time::timeout(Duration::from_secs(5), self.reader.read_line(&mut line))
                    .await
                    .expect("the server answers in time")
                    .expect("the socket is readable");

            if read == 0 {
                break;
            }

            collected.push_str(&line);

            // `250-` means another line follows; `250 ` is the last one.
            let continues = line.len() > 3 && line.as_bytes().get(3) == Some(&b'-');
            if !continues {
                break;
            }
        }

        collected
    }

    async fn send(&mut self, line: &str) -> String {
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .expect("the socket is writable");
        self.writer.flush().await.expect("flushes");

        self.read_reply().await
    }

    /// Sends the body of a `DATA` command and the terminating dot.
    async fn send_data(&mut self, body: &str) -> String {
        self.writer
            .write_all(body.as_bytes())
            .await
            .expect("writable");
        self.writer.write_all(b"\r\n.\r\n").await.expect("writable");
        self.writer.flush().await.expect("flushes");

        self.read_reply().await
    }

    /// `EHLO` plus `AUTH PLAIN` with the given key.
    async fn authenticate(&mut self, key: &str) -> String {
        let _greeting = self.send("EHLO test.example.com").await;

        let payload = BASE64.encode(format!("\0resend\0{key}"));

        self.send(&format!("AUTH PLAIN {payload}")).await
    }
}

/// The id out of a `250 Captured as <id>` reply.
fn captured_id(reply: &str) -> String {
    reply
        .trim()
        .rsplit(' ')
        .next()
        .expect("the reply names the id")
        .to_owned()
}

#[tokio::test]
async fn should_greet_and_advertise_auth() {
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let mut client = Client::connect(server.smtp_address()).await;

    let reply = client.send("EHLO test.example.com").await;

    assert!(reply.contains("AUTH PLAIN LOGIN"));
    assert!(
        !reply.contains("STARTTLS"),
        "there is no TLS here, and advertising it would be a lie a client acts on"
    );
}

#[tokio::test]
async fn should_capture_a_message_into_the_same_mailbox_the_api_reads() {
    let name = "re_should_capture_over_smtp";
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let api = client_for(&server, name);

    let mut client = Client::connect(server.smtp_address()).await;
    assert!(client.authenticate(name).await.starts_with("235"));
    assert!(
        client
            .send("MAIL FROM:<onboarding@example.com>")
            .await
            .starts_with("250")
    );
    assert!(
        client
            .send("RCPT TO:<delivered@example.org>")
            .await
            .starts_with("250")
    );
    assert!(client.send("DATA").await.starts_with("354"));

    let reply = client
        .send_data(
            "From: Acme <onboarding@example.com>\r\n\
             To: delivered@example.org\r\n\
             Subject: Over SMTP\r\n\
             \r\n\
             It works",
        )
        .await;

    assert!(reply.starts_with("250"), "got {reply}");

    // The whole point: read it back through the Resend SDK.
    let retrieved = api
        .emails
        .get(&captured_id(&reply))
        .await
        .expect("the SDK retrieves a message that arrived over SMTP");

    assert_eq!(retrieved.subject, "Over SMTP");
    assert_eq!(retrieved.from, "Acme <onboarding@example.com>");
    assert_eq!(retrieved.to, ["delivered@example.org"]);
    assert_eq!(retrieved.text.as_deref(), Some("It works"));
    assert_eq!(retrieved.last_event, EmailEvent::Sent);
}

#[tokio::test]
async fn should_capture_an_html_message_with_its_body_intact() {
    let name = "re_should_capture_html_over_smtp";
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let api = client_for(&server, name);

    let mut client = Client::connect(server.smtp_address()).await;
    let _authenticated = client.authenticate(name).await;
    let _from = client.send("MAIL FROM:<a@example.com>").await;
    let _to = client.send("RCPT TO:<b@example.org>").await;
    let _data = client.send("DATA").await;

    let reply = client
        .send_data(
            "From: a@example.com\r\n\
             To: b@example.org\r\n\
             Subject: HTML\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             \r\n\
             <p>It works</p>",
        )
        .await;

    let retrieved = api
        .emails
        .get(&captured_id(&reply))
        .await
        .expect("retrieved");

    assert_eq!(retrieved.html.as_deref(), Some("<p>It works</p>"));
}

#[tokio::test]
async fn should_reconstruct_a_bcc_recipient_from_the_envelope() {
    let name = "re_should_reconstruct_bcc_over_smtp";
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let api = client_for(&server, name);

    let mut client = Client::connect(server.smtp_address()).await;
    let _authenticated = client.authenticate(name).await;
    let _from = client.send("MAIL FROM:<a@example.com>").await;
    let _to = client.send("RCPT TO:<visible@example.org>").await;
    // In the envelope and in no header, which is exactly what a blind copy is.
    let _bcc = client.send("RCPT TO:<blind@example.org>").await;
    let _data = client.send("DATA").await;

    let reply = client
        .send_data(
            "From: a@example.com\r\n\
             To: visible@example.org\r\n\
             Subject: Blind copy\r\n\
             \r\n\
             It works",
        )
        .await;

    let retrieved = api
        .emails
        .get(&captured_id(&reply))
        .await
        .expect("retrieved");

    assert_eq!(retrieved.to, ["visible@example.org"]);
    assert_eq!(
        retrieved.bcc,
        ["blind@example.org"],
        "a recipient in the envelope that no header accounts for was blind-copied"
    );
}

#[tokio::test]
async fn should_undo_dot_stuffing_in_the_body() {
    let name = "re_should_undo_dot_stuffing";
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let api = client_for(&server, name);

    let mut client = Client::connect(server.smtp_address()).await;
    let _authenticated = client.authenticate(name).await;
    let _from = client.send("MAIL FROM:<a@example.com>").await;
    let _to = client.send("RCPT TO:<b@example.org>").await;
    let _data = client.send("DATA").await;

    // A line the client wrote as `.hidden` is sent as `..hidden`. Leaving the extra dot
    // in corrupts the message.
    let reply = client
        .send_data(
            "From: a@example.com\r\n\
             To: b@example.org\r\n\
             Subject: Dots\r\n\
             \r\n\
             ..hidden\r\n\
             normal",
        )
        .await;

    let retrieved = api
        .emails
        .get(&captured_id(&reply))
        .await
        .expect("retrieved");

    let text = retrieved.text.unwrap_or_default();

    assert!(text.contains(".hidden"), "got {text:?}");
    assert!(!text.contains("..hidden"), "got {text:?}");
}

#[tokio::test]
async fn should_refuse_a_send_before_authentication() {
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let mut client = Client::connect(server.smtp_address()).await;

    let _greeting = client.send("EHLO test.example.com").await;

    assert!(
        client
            .send("MAIL FROM:<a@example.com>")
            .await
            .starts_with("530")
    );
}

#[tokio::test]
async fn should_refuse_a_password_that_is_not_an_api_key() {
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let mut client = Client::connect(server.smtp_address()).await;

    let reply = client.authenticate("hunter2").await;

    assert!(reply.starts_with("535"), "got {reply}");
}

#[tokio::test]
async fn should_refuse_a_username_that_is_not_resend() {
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let mut client = Client::connect(server.smtp_address()).await;

    let _greeting = client.send("EHLO test.example.com").await;
    let payload = BASE64.encode("\0admin\0re_test");

    let reply = client.send(&format!("AUTH PLAIN {payload}")).await;

    assert!(reply.starts_with("535"), "got {reply}");
}

#[tokio::test]
async fn should_authenticate_with_the_login_mechanism_too() {
    let name = "re_should_authenticate_with_login";
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let mut client = Client::connect(server.smtp_address()).await;

    let _greeting = client.send("EHLO test.example.com").await;

    assert!(client.send("AUTH LOGIN").await.starts_with("334"));
    assert!(
        client
            .send(&BASE64.encode("resend"))
            .await
            .starts_with("334")
    );
    assert!(client.send(&BASE64.encode(name)).await.starts_with("235"));
}

#[tokio::test]
async fn should_refuse_more_than_fifty_recipients() {
    let name = "re_should_refuse_fifty_one_recipients_smtp";
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let mut client = Client::connect(server.smtp_address()).await;

    let _authenticated = client.authenticate(name).await;
    let _from = client.send("MAIL FROM:<a@example.com>").await;

    for index in 0..50 {
        let reply = client
            .send(&format!("RCPT TO:<r{index}@example.org>"))
            .await;

        assert!(reply.starts_with("250"), "recipient {index}: {reply}");
    }

    assert!(
        client
            .send("RCPT TO:<one-too-many@example.org>")
            .await
            .starts_with("452"),
        "Resend allows at most 50"
    );
}

#[tokio::test]
async fn should_abandon_a_transaction_on_rset() {
    let name = "re_should_abandon_on_rset";
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let mut client = Client::connect(server.smtp_address()).await;

    let _authenticated = client.authenticate(name).await;
    let _from = client.send("MAIL FROM:<a@example.com>").await;
    let _to = client.send("RCPT TO:<b@example.org>").await;

    assert!(client.send("RSET").await.starts_with("250"));
    assert!(
        client.send("DATA").await.starts_with("503"),
        "the transaction is gone, so DATA has nothing to send"
    );
}

#[tokio::test]
async fn should_report_an_unimplemented_command_as_such() {
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let mut client = Client::connect(server.smtp_address()).await;

    let _greeting = client.send("EHLO test.example.com").await;

    // Not implemented rather than not recognised: there is no TLS here on purpose.
    assert!(client.send("STARTTLS").await.starts_with("502"));
}

#[tokio::test]
async fn should_not_confirm_whether_an_address_exists() {
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let mut client = Client::connect(server.smtp_address()).await;

    let _greeting = client.send("EHLO test.example.com").await;

    // A feature only a spammer wants, and there are no addresses here anyway.
    assert!(client.send("VRFY postmaster").await.starts_with("252"));
}

#[tokio::test]
async fn should_say_goodbye_on_quit() {
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let mut client = Client::connect(server.smtp_address()).await;

    assert!(client.send("QUIT").await.starts_with("221"));
}

#[tokio::test]
async fn should_refuse_an_overlong_command_line() {
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let mut client = Client::connect(server.smtp_address()).await;

    // Bounded reads are the reason this is a 500 rather than an allocation until the
    // process dies.
    let reply = client.send(&format!("EHLO {}", "a".repeat(8_192))).await;

    assert!(reply.starts_with("500"), "got {reply}");
}

#[tokio::test]
async fn should_not_capture_a_message_under_another_key() {
    let mine = "re_should_not_cross_keys_smtp_mine";
    let theirs = "re_should_not_cross_keys_smtp_theirs";

    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let other = client_for(&server, theirs);

    let mut client = Client::connect(server.smtp_address()).await;
    let _authenticated = client.authenticate(mine).await;
    let _from = client.send("MAIL FROM:<a@example.com>").await;
    let _to = client.send("RCPT TO:<b@example.org>").await;
    let _data = client.send("DATA").await;

    let reply = client
        .send_data("From: a@example.com\r\nSubject: Private\r\n\r\nIt works")
        .await;

    let error = other
        .emails
        .get(&captured_id(&reply))
        .await
        .expect_err("the key that authenticated owns it, and this is not that key");

    let resend_rs::Error::Resend(error) = error else {
        panic!("expected a Resend error, got {error:?}");
    };

    assert_eq!(error.name, "not_found");
}

#[tokio::test]
async fn should_let_one_connection_send_more_than_one_message() {
    let name = "re_should_send_several_over_one_connection";
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let mut client = Client::connect(server.smtp_address()).await;

    let _authenticated = client.authenticate(name).await;

    for subject in ["first", "second"] {
        let _from = client.send("MAIL FROM:<a@example.com>").await;
        let _to = client.send("RCPT TO:<b@example.org>").await;
        let _data = client.send("DATA").await;

        let reply = client
            .send_data(&format!(
                "From: a@example.com\r\nTo: b@example.org\r\nSubject: {subject}\r\n\r\nIt works"
            ))
            .await;

        assert!(reply.starts_with("250"), "{subject}: {reply}");
    }

    let key = mock_resend_core::ApiKey::parse(name).expect("valid");

    assert_eq!(
        server.engine().list_emails(&key).await.expect("list").len(),
        2
    );
}

#[tokio::test]
async fn should_produce_the_same_events_as_an_api_send() {
    let name = "re_should_produce_the_same_events";
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let api = client_for(&server, name);

    let mut client = Client::connect(server.smtp_address()).await;
    let _authenticated = client.authenticate(name).await;
    let _from = client.send("MAIL FROM:<a@example.com>").await;
    let _to = client.send("RCPT TO:<b@example.org>").await;
    let _data = client.send("DATA").await;

    let reply = client
        .send_data("From: a@example.com\r\nSubject: Events\r\n\r\nIt works")
        .await;
    let id = captured_id(&reply);

    // Driven through the admin API, exactly as it would be for an API send.
    let response = reqwest::Client::new()
        .post(format!("{}/_admin/emails/{id}/events", server.base_url()))
        .json(&serde_json::json!({ "api_key": name, "event": "delivered" }))
        .send()
        .await
        .expect("the admin API answers");

    assert_eq!(response.status(), 200);
    assert_eq!(
        api.emails.get(&id).await.expect("retrieved").last_event,
        EmailEvent::Delivered
    );
}

#[tokio::test]
async fn should_capture_an_attachment_out_of_a_multipart_message() {
    let name = "re_should_capture_an_smtp_attachment";
    let server = mock_resend_testkit::MockResend::start()
        .await
        .expect("the instance binds a port");
    let api = client_for(&server, name);

    let mut client = Client::connect(server.smtp_address()).await;
    let _authenticated = client.authenticate(name).await;
    let _from = client.send("MAIL FROM:<a@example.com>").await;
    let _to = client.send("RCPT TO:<b@example.org>").await;
    let _data = client.send("DATA").await;

    // The shape a mail client really sends: the body in an alternative, nested inside a
    // mixed that carries the file.
    let reply = client
        .send_data(
            "From: a@example.com\r\n\
             To: b@example.org\r\n\
             Subject: With a file\r\n\
             Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
             \r\n\
             --outer\r\n\
             Content-Type: multipart/alternative; boundary=\"inner\"\r\n\
             \r\n\
             --inner\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             It works\r\n\
             --inner--\r\n\
             --outer\r\n\
             Content-Type: text/csv\r\n\
             Content-Disposition: attachment; filename=\"report.csv\"\r\n\
             \r\n\
             name,count\r\n\
             --outer--",
        )
        .await;

    assert!(reply.starts_with("250"), "got {reply}");

    let id = captured_id(&reply);
    let retrieved = api.emails.get(&id).await.expect("retrieved");

    assert_eq!(retrieved.text.as_deref(), Some("It works"));

    // Resend does not report attachments on a retrieve, so the admin surface is where a
    // captured file shows up.
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!(
            "{}/_admin/emails/{id}?api_key={name}",
            server.base_url()
        ))
        .send()
        .await
        .expect("the admin API answers")
        .json()
        .await
        .expect("the body is json");

    assert_eq!(body["attachments"][0]["filename"], "report.csv");
    assert_eq!(body["attachments"][0]["source"], "content");
    assert_eq!(body["attachments"][0]["bytes"], 10);
}

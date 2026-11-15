//! Reading a line off a socket without trusting the peer.
//!
//! `AsyncBufReadExt::read_line` and `read_until` both append to a buffer with no upper
//! bound, so a peer that opens a connection and sends bytes without a newline can make
//! this process allocate until it dies. Everything in this crate parses hostile input,
//! so the limit is enforced here rather than hoped for.

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Why a line could not be read.
#[derive(Debug)]
pub enum ReadError {
    /// The peer closed the connection.
    Closed,
    /// The line was longer than the limit, so the connection cannot be trusted to be
    /// speaking SMTP.
    TooLong,
    /// The socket failed.
    Io(std::io::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("the peer closed the connection"),
            Self::TooLong => f.write_str("the peer sent a line over the limit"),
            Self::Io(error) => write!(f, "the socket failed: {error}"),
        }
    }
}

/// Reads one `\n`-terminated line into `buffer`, up to `limit` bytes.
///
/// The trailing `\r\n` or `\n` is removed. The buffer is cleared first, so a caller can
/// reuse one allocation for a whole session.
///
/// # Errors
///
/// [`ReadError::TooLong`] once `limit` bytes have arrived with no newline among them,
/// [`ReadError::Closed`] at end of stream, and [`ReadError::Io`] for a socket failure.
pub async fn read_line<R>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> Result<(), ReadError>
where
    R: AsyncBufRead + Unpin,
{
    buffer.clear();

    loop {
        let available = reader.fill_buf().await.map_err(ReadError::Io)?;

        if available.is_empty() {
            return if buffer.is_empty() {
                Err(ReadError::Closed)
            } else {
                // A final line with no newline is still a line; the peer just hung up
                // after it.
                trim_eol(buffer);
                Ok(())
            };
        }

        match available.iter().position(|byte| *byte == b'\n') {
            Some(position) => {
                // The limit is checked before the bytes are copied, so an overlong line
                // is never allocated.
                if buffer.len() + position + 1 > limit {
                    return Err(ReadError::TooLong);
                }

                buffer.extend_from_slice(&available[..=position]);
                reader.consume(position + 1);
                trim_eol(buffer);

                return Ok(());
            }
            None => {
                if buffer.len() + available.len() > limit {
                    return Err(ReadError::TooLong);
                }

                let taken = available.len();
                buffer.extend_from_slice(available);
                reader.consume(taken);
            }
        }
    }
}

/// Removes a trailing `\r\n` or `\n`.
fn trim_eol(buffer: &mut Vec<u8>) {
    if buffer.last() == Some(&b'\n') {
        let _newline = buffer.pop();
    }
    if buffer.last() == Some(&b'\r') {
        let _carriage_return = buffer.pop();
    }
}

#[cfg(test)]
mod read_line {
    use tokio::io::BufReader;

    use super::{ReadError, read_line};

    async fn lines(input: &[u8], limit: usize) -> Result<Vec<String>, ReadError> {
        let mut reader = BufReader::new(input);
        let mut buffer = Vec::new();
        let mut lines = Vec::new();

        loop {
            match read_line(&mut reader, &mut buffer, limit).await {
                Ok(()) => lines.push(String::from_utf8_lossy(&buffer).into_owned()),
                Err(ReadError::Closed) => return Ok(lines),
                Err(error) => return Err(error),
            }
        }
    }

    #[tokio::test]
    async fn should_read_crlf_terminated_lines() {
        let read = lines(b"EHLO one\r\nMAIL FROM:<a@example.com>\r\n", 1024)
            .await
            .expect("reads");

        assert_eq!(read, ["EHLO one", "MAIL FROM:<a@example.com>"]);
    }

    #[tokio::test]
    async fn should_read_bare_lf_terminated_lines() {
        // Plenty of clients send a bare newline, and refusing them would be stricter
        // than every real mail server.
        let read = lines(b"NOOP\nQUIT\n", 1024).await.expect("reads");

        assert_eq!(read, ["NOOP", "QUIT"]);
    }

    #[tokio::test]
    async fn should_read_a_final_line_with_no_newline() {
        let read = lines(b"QUIT", 1024).await.expect("reads");

        assert_eq!(read, ["QUIT"]);
    }

    #[tokio::test]
    async fn should_keep_an_empty_line_as_an_empty_line() {
        // The blank line between headers and body is significant.
        let read = lines(b"A\r\n\r\nB\r\n", 1024).await.expect("reads");

        assert_eq!(read, ["A", "", "B"]);
    }

    #[tokio::test]
    async fn should_report_the_end_of_the_stream() {
        let read = lines(b"", 1024).await.expect("reads");

        assert!(read.is_empty());
    }

    #[tokio::test]
    async fn should_refuse_a_line_over_the_limit() {
        let input = format!("{}\r\n", "a".repeat(100));

        assert!(matches!(
            lines(input.as_bytes(), 32).await,
            Err(ReadError::TooLong)
        ));
    }

    #[tokio::test]
    async fn should_refuse_a_peer_that_never_sends_a_newline() {
        // The case the standard helpers get wrong: without a bound this allocates until
        // the process dies.
        let input = vec![b'a'; 10_000];

        assert!(matches!(
            lines(&input, 1_024).await,
            Err(ReadError::TooLong)
        ));
    }

    #[tokio::test]
    async fn should_accept_a_line_exactly_at_the_limit() {
        // 30 characters plus CRLF is 32.
        let input = format!("{}\r\n", "a".repeat(30));

        let read = lines(input.as_bytes(), 32).await.expect("reads");

        assert_eq!(read[0].len(), 30);
    }

    #[tokio::test]
    async fn should_keep_a_line_containing_non_utf8_bytes_readable() {
        // A command line is ASCII, but a DATA line can be anything, and the reader must
        // not lose bytes it does not understand.
        let read = lines(b"\xff\xfe\r\n", 1024).await.expect("reads");

        assert_eq!(read.len(), 1);
    }
}

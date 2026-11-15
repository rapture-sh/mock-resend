//! The container healthcheck.
//!
//! The binary checks itself, so the image needs no `curl` or `wget`. That keeps the
//! runtime layer to the binary and a CA bundle, which is most of why the image is small.
//!
//! Written against `std::net` rather than an HTTP client: a healthcheck runs every ten
//! seconds forever, and one request to a known local endpoint does not justify linking a
//! second client into the server.

use std::io::{Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// How long the check waits at each stage.
const TIMEOUT: Duration = Duration::from_secs(2);

/// The most of a response to read before giving up.
///
/// The status line is in the first few bytes; anything beyond this is the health body,
/// which the check does not need.
const MAX_RESPONSE_BYTES: usize = 512;

/// Asks the admin API whether the server is up.
///
/// Returns the reason it is not, or `Ok(())`.
///
/// Connects to the loopback address rather than to whatever `--host` says: inside the
/// container the server is bound to all interfaces, and `0.0.0.0` is not an address a
/// client connects to.
///
/// # Errors
///
/// A description of what failed, for the healthcheck's own output.
pub fn run(port: u16) -> Result<(), String> {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT)
        .map_err(|error| format!("could not connect to {address}: {error}"))?;

    stream
        .set_read_timeout(Some(TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(TIMEOUT)))
        .map_err(|error| format!("could not set a timeout: {error}"))?;

    let request = format!(
        "GET /_admin/health HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
    );

    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|error| format!("could not send the request: {error}"))?;

    // Bounded, like every other read in this project: a healthcheck that can be made to
    // allocate without limit is a healthcheck that fails the container it watches.
    let mut response = Vec::new();
    let mut chunk = [0_u8; 256];

    while response.len() < MAX_RESPONSE_BYTES {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(error) => return Err(format!("could not read the response: {error}")),
        }
    }

    let status = String::from_utf8_lossy(&response);
    let status_line = status.lines().next().unwrap_or_default();

    if status_line.contains(" 200") {
        Ok(())
    } else {
        Err(format!("the server answered `{status_line}`"))
    }
}

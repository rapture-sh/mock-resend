//! Command line definitions.
//!
//! Every flag has a `MOCK_RESEND_`-prefixed environment variable equivalent, which is
//! usually easier in a compose file.

use std::net::IpAddr;

use clap::Parser;

/// A Resend-compatible email API for local development and testing.
///
/// Captures every message instead of delivering it. There is no MTA here.
#[derive(Debug, Clone, Parser)]
#[command(name = "mock-resend", version, about, long_about = None)]
pub struct Cli {
    /// Address to bind the Resend HTTP API to.
    ///
    /// Defaults to all interfaces so that the container is reachable from the host.
    /// Nothing here is authenticated, so keep it on a container network or localhost.
    #[arg(long, env = "MOCK_RESEND_HOST", default_value = "0.0.0.0")]
    pub host: IpAddr,

    /// Port for the Resend HTTP API.
    #[arg(long, env = "MOCK_RESEND_PORT", default_value_t = 9100)]
    pub port: u16,

    /// Requests per second per API key. Off by default.
    ///
    /// Real Resend allows 10 a second, but a test suite that trips a limit it did not
    /// ask for is a flaky test suite. A suite proving its backoff works turns this on
    /// deliberately. When set, every response carries the `ratelimit-*` headers.
    #[arg(long, env = "MOCK_RESEND_RATE_LIMIT")]
    pub rate_limit: Option<u32>,

    /// Milliseconds after a send before the message is delivered. Off by default.
    ///
    /// With this unset a captured message stays at `sent` until something advances it,
    /// so a test asserting "it was accepted" cannot race the delivery ticker. Set it to
    /// walk messages through the lifecycle without driving each step.
    #[arg(long, env = "MOCK_RESEND_DELIVERY_DELAY")]
    pub delivery_delay: Option<u32>,

    /// Log level: `error`, `warn`, `info`, `debug` or `trace`.
    #[arg(long, env = "MOCK_RESEND_LOG", default_value = "info")]
    pub log: String,
}

#[cfg(test)]
mod command_line {
    use clap::{CommandFactory as _, Parser as _};

    use super::Cli;

    #[test]
    fn should_default_to_the_documented_port() {
        let cli = Cli::parse_from(["mock-resend"]);

        assert_eq!(cli.port, 9100);
    }

    #[test]
    fn should_accept_a_port_override() {
        let cli = Cli::parse_from(["mock-resend", "--port", "9999"]);

        assert_eq!(cli.port, 9999);
    }

    #[test]
    fn should_reject_a_port_that_is_not_a_number() {
        assert!(Cli::try_parse_from(["mock-resend", "--port", "nine"]).is_err());
    }

    #[test]
    fn should_leave_rate_limiting_and_delivery_off_by_default() {
        let cli = Cli::parse_from(["mock-resend"]);

        assert_eq!(cli.rate_limit, None);
        assert_eq!(cli.delivery_delay, None);
    }

    #[test]
    fn should_accept_a_rate_limit_and_a_delivery_delay() {
        let cli = Cli::parse_from([
            "mock-resend",
            "--rate-limit",
            "10",
            "--delivery-delay",
            "250",
        ]);

        assert_eq!(cli.rate_limit, Some(10));
        assert_eq!(cli.delivery_delay, Some(250));
    }

    #[test]
    fn should_accept_a_rate_limit_of_zero() {
        // Shutting a key out entirely is a legitimate thing to test against.
        let cli = Cli::parse_from(["mock-resend", "--rate-limit", "0"]);

        assert_eq!(cli.rate_limit, Some(0));
    }

    #[test]
    fn should_reject_a_host_that_is_not_an_address() {
        assert!(Cli::try_parse_from(["mock-resend", "--host", "not-an-ip"]).is_err());
    }

    /// `--help` and `--version` have to work, because the arguments are documented in
    /// the README and a broken definition is a broken first impression.
    #[test]
    fn should_build_a_valid_command_definition() {
        Cli::command().debug_assert();
    }
}

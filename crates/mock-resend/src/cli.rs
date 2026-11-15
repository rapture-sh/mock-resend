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

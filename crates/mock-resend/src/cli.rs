//! Command line definitions.
//!
//! Every flag has a `MOCK_RESEND_`-prefixed environment variable equivalent, which is
//! usually easier in a compose file.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

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

    /// Port for the SMTP listener.
    #[arg(long, env = "MOCK_RESEND_SMTP_PORT", default_value_t = 1025)]
    pub smtp_port: u16,

    /// Disable the SMTP listener.
    #[arg(long, env = "MOCK_RESEND_NO_SMTP")]
    pub no_smtp: bool,

    /// Where captured mail is written, as one `.eml` per message.
    ///
    /// Defaults to `/data` inside the published image and `./data` elsewhere, so a
    /// mounted volume keeps captured mail across a container restart and the files can
    /// be opened in a mail client.
    #[arg(long, env = "MOCK_RESEND_DATA")]
    pub data: Option<PathBuf>,

    /// Keep everything in RAM and discard it on exit.
    ///
    /// Nothing is written anywhere. For a suite that wants no files at all.
    #[arg(long, env = "MOCK_RESEND_IN_MEMORY", conflicts_with = "data")]
    pub in_memory: bool,

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

    /// Let webhooks reach public internet addresses.
    ///
    /// Off by default. Every legitimate handler is on localhost or a neighbouring
    /// container, and the dangerous case is a typo posting captured mail to a stranger.
    /// Link-local addresses stay refused either way, because that is where a cloud
    /// metadata service lives.
    #[arg(long, env = "MOCK_RESEND_ALLOW_PUBLIC_WEBHOOKS")]
    pub allow_public_webhooks: bool,

    /// Log level: `error`, `warn`, `info`, `debug` or `trace`.
    #[arg(long, env = "MOCK_RESEND_LOG", default_value = "info")]
    pub log: String,
}

impl Cli {
    /// Where captured mail should be written, or `None` for in-memory.
    ///
    /// `/data` inside the published image, `./data` elsewhere. The image sets
    /// `MOCK_RESEND_DATA`, so this default only has to be right for someone running
    /// `cargo run`.
    #[must_use]
    pub fn data_directory(&self) -> Option<PathBuf> {
        if self.in_memory {
            return None;
        }

        Some(
            self.data
                .clone()
                .unwrap_or_else(|| Path::new("./data").to_path_buf()),
        )
    }
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
    fn should_default_to_the_documented_smtp_port() {
        let cli = Cli::parse_from(["mock-resend"]);

        assert_eq!(cli.smtp_port, 1025);
        assert!(!cli.no_smtp);
    }

    #[test]
    fn should_accept_turning_smtp_off() {
        assert!(Cli::parse_from(["mock-resend", "--no-smtp"]).no_smtp);
    }

    #[test]
    fn should_default_to_writing_captured_mail_to_disk() {
        let cli = Cli::parse_from(["mock-resend"]);

        assert_eq!(
            cli.data_directory(),
            Some(std::path::Path::new("./data").to_path_buf())
        );
    }

    #[test]
    fn should_write_nothing_when_asked_for_memory_only() {
        let cli = Cli::parse_from(["mock-resend", "--in-memory"]);

        assert_eq!(cli.data_directory(), None);
    }

    #[test]
    fn should_accept_a_data_directory() {
        let cli = Cli::parse_from(["mock-resend", "--data", "/var/mail"]);

        assert_eq!(
            cli.data_directory(),
            Some(std::path::Path::new("/var/mail").to_path_buf())
        );
    }

    #[test]
    fn should_refuse_a_data_directory_and_in_memory_together() {
        // One of them is a lie, and which one is not obvious from the command line.
        assert!(
            Cli::try_parse_from(["mock-resend", "--data", "/var/mail", "--in-memory"]).is_err()
        );
    }

    #[test]
    fn should_keep_webhooks_local_only_by_default() {
        assert!(!Cli::parse_from(["mock-resend"]).allow_public_webhooks);
    }

    #[test]
    fn should_accept_the_public_webhook_opt_in() {
        assert!(Cli::parse_from(["mock-resend", "--allow-public-webhooks"]).allow_public_webhooks);
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

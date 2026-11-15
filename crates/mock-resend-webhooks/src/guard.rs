//! The outbound address policy.
//!
//! A webhook endpoint is a URL the caller chose, so posting to one is a server-side
//! request forgery on the developer's own machine. This module decides what may be
//! reached, and it is the only gate in front of the only outbound client in the
//! workspace.
//!
//! The default is **the opposite of a normal SSRF guard**, and deliberately so. A
//! normal guard blocks private addresses and allows the internet; here every legitimate
//! endpoint is a handler on `localhost` or a neighbouring container, and the dangerous
//! case is a typo sending captured mail out to a stranger. So loopback and private
//! ranges are allowed, the public internet is refused unless explicitly enabled, and
//! link-local is refused either way because that is where a cloud metadata service
//! lives.

use std::net::IpAddr;

use tokio::net::lookup_host;
use url::Url;

/// What the guard will reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    /// Loopback and private ranges only. The default.
    #[default]
    LocalOnly,
    /// Public addresses as well, for someone deliberately pointing at a remote handler.
    AllowPublic,
}

/// Why an endpoint was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The URL could not be parsed.
    #[error("`{url}` is not a URL that can be posted to")]
    NotAUrl {
        /// The offending value.
        url: String,
    },

    /// The scheme was not `http` or `https`.
    #[error("`{scheme}` is not an http scheme")]
    Scheme {
        /// The offending scheme.
        scheme: String,
    },

    /// The host could not be resolved.
    #[error("`{host}` did not resolve to any address")]
    Unresolved {
        /// The offending host.
        host: String,
    },

    /// The address is one this guard never reaches.
    #[error("{address} is a {reason} address, which mock-resend never posts to")]
    Forbidden {
        /// The resolved address.
        address: IpAddr,
        /// Which category it fell into.
        reason: &'static str,
    },

    /// The address is public and the policy is local-only.
    #[error(
        "{address} is a public address; mock-resend refuses to post captured mail to the internet unless --allow-public-webhooks is set"
    )]
    Public {
        /// The resolved address.
        address: IpAddr,
    },
}

/// Checks a URL against `policy`.
///
/// Every address the host resolves to is checked, and one bad address refuses the whole
/// endpoint: a name resolving to both a loopback and a metadata address must not be
/// reachable just because the first answer looked fine.
///
/// # Errors
///
/// A [`Refusal`] naming what was wrong.
///
/// # DNS rebinding
///
/// The address is resolved here and the request is made by the client afterwards, so a
/// name that changes its answer in between could slip past. Closing that needs the
/// connection pinned to the address checked, which is not worth the complexity for a
/// tool that runs on a developer's machine; the window is noted rather than hidden.
pub async fn check(url: &str, policy: Policy) -> Result<Url, Refusal> {
    let parsed = Url::parse(url.trim()).map_err(|_| Refusal::NotAUrl {
        url: url.to_owned(),
    })?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Refusal::Scheme {
            scheme: parsed.scheme().to_owned(),
        });
    }

    let Some(host) = parsed.host_str() else {
        return Err(Refusal::NotAUrl {
            url: url.to_owned(),
        });
    };

    // A default port keeps `lookup_host` happy; which port it is does not matter to the
    // address check.
    let port = parsed.port_or_known_default().unwrap_or(80);

    let addresses: Vec<IpAddr> = lookup_host((host, port))
        .await
        .map_err(|_| Refusal::Unresolved {
            host: host.to_owned(),
        })?
        .map(|socket| socket.ip())
        .collect();

    if addresses.is_empty() {
        return Err(Refusal::Unresolved {
            host: host.to_owned(),
        });
    }

    for address in addresses {
        check_address(address, policy)?;
    }

    Ok(parsed)
}

/// Checks one resolved address.
///
/// # Errors
///
/// A [`Refusal`] naming the category the address fell into.
pub fn check_address(address: IpAddr, policy: Policy) -> Result<(), Refusal> {
    if let Some(reason) = always_forbidden(address) {
        return Err(Refusal::Forbidden { address, reason });
    }

    if is_local(address) {
        return Ok(());
    }

    match policy {
        Policy::AllowPublic => Ok(()),
        Policy::LocalOnly => Err(Refusal::Public { address }),
    }
}

/// Categories no policy reaches.
fn always_forbidden(address: IpAddr) -> Option<&'static str> {
    if address.is_multicast() {
        return Some("multicast");
    }

    if address.is_unspecified() {
        return Some("unspecified");
    }

    match address {
        IpAddr::V4(address) => {
            // 169.254.0.0/16. The cloud metadata service at 169.254.169.254 lives here,
            // and it is the single most valuable thing an SSRF can reach from CI.
            if address.is_link_local() {
                return Some("link-local");
            }

            if address.is_broadcast() {
                return Some("broadcast");
            }

            // 0.0.0.0/8, which some stacks treat as "this host".
            if address.octets()[0] == 0 {
                return Some("this-network");
            }

            None
        }
        IpAddr::V6(address) => {
            let segments = address.segments();

            // fe80::/10, the IPv6 link-local range.
            if segments[0] & 0xffc0 == 0xfe80 {
                return Some("link-local");
            }

            // An IPv4-mapped address has to be judged as the IPv4 address it carries,
            // or ::ffff:169.254.169.254 would walk straight through this.
            if let Some(mapped) = address.to_ipv4_mapped() {
                return always_forbidden(IpAddr::V4(mapped));
            }

            None
        }
    }
}

/// Whether the address is one of the ranges a local handler lives in.
fn is_local(address: IpAddr) -> bool {
    if address.is_loopback() {
        return true;
    }

    match address {
        IpAddr::V4(address) => address.is_private(),
        IpAddr::V6(address) => {
            let segments = address.segments();

            // fc00::/7, unique local addresses.
            if segments[0] & 0xfe00 == 0xfc00 {
                return true;
            }

            address
                .to_ipv4_mapped()
                .is_some_and(|mapped| mapped.is_loopback() || mapped.is_private())
        }
    }
}

#[cfg(test)]
mod check_address {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::{Policy, Refusal, check_address};

    fn v4(address: &str) -> IpAddr {
        IpAddr::V4(address.parse::<Ipv4Addr>().expect("a valid address"))
    }

    fn v6(address: &str) -> IpAddr {
        IpAddr::V6(address.parse::<Ipv6Addr>().expect("a valid address"))
    }

    #[test]
    fn should_allow_loopback_because_that_is_where_a_test_handler_lives() {
        assert!(check_address(v4("127.0.0.1"), Policy::LocalOnly).is_ok());
        assert!(check_address(v6("::1"), Policy::LocalOnly).is_ok());
    }

    #[test]
    fn should_allow_the_private_ranges_a_container_network_uses() {
        for address in ["10.0.0.5", "172.17.0.2", "192.168.1.10"] {
            assert!(
                check_address(v4(address), Policy::LocalOnly).is_ok(),
                "{address} should be reachable"
            );
        }
    }

    #[test]
    fn should_allow_a_unique_local_ipv6_address() {
        assert!(check_address(v6("fd00::1"), Policy::LocalOnly).is_ok());
    }

    #[test]
    fn should_refuse_the_cloud_metadata_address_under_every_policy() {
        // The single most valuable thing an SSRF can reach from a CI runner.
        for policy in [Policy::LocalOnly, Policy::AllowPublic] {
            assert!(
                matches!(
                    check_address(v4("169.254.169.254"), policy),
                    Err(Refusal::Forbidden { .. })
                ),
                "metadata must be refused under {policy:?}"
            );
        }
    }

    #[test]
    fn should_refuse_the_whole_link_local_range() {
        assert!(matches!(
            check_address(v4("169.254.1.1"), Policy::AllowPublic),
            Err(Refusal::Forbidden { .. })
        ));
        assert!(matches!(
            check_address(v6("fe80::1"), Policy::AllowPublic),
            Err(Refusal::Forbidden { .. })
        ));
    }

    #[test]
    fn should_refuse_an_ipv4_mapped_metadata_address() {
        // Without unwrapping the mapping this walks straight through the IPv6 branch.
        assert!(matches!(
            check_address(v6("::ffff:169.254.169.254"), Policy::AllowPublic),
            Err(Refusal::Forbidden { .. })
        ));
    }

    #[test]
    fn should_allow_an_ipv4_mapped_loopback_address() {
        assert!(check_address(v6("::ffff:127.0.0.1"), Policy::LocalOnly).is_ok());
    }

    #[test]
    fn should_refuse_multicast_and_broadcast() {
        assert!(matches!(
            check_address(v4("224.0.0.1"), Policy::AllowPublic),
            Err(Refusal::Forbidden { .. })
        ));
        assert!(matches!(
            check_address(v4("255.255.255.255"), Policy::AllowPublic),
            Err(Refusal::Forbidden { .. })
        ));
    }

    #[test]
    fn should_refuse_the_unspecified_address() {
        assert!(matches!(
            check_address(v4("0.0.0.0"), Policy::AllowPublic),
            Err(Refusal::Forbidden { .. })
        ));
    }

    #[test]
    fn should_refuse_a_public_address_by_default() {
        let refusal = check_address(v4("93.184.216.34"), Policy::LocalOnly)
            .expect_err("the internet is not reachable by default");

        assert!(matches!(refusal, Refusal::Public { .. }));
    }

    #[test]
    fn should_reach_a_public_address_once_it_is_allowed() {
        assert!(check_address(v4("93.184.216.34"), Policy::AllowPublic).is_ok());
    }
}

#[cfg(test)]
mod check {
    use super::{Policy, Refusal, check};

    #[tokio::test]
    async fn should_accept_a_loopback_url() {
        assert!(
            check("http://127.0.0.1:3000/hooks", Policy::LocalOnly)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn should_refuse_a_url_that_is_not_a_url() {
        assert!(matches!(
            check("not a url", Policy::AllowPublic).await,
            Err(Refusal::NotAUrl { .. })
        ));
    }

    #[tokio::test]
    async fn should_refuse_a_non_http_scheme() {
        for url in ["file:///etc/passwd", "ftp://example.com/x", "gopher://x/1"] {
            assert!(
                matches!(
                    check(url, Policy::AllowPublic).await,
                    Err(Refusal::Scheme { .. })
                ),
                "{url} should be refused"
            );
        }
    }

    #[tokio::test]
    async fn should_refuse_a_metadata_url_by_address_not_by_name() {
        // Written as a literal, so no DNS is involved and the check is on the address.
        assert!(matches!(
            check(
                "http://169.254.169.254/latest/meta-data/",
                Policy::AllowPublic
            )
            .await,
            Err(Refusal::Forbidden { .. })
        ));
    }

    #[tokio::test]
    async fn should_refuse_a_host_that_does_not_resolve() {
        let refusal = check(
            "http://this-host-does-not-exist.invalid/hooks",
            Policy::AllowPublic,
        )
        .await
        .expect_err("`.invalid` never resolves");

        assert!(matches!(refusal, Refusal::Unresolved { .. }));
    }

    #[tokio::test]
    async fn should_keep_the_path_and_query_of_an_accepted_url() {
        let url = check("http://127.0.0.1:3000/hooks?a=1", Policy::LocalOnly)
            .await
            .expect("accepted");

        assert_eq!(url.path(), "/hooks");
        assert_eq!(url.query(), Some("a=1"));
    }
}

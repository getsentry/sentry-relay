//! Implements event filtering based on the client ip address.
//!
//! A project may be configured with blacklisted ip addresses that will
//! be banned from sending events (all events received from banned ip
//! addresses will be filtered).

use std::net::IpAddr;

use ipnetwork::IpNetwork;

use crate::filter::config::ClientIpsFilterConfig;
use crate::filter::FilterStatKey;

/// Should filter event
pub fn should_filter(
    client_ip: Option<IpAddr>,
    config: &ClientIpsFilterConfig,
) -> Result<(), FilterStatKey> {
    let blacklisted_ips = &config.blacklisted_ips;
    if blacklisted_ips.is_empty() {
        return Ok(());
    }

    if let Some(client_ip) = client_ip {
        for black_listed_ip in blacklisted_ips {
            if black_listed_ip.contains('/') {
                //probably a network specification
                let bl_ip_network: Result<IpNetwork, _> = black_listed_ip.as_str().parse();
                if let Ok(bl_ip_network) = bl_ip_network {
                    if bl_ip_network.contains(client_ip) {
                        return Err(FilterStatKey::IpAddress);
                    }
                }
            } else {
                //probably an ip address
                let black_listed_ip: Result<IpAddr, _> = black_listed_ip.as_str().parse();
                if let Ok(black_listed_ip) = black_listed_ip {
                    if client_ip == black_listed_ip {
                        return Err(FilterStatKey::IpAddress);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_filter_blacklisted_ips() {
        let examples = &[
            //test matches ipv4
            ("1.2.3.4", &[][..], false),
            ("122.33.230.14", &["122.33.230.14"], true),
            ("122.33.230.14", &["112.133.110.33", "122.33.230.14"], true),
            //test matches ipv6
            ("aaaa:bbbb::cccc", &["aaaa:bbbb::cccc"], true),
            (
                "aaaa:bbbb::cccc",
                &["121.121.12.121", "aaaa:bbbb::cccc"],
                true,
            ),
            ("aaaa:bbbb::ccce", &["aaaa:bbbb::cccc"], false),
            //test network ipv4 matches
            ("122.33.230.1", &["122.33.230.0/30"], true),
            ("122.33.230.2", &["122.33.230.0/30"], true),
            ("122.33.230.4", &["122.33.230.0/30"], false),
            ("122.33.230.5", &["122.33.230.4/30"], true),
            //test network ipv6 matches
            ("a:b:c::1", &["a:b:c::0/126"], true),
            ("a:b:c::2", &["a:b:c::0/126"], true),
            ("a:b:c::4", &["a:b:c::0/126"], false),
            ("a:b:c::5", &["a:b:c::4/126"], true),
            //sentry compatibility tests
            ("127.0.0.1", &[][..], false),
            (
                "127.0.0.1",
                &["0.0.0.0", "192.168.1.1", "10.0.0.0/8"],
                false,
            ),
            ("127.0.0.1", &["127.0.0.1"], true),
            ("127.0.0.1", &["0.0.0.0", "127.0.0.1", "192.168.1.1"], true),
            ("127.0.0.1", &["127.0.0.0/8"], true),
            (
                "127.0.0.1",
                &["0.0.0.0", "127.0.0.0/8", "192.168.1.0/8"],
                true,
            ),
            ("127.0.0.1", &["lol/bar"], false),
        ];

        for &(ip_addr, blacklisted_ips, expected) in examples {
            let ip_addr = ip_addr.parse::<IpAddr>().ok();
            let config = ClientIpsFilterConfig {
                blacklisted_ips: blacklisted_ips.iter().map(|ip| ip.to_string()).collect(),
            };

            let actual = should_filter(ip_addr, &config) != Ok(());

            assert_eq!(
                actual,
                expected,
                "Address {} should have {} been filtered by {:?}",
                ip_addr.unwrap(),
                if expected { "" } else { "not" },
                blacklisted_ips
            );
        }
    }
}

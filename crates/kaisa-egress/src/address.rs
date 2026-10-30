//! Which addresses belong to the host's own networks.

use std::net::IpAddr;

/// True for addresses an agent must never reach through the broker: loopback, private,
/// link-local, shared (CGNAT), unique-local, unspecified, broadcast and multicast.
#[must_use]
pub fn is_host_internal(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            let [first, second, ..] = v4.octets();
            let shared_cgnat: bool = first == 100 && (64..=127).contains(&second);
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || shared_cgnat
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_host_internal(IpAddr::V4(mapped));
            }
            let first: u16 = v6.segments()[0];
            let unique_local: bool = (first & 0xfe00) == 0xfc00;
            let link_local: bool = (first & 0xffc0) == 0xfe80;
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || unique_local
                || link_local
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::is_host_internal;

    fn address(text: &str) -> IpAddr {
        text.parse().expect("test addresses are valid")
    }

    #[test]
    fn host_internal_addresses_are_recognised() {
        for internal in [
            "127.0.0.1",
            "10.1.2.3",
            "172.20.0.1",
            "192.168.0.10",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(
                is_host_internal(address(internal)),
                "{internal} is on the host's own networks and must be refused"
            );
        }
    }

    #[test]
    fn public_addresses_are_not_internal() {
        for public in ["203.0.113.10", "2001:db8::10"] {
            assert!(
                !is_host_internal(address(public)),
                "{public} is outside the host and may be reached when allow-listed"
            );
        }
    }
}

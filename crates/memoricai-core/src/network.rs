//! Pure network-address policy shared by outbound HTTP clients.

use std::net::IpAddr;

/// Return true for addresses that must never be reached from user-controlled URLs.
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_documentation()
                || a == 0
                || a >= 240
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0)
                || (a == 198 && (18..=19).contains(&b))
        }
        IpAddr::V6(ip) => {
            if let Some(ipv4) = ip.to_ipv4() {
                return is_blocked_ip(IpAddr::V4(ipv4));
            }
            let octets = ip.octets();
            // Addresses that embed an IPv4 address must be evaluated under the IPv4 policy,
            // or they become an SSRF bypass on networks that route them (NAT64/DNS64, 6to4).
            // NAT64 well-known prefix 64:ff9b::/96 carries the IPv4 in the low 32 bits.
            if octets[..12] == [0x00, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0] {
                let v4 = std::net::Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15]);
                return is_blocked_ip(IpAddr::V4(v4));
            }
            // 6to4 (2002::/16) carries the IPv4 in octets 2..6.
            if octets[0] == 0x20 && octets[1] == 0x02 {
                let v4 = std::net::Ipv4Addr::new(octets[2], octets[3], octets[4], octets[5]);
                return is_blocked_ip(IpAddr::V4(v4));
            }
            let unique_local = octets[0] & 0xfe == 0xfc; // fc00::/7
            let link_local = octets[0] == 0xfe && octets[1] & 0xc0 == 0x80; // fe80::/10
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || unique_local
                || link_local
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_private_and_metadata_addresses() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "192.168.1.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "64:ff9b::7f00:1",    // NAT64-embedded 127.0.0.1
            "64:ff9b::a9fe:a9fe", // NAT64-embedded 169.254.169.254
            "2002:7f00:1::",      // 6to4-embedded 127.0.0.1
        ] {
            assert!(is_blocked_ip(value.parse().unwrap()), "{value}");
        }
        assert!(!is_blocked_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_blocked_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}

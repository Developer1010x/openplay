//! Ordering for the addresses mDNS reports for a discovered receiver.
//!
//! `mdns_sd::ServiceInfo::get_addresses` returns a `&HashSet<IpAddr>`, so the
//! order a receiver's addresses arrive in is not merely unsorted — it is
//! non-deterministic, and changes between advertisements of the same service.
//! Since callers connect to the first address, that made the dialled address a
//! lottery, re-rolled every time a record refreshed.
//!
//! Sorting at collection makes the first address the most connectable one, so
//! `addresses.first()` is correct by construction rather than by luck.

use std::net::IpAddr;

/// Connectability rank, lowest first.
///
/// A link-local address is ranked below loopback because we advertise no zone
/// index and cannot reconstruct one, so `fe80::1` is not dialable at all, while
/// loopback at least resolves — to the wrong host for a remote receiver, which
/// is why it still ranks below anything routable.
fn rank(ip: &IpAddr) -> u8 {
    let (loopback, link_local, unspecified) = match ip {
        IpAddr::V4(v4) => (v4.is_loopback(), v4.is_link_local(), v4.is_unspecified()),
        // `Ipv6Addr::is_unicast_link_local` is still unstable, so test fe80::/10
        // directly.
        IpAddr::V6(v6) => (
            v6.is_loopback(),
            (v6.segments()[0] & 0xffc0) == 0xfe80,
            v6.is_unspecified(),
        ),
    };

    if unspecified {
        return 4;
    }
    if link_local {
        return 3;
    }
    if loopback {
        return 2;
    }
    // Routable. Prefer IPv4: every receiver we target reachable over IPv6 is
    // also reachable over IPv4, and IPv4 needs no scope handling.
    match ip {
        IpAddr::V4(_) => 0,
        IpAddr::V6(_) => 1,
    }
}

/// Sorts discovered addresses most-connectable first.
///
/// Ties break on the address value so the result is stable across runs, which
/// the `HashSet` iteration order on its own is not.
pub fn sort_by_connectability(addrs: &mut [IpAddr]) {
    addrs.sort_by_key(|ip| (rank(ip), *ip));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ips(raw: &[&str]) -> Vec<IpAddr> {
        raw.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn routable_ipv4_wins() {
        // The exact set this machine advertised for itself, in one of the
        // orders observed in the wild.
        let mut a = ips(&[
            "fe80::1",
            "::1",
            "fe80::1427:c2c8:a235:a115",
            "127.0.0.1",
            "192.168.0.97",
        ]);
        sort_by_connectability(&mut a);
        assert_eq!(a[0], "192.168.0.97".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn link_local_ranks_below_loopback() {
        let mut a = ips(&["fe80::1", "127.0.0.1"]);
        sort_by_connectability(&mut a);
        assert_eq!(a, ips(&["127.0.0.1", "fe80::1"]));
    }

    #[test]
    fn routable_ipv6_beats_loopback_and_link_local() {
        let mut a = ips(&["fe80::1", "::1", "2001:db8::1"]);
        sort_by_connectability(&mut a);
        assert_eq!(a[0], "2001:db8::1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn ipv4_preferred_over_routable_ipv6() {
        let mut a = ips(&["2001:db8::1", "192.168.0.97"]);
        sort_by_connectability(&mut a);
        assert_eq!(a[0], "192.168.0.97".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn ipv4_link_local_is_demoted_too() {
        let mut a = ips(&["169.254.1.1", "192.168.0.97"]);
        sort_by_connectability(&mut a);
        assert_eq!(a, ips(&["192.168.0.97", "169.254.1.1"]));
    }

    /// Every permutation of the observed set must produce the same first
    /// address — that is the whole point.
    #[test]
    fn ordering_is_stable_whatever_the_hashset_yielded() {
        let base = ips(&["fe80::1", "::1", "127.0.0.1", "192.168.0.97"]);
        let mut sorted: Option<Vec<IpAddr>> = None;
        for rotate in 0..base.len() {
            let mut a = base.clone();
            a.rotate_left(rotate);
            sort_by_connectability(&mut a);
            match &sorted {
                None => sorted = Some(a),
                Some(first) => assert_eq!(first, &a),
            }
        }
    }

    #[test]
    fn empty_and_single_are_fine() {
        let mut none: Vec<IpAddr> = vec![];
        sort_by_connectability(&mut none);
        assert!(none.is_empty());

        let mut one = ips(&["fe80::1"]);
        sort_by_connectability(&mut one);
        assert_eq!(one, ips(&["fe80::1"]));
    }

    /// The ordering that made two-machine casting fail, recorded as a fact
    /// rather than a preference.
    ///
    /// This machine advertised exactly this set: a Docker bridge, a Compose
    /// bridge, and the real wireless address. All three are routable IPv4, so
    /// `rank` is 0 for each and the tie breaks on the numeric value — which
    /// puts `172.17.0.1` first and the only externally reachable address last.
    ///
    /// Ranking cannot fix this. A sender's own `docker0` is `172.17.0.1/16`,
    /// so "prefer the same subnet" prefers the wrong address too, and
    /// "prefer 192.168/16" is wrong on every 10.x corporate LAN. The fix is
    /// that the caller tries all of them and lets the pinned certificate
    /// decide, so this test asserts the ordering it must cope with rather
    /// than an ordering that would be safe to dial blindly.
    #[test]
    fn bridge_addresses_still_sort_ahead_of_the_real_lan_address() {
        let mut a = ips(&["192.168.1.10", "172.17.0.1", "172.18.0.1"]);
        sort_by_connectability(&mut a);
        assert_eq!(
            a,
            ips(&["172.17.0.1", "172.18.0.1", "192.168.1.10"]),
            "if this ever changes, the caller may no longer need to try every address"
        );
        assert_ne!(
            a[0],
            "192.168.1.10".parse::<IpAddr>().unwrap(),
            "sorting alone does not make the first address the dialable one"
        );
    }
}

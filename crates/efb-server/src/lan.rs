//! The URLs a tablet on the same network can open.
//!
//! Ordered by how likely they are to work: the interface carrying the
//! default route first (the one the browser and the sim already use), then
//! other physical adapters, then the rest. Adapters that look like a LAN but
//! never reach a sibling device are dropped: the Windows Mobile Hotspot
//! bridge, Hyper-V and WSL switches, VM host-only adapters and VPN overlays.

use std::collections::HashSet;
use std::net::Ipv4Addr;

use netdev::interface::types::InterfaceType;

const UNREACHABLE_ADAPTERS: [&str; 16] = [
    "loopback",
    "vethernet",
    "wsl",
    "hyper-v",
    "virtualbox",
    "vmware",
    "vbox",
    "tailscale",
    "zerotier",
    "wireguard",
    "openvpn",
    "tap-windows",
    "tap adapter",
    "tunnel",
    "loopback pseudo",
    "bluetooth",
];

/// `http://<ip>:<port>/` per usable address, best first. Queries the OS;
/// cheap, but blocking.
pub fn lan_urls(port: u16) -> Vec<String> {
    let default_if = netdev::get_default_interface().ok().map(|i| i.name);
    let mut ranked: Vec<(u8, Ipv4Addr)> = Vec::new();
    for iface in netdev::get_interfaces() {
        let addrs: Vec<Ipv4Addr> = iface.ipv4.iter().map(|n| n.addr()).collect();
        let labels = [
            iface.friendly_name.as_deref().unwrap_or(""),
            iface.description.as_deref().unwrap_or(""),
            iface.name.as_str(),
        ];
        if !is_eligible(&labels, &addrs) {
            continue;
        }
        let tier = if default_if.as_deref() == Some(iface.name.as_str()) {
            0
        } else if matches!(
            iface.if_type,
            InterfaceType::Wireless80211 | InterfaceType::Ethernet
        ) {
            1
        } else {
            2
        };
        ranked.extend(
            addrs
                .into_iter()
                .filter(|a| is_usable(*a))
                .map(|a| (tier, a)),
        );
    }
    rank(ranked, port)
}

fn rank(mut ranked: Vec<(u8, Ipv4Addr)>, port: u16) -> Vec<String> {
    // Stable, so discovery order breaks ties; first tier seen wins a dupe.
    ranked.sort_by_key(|r| r.0);
    let mut seen = HashSet::new();
    ranked
        .into_iter()
        .filter(|(_, ip)| seen.insert(*ip))
        .map(|(_, ip)| format!("http://{ip}:{port}/"))
        .collect()
}

fn is_usable(ip: Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() && !ip.is_multicast()
}

/// `labels`: friendly name, description and system name, any of which may
/// carry the giveaway.
fn is_eligible(labels: &[&str], addrs: &[Ipv4Addr]) -> bool {
    if addrs.is_empty() {
        return false;
    }
    let labels: Vec<String> = labels.iter().map(|l| l.to_lowercase()).collect();
    if labels
        .iter()
        .any(|l| UNREACHABLE_ADAPTERS.iter().any(|n| l.contains(n)))
    {
        return false;
    }
    // Hotspot bridge: named "Local Area Connection* N" *and* on Microsoft's
    // 192.168.137.0/24. Either alone also matches real adapters.
    let star_name = labels
        .iter()
        .any(|l| l.starts_with("local area connection*"));
    let hotspot_subnet = addrs.iter().any(|a| a.octets()[..3] == [192, 168, 137]);
    !(star_name && hotspot_subnet)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 20);

    #[test]
    fn virtual_and_vpn_adapters_are_dropped() {
        assert!(is_eligible(&["Wi-Fi", "Intel AX200", "wlan0"], &[HOME]));
        assert!(!is_eligible(&["vEthernet (WSL)", "", ""], &[HOME]));
        assert!(!is_eligible(&["", "Tailscale Tunnel", ""], &[HOME]));
        assert!(!is_eligible(&["Wi-Fi", "", ""], &[]));
    }

    #[test]
    fn hotspot_bridge_needs_both_signals() {
        let hotspot = Ipv4Addr::new(192, 168, 137, 1);
        assert!(!is_eligible(
            &["Local Area Connection* 10", "", ""],
            &[hotspot]
        ));
        assert!(is_eligible(&["Local Area Connection* 10", "", ""], &[HOME]));
        assert!(is_eligible(&["Ethernet", "", ""], &[hotspot]));
    }

    #[test]
    fn default_route_first_without_duplicates() {
        let other = Ipv4Addr::new(10, 0, 0, 5);
        let urls = rank(vec![(1, other), (0, HOME), (2, other)], 28012);
        assert_eq!(
            urls,
            ["http://192.168.1.20:28012/", "http://10.0.0.5:28012/"]
        );
        assert!(!is_usable(Ipv4Addr::new(169, 254, 1, 1)));
        assert!(!is_usable(Ipv4Addr::LOCALHOST));
    }
}

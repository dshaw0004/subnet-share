use std::net::IpAddr;
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct IfaceInfo {
    pub name: String,
    pub ip: String,
    pub is_default: bool,
    /// Docker / WSL / VPN / virtual adapters (likely not what guests are on).
    pub virtual_hint: bool,
}

const VIRTUAL_PREFIXES: &[&str] = &[
    "docker", "veth", "br-", "virbr", "vethernet", "vmnet", "vboxnet", "wsl",
    "tailscale", "zt", "tun", "tap", "wg", "utun", "hyper-v", "vpn",
];

fn looks_virtual(label: &str) -> bool {
    let l = label.to_lowercase();
    VIRTUAL_PREFIXES.iter().any(|p| l.starts_with(p) || l.contains(&format!(" {p}")))
}

/// Usable IPv4 addresses: default-route interface first, then physical, then virtual.
pub fn list_interfaces() -> Vec<IfaceInfo> {
    let mut out = Vec::new();
    for i in netdev::get_interfaces() {
        if !i.is_up() || i.is_loopback() { continue; }
        let label = i.friendly_name.clone().unwrap_or_else(|| i.name.clone());
        let virt = !i.is_physical() || looks_virtual(&label) || looks_virtual(&i.name);
        for n in &i.ipv4 {
            let a = n.addr();
            if a.is_loopback() || a.is_link_local() || a.is_unspecified() { continue; }
            out.push(IfaceInfo { name: label.clone(), ip: a.to_string(), is_default: i.default, virtual_hint: virt });
        }
    }
    out.sort_by_key(|x| (!x.is_default, x.virtual_hint));
    out
}

/// Best guess for the LAN address (first entry of `list_interfaces`).
pub fn default_ip() -> Option<IpAddr> {
    list_interfaces().first().and_then(|i| i.ip.parse().ok())
}

use std::sync::Arc;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::state::{AppState, Peer};

const SERVICE: &str = "_subnetshare._tcp.local.";

/// Advertise this device on the LAN. Returns the daemon (keep it alive).
pub fn advertise(device_name: &str, port: u16) -> crate::anyhow_free::Result<ServiceDaemon> {
    let daemon = ServiceDaemon::new()?;
    let host = format!("{device_name}.local.");
    let ip = crate::net::default_ip()
        .map(|i| i.to_string())
        .unwrap_or_default();
    let info = ServiceInfo::new(
        SERVICE,
        device_name,
        &host,
        ip.as_str(),
        port,
        std::collections::HashMap::<String, String>::new(),
    )?
    .enable_addr_auto();
    daemon.register(info)?;
    Ok(daemon)
}

/// Browse for peers on the LAN. Runs until the daemon is dropped.
/// Updates `state.peers` as services appear and disappear.
pub fn browse(daemon: &ServiceDaemon, state: Arc<AppState>) -> crate::anyhow_free::Result<()> {
    let receiver = daemon.browse(SERVICE)?;
    tokio::spawn(async move {
        while let Ok(event) = receiver.recv_async().await {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    // Use the first IPv4 address reported; skip ourselves (same device_name)
                    if info.get_fullname().starts_with(&state.device_name) {
                        continue;
                    }
                    let Some(ip) = info.get_addresses_v4().into_iter().next() else { continue };
                    let name = info.get_fullname()
                        .trim_end_matches(SERVICE)
                        .trim_end_matches('.')
                        .to_string();
                    let peer = Peer { name, ip: ip.to_string(), port: info.get_port() };
                    state.upsert_peer(peer).await;
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    let name = fullname
                        .trim_end_matches(SERVICE)
                        .trim_end_matches('.')
                        .to_string();
                    state.remove_peer(&name).await;
                }
                _ => {}
            }
        }
    });
    Ok(())
}

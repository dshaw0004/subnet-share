//! Standalone core: discovery, HTTP server, transfers. No Tauri dependency.
pub mod config;
pub mod discovery;
pub mod net;
pub mod qr;
pub mod server;
pub mod state;
pub mod transfer;

use std::sync::Arc;
use config::Config;
use state::AppState;

pub struct Core {
    pub state: Arc<AppState>,
    pub port: u16,
    /// Keep the mDNS daemon alive for the process lifetime.
    _mdns: mdns_sd::ServiceDaemon,
}

impl Core {
    /// Bind (default port, then fallback range), start server + mDNS.
    pub async fn start(cfg: Config) -> anyhow_free::Result<Self> {
        let cfg = Arc::new(cfg);
        let state = Arc::new(AppState::new(cfg.clone()));
        let (listener, port) = server::bind(&cfg).await?;
        tokio::spawn(server::serve(listener, state.clone()));
        let daemon = discovery::advertise(&cfg.device_name, port)?;
        discovery::browse(&daemon, state.clone())?;
        Ok(Self { state, port, _mdns: daemon })
    }
}

pub mod anyhow_free {
    pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
}

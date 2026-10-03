mod inbox;
mod share;

use std::sync::Arc;
use axum::{routing::{get, post}, Router};
use rust_embed::RustEmbed;
use tokio::net::TcpListener;

use crate::{config::Config, state::AppState};
use inbox::RateLimiter;

#[derive(RustEmbed)]
#[folder = "../../ui/guest"]
#[include = "index.html"]
pub struct GuestAssets;

pub async fn bind(cfg: &Config) -> crate::anyhow_free::Result<(TcpListener, u16)> {
    let ports = std::iter::once(cfg.default_port).chain(cfg.fallback_range.clone());
    for p in ports {
        if let Ok(l) = TcpListener::bind(("0.0.0.0", p)).await {
            return Ok((l, p));
        }
    }
    Err("no free port in range".into())
}

pub async fn serve(listener: TcpListener, state: Arc<AppState>) {
    let rl = Arc::new(RateLimiter::default());
    let app = Router::new()
        // token-gated; 404 when no share is active
        .route("/share", get(share::handler))
        .route("/share/", get(share::handler))
        .route("/share/{*path}", get(share::handler))
        // offer metadata only; never serves files
        .route("/inbox", post(inbox::handler))
        .with_state((state, rl))
        .into_make_service_with_connect_info::<std::net::SocketAddr>();

    let _ = axum::serve(listener, app).await;
}

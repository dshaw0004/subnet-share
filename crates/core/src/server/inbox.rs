//! `/inbox` — accepts small offer metadata from remote senders.
//! Rate-limited per IP · validates sender URL host · queues silently when muted.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, State},
    http::StatusCode,
};
use std::net::SocketAddr;

use crate::state::{AppState, Offer};

const MAX_BODY: usize = 16 * 1024; // 16 KiB
const RATE_WINDOW: Duration = Duration::from_secs(60);
const RATE_LIMIT: u32 = 10; // offers per IP per minute

// ─── Rate limiter ──────────────────────────────────────────────────────────

#[derive(Default)]
pub struct RateLimiter(Mutex<HashMap<IpAddr, (u32, Instant)>>);

impl RateLimiter {
    /// Returns true if the request is allowed.
    pub fn check(&self, ip: IpAddr) -> bool {
        let mut map = self.0.lock().unwrap();
        let now = Instant::now();
        let entry = map.entry(ip).or_insert((0, now));
        // Reset window if expired
        if now.duration_since(entry.1) >= RATE_WINDOW {
            *entry = (0, now);
        }
        entry.0 += 1;
        entry.0 <= RATE_LIMIT
    }
}

// ─── Handler ───────────────────────────────────────────────────────────────

pub async fn handler(
    State((st, rl)): State<(Arc<AppState>, Arc<RateLimiter>)>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: String,
) -> StatusCode {
    // 1. Body size cap
    if body.len() > MAX_BODY {
        return StatusCode::PAYLOAD_TOO_LARGE;
    }

    // 2. Rate limit
    let peer_ip = addr.ip();
    if !rl.check(peer_ip) {
        return StatusCode::TOO_MANY_REQUESTS;
    }

    // 3. Parse JSON
    let offer: Offer = match serde_json::from_str(&body) {
        Ok(o) => o,
        Err(_) => return StatusCode::UNPROCESSABLE_ENTITY,
    };

    // 4. Validate: sender URL host must match the connecting IP
    if let Ok(url) = url::Url::parse(&offer.url) {
        let host_ok = url
            .host_str()
            .and_then(|h| h.parse::<IpAddr>().ok())
            .map(|h| h == peer_ip)
            .unwrap_or(false);
        if !host_ok {
            return StatusCode::FORBIDDEN;
        }
    } else {
        return StatusCode::UNPROCESSABLE_ENTITY;
    }

    // 5. Basic sanity on offer contents
    if offer.id.is_empty() || offer.files.is_empty() || offer.token.is_empty() {
        return StatusCode::UNPROCESSABLE_ENTITY;
    }

    // 6. Push to inbox (queues silently when muted)
    st.push_inbox(offer).await;
    StatusCode::OK
}

use std::{path::PathBuf, sync::Arc};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

// ─── Share session (Mode 1) ────────────────────────────────────────────────

pub struct ShareSession {
    pub root: PathBuf,
    pub token: String,
    pub open_mode: bool,   // never persisted; resets on restart
    pub show_hidden: bool, // dotfiles hidden by default
}

#[derive(Serialize, Clone)]
pub struct ShareInfo {
    pub path: String,
    pub token: String,
    pub open_mode: bool,
}

// ─── Mode 2 types ─────────────────────────────────────────────────────────

/// A single file in a transfer offer.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OfferFile {
    pub name: String,
    pub size: u64,
    /// Optional BLAKE3 hex digest chosen by the sender.
    pub hash: Option<String>,
}

/// The offer metadata broadcast to every peer's /inbox.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Offer {
    pub id: String,
    pub from_name: String,
    pub from_ip: String,
    pub files: Vec<OfferFile>,
    /// Full URL of the share root on the sender's server.
    pub url: String,
    pub token: String,
}

/// Offer as stored in this device's inbox (received from a remote sender).
#[derive(Serialize, Clone, Debug)]
pub struct InboxOffer {
    pub offer: Offer,
    /// Whether the offer arrived while muted (queued silently).
    pub queued: bool,
    pub status: InboxStatus,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InboxStatus {
    Pending,   // toast not yet acknowledged
    Accepted,
    Ignored,
    Downloading,
    Done,
    Failed(String),
    Cancelled,
}

/// Status of one receiver for an outgoing offer.
#[derive(Serialize, Clone, Debug)]
pub struct ReceiverStatus {
    pub peer_name: String,
    pub peer_ip: String,
    #[serde(flatten)]
    pub state: SendState,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum SendState {
    Pending,
    /// Inbox returned 200 OK.
    Delivered,
    /// Peer was unreachable / inbox returned error.
    Unreachable,
    /// Receiver accepted and is downloading.
    Downloading { bytes_done: u64, bytes_total: u64 },
    Done,
    Muted,
    Cancelled,
}

/// An outgoing offer tracked by the sender.
#[derive(Serialize, Clone, Debug)]
pub struct OutboxEntry {
    pub offer: Offer,
    pub receivers: Vec<ReceiverStatus>,
}

// ─── Discovered peer ──────────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug)]
pub struct Peer {
    pub name: String,
    pub ip: String,
    pub port: u16,
}

// ─── AppState ─────────────────────────────────────────────────────────────

pub struct AppState {
    pub device_name: String,
    pub share: RwLock<Option<ShareSession>>,
    pub muted: RwLock<bool>,
    /// Peer list updated by the mDNS browser.
    pub peers: RwLock<Vec<Peer>>,
    /// Received offers (populated by /inbox handler).
    pub inbox: Mutex<Vec<InboxOffer>>,
    /// Outgoing offers (populated by send_files).
    pub outbox: Mutex<Vec<OutboxEntry>>,
    /// Reference to the config for save().
    pub config: Arc<crate::config::Config>,
}

fn new_token() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(24)
        .map(char::from)
        .collect()
}

impl AppState {
    pub fn new(cfg: Arc<crate::config::Config>) -> Self {
        Self {
            device_name: cfg.device_name.clone(),
            share: RwLock::new(None),
            muted: RwLock::new(cfg.muted),
            peers: RwLock::new(Vec::new()),
            inbox: Mutex::new(Vec::new()),
            outbox: Mutex::new(Vec::new()),
            config: cfg,
        }
    }

    // ── Mode 1: share session ──────────────────────────────────────────────

    /// Starts (or replaces) the share. Always begins in protected mode.
    pub async fn start_share(&self, root: PathBuf) -> String {
        let token = new_token();
        *self.share.write().await = Some(ShareSession {
            root,
            token: token.clone(),
            open_mode: false,
            show_hidden: false,
        });
        token
    }

    pub async fn stop_share(&self) {
        *self.share.write().await = None;
    }

    pub async fn share_info(&self) -> Option<ShareInfo> {
        self.share.read().await.as_ref().map(|s| ShareInfo {
            path: s.root.to_string_lossy().into_owned(),
            token: s.token.clone(),
            open_mode: s.open_mode,
        })
    }

    /// New token; old links and guest cookies stop working immediately.
    pub async fn regenerate_token(&self) -> Option<String> {
        let mut g = self.share.write().await;
        let s = g.as_mut()?;
        s.token = new_token();
        Some(s.token.clone())
    }

    /// Returns false if nothing is being shared.
    pub async fn set_open_mode(&self, on: bool) -> bool {
        match self.share.write().await.as_mut() {
            Some(s) => {
                s.open_mode = on;
                true
            }
            None => false,
        }
    }

    // ── Peers ──────────────────────────────────────────────────────────────

    pub async fn get_peers(&self) -> Vec<Peer> {
        self.peers.read().await.clone()
    }

    pub async fn upsert_peer(&self, peer: Peer) {
        let mut list = self.peers.write().await;
        if let Some(p) = list.iter_mut().find(|p| p.name == peer.name) {
            *p = peer;
        } else {
            list.push(peer);
        }
    }

    pub async fn remove_peer(&self, name: &str) {
        self.peers.write().await.retain(|p| p.name != name);
    }

    // ── Mute ──────────────────────────────────────────────────────────────

    pub async fn set_muted(&self, on: bool) {
        *self.muted.write().await = on;
        // When unmuting, promote all queued-pending offers so they surface as toasts.
        if !on {
            let mut inbox = self.inbox.lock().await;
            for entry in inbox.iter_mut() {
                if entry.queued && entry.status == InboxStatus::Pending {
                    entry.queued = false;
                }
            }
        }
        self.config.save(on);
    }

    pub async fn is_muted(&self) -> bool {
        *self.muted.read().await
    }

    // ── Inbox ──────────────────────────────────────────────────────────────

    pub async fn push_inbox(&self, offer: Offer) {
        let queued = self.is_muted().await;
        self.inbox.lock().await.push(InboxOffer {
            offer,
            queued,
            status: InboxStatus::Pending,
        });
    }

    pub async fn get_inbox(&self) -> Vec<InboxOffer> {
        self.inbox.lock().await.clone()
    }

    /// Returns false if the offer id was not found.
    pub async fn accept_inbox(&self, offer_id: &str) -> bool {
        let mut inbox = self.inbox.lock().await;
        if let Some(e) = inbox.iter_mut().find(|e| e.offer.id == offer_id) {
            e.status = InboxStatus::Accepted;
            return true;
        }
        false
    }

    pub async fn ignore_inbox(&self, offer_id: &str) -> bool {
        let mut inbox = self.inbox.lock().await;
        if let Some(e) = inbox.iter_mut().find(|e| e.offer.id == offer_id) {
            e.status = InboxStatus::Ignored;
            return true;
        }
        false
    }

    pub async fn set_inbox_status(&self, offer_id: &str, status: InboxStatus) {
        let mut inbox = self.inbox.lock().await;
        if let Some(e) = inbox.iter_mut().find(|e| e.offer.id == offer_id) {
            e.status = status;
        }
    }

    // ── Outbox ─────────────────────────────────────────────────────────────

    pub async fn push_outbox(&self, entry: OutboxEntry) {
        self.outbox.lock().await.push(entry);
    }

    pub async fn get_outbox(&self) -> Vec<OutboxEntry> {
        self.outbox.lock().await.clone()
    }

    /// Update a single receiver's state within an offer.
    pub async fn set_receiver_state(&self, offer_id: &str, peer_ip: &str, state: SendState) {
        let mut outbox = self.outbox.lock().await;
        if let Some(entry) = outbox.iter_mut().find(|e| e.offer.id == offer_id) {
            if let Some(r) = entry.receivers.iter_mut().find(|r| r.peer_ip == peer_ip) {
                r.state = state;
            }
        }
    }

    /// Mark all receivers of an offer as Cancelled and return their ips.
    pub async fn cancel_outbox(&self, offer_id: &str) -> Vec<String> {
        let mut outbox = self.outbox.lock().await;
        let mut ips = Vec::new();
        if let Some(entry) = outbox.iter_mut().find(|e| e.offer.id == offer_id) {
            for r in &mut entry.receivers {
                r.state = SendState::Cancelled;
                ips.push(r.peer_ip.clone());
            }
        }
        ips
    }

    /// True if any outbox entry has an active receiver (used for quit guard).
    pub async fn has_active_transfers(&self) -> bool {
        let outbox = self.outbox.lock().await;
        outbox.iter().any(|e| {
            e.receivers.iter().any(|r| {
                matches!(r.state, SendState::Downloading { .. } | SendState::Delivered)
            })
        })
    }

    /// Generate a new offer id + token for a send session.
    pub fn new_offer_id() -> String {
        rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(16)
            .map(char::from)
            .collect()
    }
}

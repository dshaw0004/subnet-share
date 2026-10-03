//! Mode 2 transfer logic: broadcast offers, pull-download with resume, hash verify, cancel.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use reqwest::Client;
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncWriteExt, BufWriter},
};
use tokio_util::sync::CancellationToken;

use crate::{
    state::{
        AppState, InboxStatus, Offer, OfferFile, OutboxEntry, Peer, ReceiverStatus, SendState,
    },
};

const BROADCAST_TIMEOUT: Duration = Duration::from_secs(4);
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

// ─── Offer broadcast (sender side) ────────────────────────────────────────

/// Create an `OutboxEntry`, post the offer to every peer's /inbox (fire-and-forget
/// per peer with `BROADCAST_TIMEOUT`), track delivery status on the outbox.
pub async fn broadcast_offer(
    state: Arc<AppState>,
    offer: Offer,
    peers: Vec<Peer>,
) {
    let client = Client::builder()
        .timeout(BROADCAST_TIMEOUT)
        .build()
        .expect("reqwest client");

    let receivers: Vec<ReceiverStatus> = peers
        .iter()
        .map(|p| ReceiverStatus {
            peer_name: p.name.clone(),
            peer_ip: p.ip.clone(),
            state: SendState::Pending,
        })
        .collect();

    let entry = OutboxEntry {
        offer: offer.clone(),
        receivers,
    };
    state.push_outbox(entry).await;

    let offer_json = match serde_json::to_string(&offer) {
        Ok(j) => j,
        Err(_) => return,
    };

    for peer in peers {
        let state = state.clone();
        let client = client.clone();
        let json = offer_json.clone();
        let offer_id = offer.id.clone();

        tokio::spawn(async move {
            let url = format!("http://{}:{}/inbox", peer.ip, peer.port);
            let result = client
                .post(&url)
                .header("Content-Type", "application/json")
                .body(json)
                .send()
                .await;

            let new_state = match result {
                Ok(resp) if resp.status().is_success() => SendState::Delivered,
                Ok(resp) if resp.status().as_u16() == 429 => {
                    // peer rate-limited us — treat as muted from their side
                    SendState::Muted
                }
                _ => SendState::Unreachable,
            };

            state
                .set_receiver_state(&offer_id, &peer.ip, new_state)
                .await;
        });
    }
}

// ─── Accept & download (receiver side) ────────────────────────────────────

/// Download all files in `offer` into `save_dir`.
/// Supports range-based resume (HTTP 206). Updates inbox status as it goes.
/// Verifies BLAKE3 hash if the sender included one.
/// The `cancel` token lets callers abort mid-download.
pub async fn accept_offer(
    state: Arc<AppState>,
    offer: Offer,
    save_dir: PathBuf,
    cancel: CancellationToken,
) {
    state
        .set_inbox_status(&offer.id, InboxStatus::Downloading)
        .await;

    let client = Client::builder()
        .timeout(Duration::from_secs(0)) // streaming — per-read timeouts via stream
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .build()
        .expect("reqwest client");

    for file in &offer.files {
        if cancel.is_cancelled() {
            state
                .set_inbox_status(&offer.id, InboxStatus::Cancelled)
                .await;
            return;
        }

        let file_url = format!(
            "{}/{}&dl",
            offer.url.trim_end_matches('/'),
            urlencoding::encode(&file.name)
        );

        let result = download_file(
            &client,
            &file_url,
            &offer.token,
            file,
            &save_dir,
            &cancel,
        )
        .await;

        match result {
            Ok(path) => {
                // Hash verify (optional)
                if let Some(expected) = &file.hash {
                    if let Err(msg) = verify_blake3(&path, expected).await {
                        state
                            .set_inbox_status(&offer.id, InboxStatus::Failed(msg))
                            .await;
                        return;
                    }
                }
            }
            Err(e) => {
                state
                    .set_inbox_status(&offer.id, InboxStatus::Failed(e))
                    .await;
                return;
            }
        }
    }

    if cancel.is_cancelled() {
        state
            .set_inbox_status(&offer.id, InboxStatus::Cancelled)
            .await;
    } else {
        state
            .set_inbox_status(&offer.id, InboxStatus::Done)
            .await;
    }
}

// ─── Cancel ───────────────────────────────────────────────────────────────

/// Cancel an outgoing offer: marks all receivers Cancelled in the outbox.
/// (No HTTP call is needed — receivers simply stop getting updates.)
pub async fn cancel_send(state: Arc<AppState>, offer_id: &str) {
    state.cancel_outbox(offer_id).await;
}

// ─── Internals ─────────────────────────────────────────────────────────────

/// Download a single file with Range resume. Returns the local path on success.
async fn download_file(
    client: &Client,
    url: &str,
    token: &str,
    file: &OfferFile,
    save_dir: &Path,
    cancel: &CancellationToken,
) -> Result<PathBuf, String> {
    // Safe filename: strip any directory separators
    let safe_name = Path::new(&file.name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());

    let dest = save_dir.join(&safe_name);

    // Check how many bytes we already have (for resume)
    let existing = if dest.exists() {
        tokio::fs::metadata(&dest)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };

    if existing >= file.size {
        // Already complete — nothing to do
        return Ok(dest);
    }

    // Build request with optional Range header
    let mut req = client.get(url).header("Cookie", format!("lan_t={token}"));
    if existing > 0 {
        req = req.header("Range", format!("bytes={}-", existing));
    }

    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();

    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }

    // Open file: append if resuming, create/truncate otherwise
    let f = if existing > 0 && status.as_u16() == 206 {
        OpenOptions::new()
            .append(true)
            .open(&dest)
            .await
            .map_err(|e| e.to_string())?
    } else {
        File::create(&dest).await.map_err(|e| e.to_string())?
    };

    let mut writer = BufWriter::new(f);
    let mut stream = resp.bytes_stream();

    use futures_util::StreamExt as _;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                writer.flush().await.ok();
                return Err("cancelled".into());
            }
            item = stream.next() => {
                match item {
                    Some(result) => {
                        let chunk: bytes::Bytes = result.map_err(|e| e.to_string())?;
                        writer.write_all(&chunk).await.map_err(|e| e.to_string())?;
                    }
                    None => break,
                }
            }
        }
    }

    writer.flush().await.map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Verify the file at `path` against a hex-encoded BLAKE3 digest.
async fn verify_blake3(path: &Path, expected_hex: &str) -> Result<(), String> {
    let path = path.to_owned();
    let expected = expected_hex.to_owned();
    tokio::task::spawn_blocking(move || {
        let data = std::fs::read(&path).map_err(|e| e.to_string())?;
        let actual = blake3::hash(&data).to_hex().to_string();
        if actual == expected {
            Ok(())
        } else {
            Err(format!(
                "Hash mismatch for {}: expected {expected}, got {actual}",
                path.display()
            ))
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

// ─── Offer builder ─────────────────────────────────────────────────────────

/// Build an Offer from a list of file paths, with optional BLAKE3 hashing.
/// Returns the Offer and the share token to use.
pub async fn build_offer(
    from_name: &str,
    from_ip: &str,
    port: u16,
    share_token: &str,
    files: &[PathBuf],
    include_hashes: bool,
) -> Result<Offer, String> {
    let id = AppState::new_offer_id();
    let url = format!("http://{}:{}/share", from_ip, port);
    let mut offer_files = Vec::new();

    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| format!("Bad path: {}", path.display()))?;
        let meta = tokio::fs::metadata(path)
            .await
            .map_err(|e| e.to_string())?;
        let size = meta.len();
        let hash = if include_hashes {
            let p = path.clone();
            Some(
                tokio::task::spawn_blocking(move || {
                    let data = std::fs::read(&p)?;
                    Ok::<String, std::io::Error>(blake3::hash(&data).to_hex().to_string())
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e: std::io::Error| e.to_string())?,
            )
        } else {
            None
        };

        offer_files.push(OfferFile { name, size, hash });
    }

    Ok(Offer {
        id,
        from_name: from_name.to_string(),
        from_ip: from_ip.to_string(),
        files: offer_files,
        url,
        token: share_token.to_string(),
    })
}

# Subnet Share — Project Context

> Last updated: 2026-10-03

---

## 1. What It Is

Subnet Share is a lightweight, fully offline LAN file-sharing application for small offices and flat networks. It runs as a system tray app on Windows and Linux. There is no cloud, no account, and no internet dependency.

The core design principle is **pull-based security**: a receiver always explicitly accepts before any data enters their machine. The sender cannot push files uninvited.

Two operating modes:

- **Mode 1 — File Server**: owner picks a folder; the app serves it read-only over HTTP with a session token. Guests browse and download via a self-contained web page embedded in the binary.
- **Mode 2 — P2P Transfer**: sender picks files; the app broadcasts an offer manifest to every discovered peer; each receiver gets a toast notification and chooses to Accept or Ignore; accepted files download with range-based resume.

---

## 2. Repository Layout

```
lan-share-scaffold/
├── Cargo.toml                  # workspace root (members: crates/core, src-tauri)
├── package.json                # npm workspace root; `npm run dev` → node dev.mjs
├── dev.mjs                     # dev launcher (starts Vite, then tauri dev --config)
├── .vite-port                  # ephemeral: real Vite port written by writePortPlugin
├── .gitignore
│
├── crates/core/                # lan-core — pure Rust, no Tauri dependency
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # Core struct, startup wiring
│       ├── config.rs           # Config struct, disk persistence
│       ├── state.rs            # AppState and all shared types
│       ├── discovery.rs        # mDNS advertise + browse
│       ├── net.rs              # interface enumeration
│       ├── qr.rs               # QR code → SVG
│       ├── transfer.rs         # broadcast_offer, accept_offer, verify_blake3
│       └── server/
│           ├── mod.rs          # axum router, port binding
│           ├── share.rs        # /share/... handler (Mode 1 file server)
│           └── inbox.rs        # /inbox handler (Mode 2 offer receiver)
│
├── src-tauri/                  # lan-app — Tauri shell
│   ├── Cargo.toml
│   ├── build.rs
│   ├── tauri.conf.json
│   ├── capabilities/default.json
│   ├── icons/
│   │   ├── icon.png            # 1024×1024 RGBA — Subnet Share brand icon
│   │   └── subset-share.svg    # source SVG
│   └── src/
│       └── main.rs             # all Tauri IPC commands + tray setup
│
└── ui/
    ├── owner/                  # Vue 3 + TypeScript owner UI (built by Vite)
    │   ├── package.json
    │   ├── vite.config.ts      # writePortPlugin, strictPort: false
    │   ├── tsconfig.json
    │   ├── index.html
    │   └── src/
    │       ├── main.ts
    │       ├── env.d.ts
    │       ├── api.ts          # all Tauri invoke() wrappers + TypeScript types
    │       └── App.vue         # three-tab UI: Share / Send / Inbox
    └── guest/
        └── index.html          # self-contained guest browser page (no build step)
```

---

## 3. Tech Stack

| Layer | Technology | Version |
|---|---|---|
| Desktop shell | Tauri | 2 |
| Core runtime | Rust / Tokio | 1 (async, full) |
| HTTP server | Axum | 0.8 |
| mDNS | mdns-sd | 0.13 |
| Network interfaces | netdev | 0.46 |
| HTTP client | reqwest (rustls) | 0.12 |
| File hashing | blake3 | 1 |
| QR code | qrcode | 0.14 |
| ZIP streaming | zip | 2 |
| Asset embedding | rust-embed | 8 |
| Owner UI framework | Vue | 3.5 |
| Owner UI build | Vite | 6 |
| Owner UI language | TypeScript | 5 |
| Tauri dialog plugin | tauri-plugin-dialog | 2 |
| Tauri process plugin | tauri-plugin-process | 2 |

---

## 4. How the Dev Server Works

`npm run dev` runs `node dev.mjs`, not `tauri dev` directly. This is because Vite's port (default 5173) may be occupied, and Tauri's `devUrl` in `tauri.conf.json` is a static string.

The flow:

1. `dev.mjs` deletes any stale `.vite-port` file.
2. `dev.mjs` spawns `npm run dev --workspace ui/owner` (Vite).
3. Vite's `writePortPlugin` (in `vite.config.ts`) writes the actual bound port to `.vite-port` the moment the HTTP server starts listening.
4. `dev.mjs` polls `.vite-port` every 200 ms (30 s timeout).
5. Once the port is known, `dev.mjs` spawns:
   ```
   npx tauri dev --config '{"build":{"devUrl":"http://localhost:<PORT>","beforeDevCommand":""}}'
   ```
   The `--config` flag merges inline JSON over `tauri.conf.json`. `beforeDevCommand` is blanked so Tauri doesn't try to start a second Vite instance.
6. Both processes are linked: if either exits, the other is killed.

For production builds: `npm run build` → `tauri build`, which runs `npm run build -w ui/owner` (vue-tsc + vite build) then bundles the Rust binary.

---

## 5. Core Architecture (`crates/core`)

### 5.1 Startup (`lib.rs`)

```rust
pub struct Core {
    pub state: Arc<AppState>,
    pub port: u16,
    _mdns: mdns_sd::ServiceDaemon,  // kept alive for process lifetime
}
```

`Core::start(cfg)`:
1. Wraps `Config` in `Arc`.
2. Creates `AppState`.
3. Binds TCP (port 48080, fallback 48081–48099).
4. Spawns the Axum server.
5. Calls `discovery::advertise` (registers mDNS service).
6. Calls `discovery::browse` (spawns async task to update `state.peers`).

### 5.2 Config (`config.rs`)

`Config` is loaded via `Config::default()`:
- Reads `~/.config/subnet-share/config.json` (Linux) or `%APPDATA%\subnet-share\config.json` (Windows).
- Persisted fields: `device_name` (defaults to system hostname), `muted`.
- Non-persisted: `default_port`, `fallback_range`, `save_dir` (defaults to `~/Downloads`).
- `Config::save(muted)` writes back to disk (best-effort, silent on error).

### 5.3 State (`state.rs`)

All runtime state lives in `AppState`, wrapped in `Arc` and shared across the Axum server, mDNS browser, transfer tasks, and Tauri IPC.

```
AppState {
    device_name: String,
    share:   RwLock<Option<ShareSession>>,   // Mode 1
    muted:   RwLock<bool>,
    peers:   RwLock<Vec<Peer>>,              // updated by mDNS browse
    inbox:   Mutex<Vec<InboxOffer>>,         // received offers
    outbox:  Mutex<Vec<OutboxEntry>>,        // sent offers + per-receiver status
    config:  Arc<Config>,                    // for save()
}
```

Key types:

```
ShareSession  { root, token, open_mode, show_hidden }
Offer         { id, from_name, from_ip, files: Vec<OfferFile>, url, token }
OfferFile     { name, size, hash: Option<String> }   // hash = BLAKE3 hex, optional
InboxOffer    { offer, queued, status: InboxStatus }
InboxStatus   = Pending | Accepted | Ignored | Downloading | Done | Failed(String) | Cancelled
OutboxEntry   { offer, receivers: Vec<ReceiverStatus> }
ReceiverStatus { peer_name, peer_ip, state: SendState }
SendState     = Pending | Delivered | Unreachable | Downloading{bytes_done,bytes_total} | Done | Muted | Cancelled
Peer          { name, ip, port }
```

### 5.4 HTTP Server (`server/`)

Single TCP listener, two route namespaces:

| Route | Method | Purpose |
|---|---|---|
| `/share` `/share/` `/share/{*path}` | GET | Token-gated file server (Mode 1) |
| `/inbox` | POST | Offer receiver (Mode 2) |

**`/share` handler** (`server/share.rs`):
- Returns 404 immediately when no share is active.
- `?t=<token>` → sets `lan_t` cookie → 303 redirect to clean URL (constant-time comparison).
- Auth: cookie must match current token, unless `open_mode` is on.
- Path safety: canonicalization, symlink escape prevention (`starts_with(root)`), hidden file filtering, `..` / null byte / drive letter rejection.
- Directory: `?json` → JSON listing, `?zip` → streamed ZIP (stored, no temp file), default → embedded guest `index.html`.
- File: served via `tower_http::ServeFile` (range requests, ETag, MIME); `Content-Security-Policy: sandbox`; `?dl` forces download with RFC 5987 filename encoding.

**`/inbox` handler** (`server/inbox.rs`):
- Body capped at 16 KiB.
- Rate limiter: 10 offers/IP/minute, sliding window, in-memory.
- Parses JSON into `Offer`.
- Validates that `offer.url` host == connecting IP (prevents spoofed URLs).
- Sanity checks: non-empty id, files, token.
- Calls `state.push_inbox(offer)` — if muted, `queued` is set to `true`.

### 5.5 Discovery (`discovery.rs`)

- `advertise(device_name, port)`: registers `_subnetshare._tcp.local.` via mdns-sd. Returns the `ServiceDaemon` (must be kept alive — stored in `Core._mdns`).
- `browse(daemon, state)`: subscribes to the same service type. On `ServiceResolved`: upserts peer (skips self by device name match). On `ServiceRemoved`: removes peer. Runs in a background async task.

### 5.6 Network Interface Picker (`net.rs`)

`list_interfaces()` enumerates IPv4 interfaces:
- Skips loopback, link-local, unspecified.
- Detects virtual adapters (Docker, WSL, VPN, etc.) by name prefix.
- Sorts: default-route interface first, then physical, then virtual.
- Returns `Vec<IfaceInfo>` with `name`, `ip`, `is_default`, `virtual_hint`.

### 5.7 Transfer (`transfer.rs`)

**Sender side — `broadcast_offer`**:
1. Builds `OutboxEntry` with all receivers at `SendState::Pending`.
2. Pushes to `state.outbox`.
3. Serialises offer to JSON once.
4. Spawns one task per peer: POST to `http://<peer_ip>:<peer_port>/inbox` with 4 s timeout.
5. Updates receiver state: `Delivered` (2xx), `Muted` (429), `Unreachable` (error or other status).

**`build_offer`**: assembles `Offer` from file paths. Optionally computes BLAKE3 in a `spawn_blocking` task per file (hashing large files on the async executor would block it).

**Receiver side — `accept_offer`**:
1. Sets `InboxStatus::Downloading`.
2. For each file in the offer:
   - Constructs `<url>/<filename>?dl` with a `Cookie: lan_t=<token>` header.
   - Checks existing file size for resume; sends `Range: bytes=<n>-` if partial.
   - Opens file in append mode if HTTP 206, create/truncate otherwise.
   - Streams response body to `BufWriter`, respecting a `CancellationToken`.
   - After download: if `OfferFile.hash` is `Some`, calls `verify_blake3` (reads file, computes hash in `spawn_blocking`, compares hex strings). Failure sets `InboxStatus::Failed`.
3. Final status: `Done` or `Cancelled`.

**`cancel_send`**: marks all receivers of an outbox entry as `Cancelled`. No HTTP call needed — the file server simply serves no more requests with that token.

---

## 6. Tauri IPC Commands (`src-tauri/src/main.rs`)

### Mode 1

| Command | Signature | Description |
|---|---|---|
| `status` | `() → Status` | port, device_name, share info, muted flag |
| `interfaces` | `() → Vec<IfaceInfo>` | network adapter list |
| `start_share` | `(path: String) → String` | start sharing a folder; returns token |
| `stop_share` | `() → ()` | stop sharing |
| `regenerate_token` | `() → String` | new token; old sessions invalidated |
| `set_open_mode` | `(enabled: bool) → bool` | toggle token-free access |
| `qr_svg` | `(text: String) → String` | generate QR as inline SVG |

### Mode 2

| Command | Signature | Description |
|---|---|---|
| `get_peers` | `() → Vec<Peer>` | currently discovered peers |
| `send_files` | `(file_paths: Vec<String>, include_hashes: bool) → String` | build offer, start share if needed, broadcast; returns offer_id |
| `get_inbox` | `() → Vec<InboxOffer>` | all received offers |
| `get_outbox` | `() → Vec<OutboxEntry>` | all sent offers with per-receiver status |
| `accept_offer` | `(offer_id: String) → ()` | mark accepted, spawn download task |
| `ignore_offer` | `(offer_id: String) → ()` | mark ignored |
| `cancel_send` | `(offer_id: String) → ()` | mark all receivers Cancelled |
| `cancel_receive` | `(offer_id: String) → ()` | cancel active download via CancellationToken |
| `set_muted` | `(muted: bool) → ()` | toggle mute; persists to config |

### Tray & Lifecycle

- Tray menu: Open / Quit.
- Close window → webview is destroyed; core keeps running.
- "Open" from tray: shows existing window or recreates the webview.
- Quit with active transfers: emits `quit-requested` event to the webview window. The UI shows a confirm dialog; on confirm it calls `@tauri-apps/plugin-process` `exit(0)`.

---

## 7. Owner UI (`ui/owner/src/`)

Built with Vue 3 Composition API + TypeScript. No CSS framework — all styles are inline in `App.vue`.

### Three tabs

**Share tab** (Mode 1):
- Choose folder button (Tauri dialog).
- Network interface picker (dropdown, rescan button, virtual adapter warning).
- Share URL input (readonly, copy button).
- QR code (inline SVG, regenerated on URL change).
- Open mode toggle with confirmation dialog and persistent warning banner.
- New link / Stop sharing buttons.

**Send tab** (Mode 2 sender):
- Live peer list polled every 2 s; checkbox selection.
- BLAKE3 hash toggle.
- "Pick files & send to all peers" button (Tauri file picker, multiple).
- Outbox: each sent offer shows filename(s) and a row per receiver with status badge (Pending / Delivered / Unreachable / Muted / Done / Cancelled).
- Cancel button per offer.

**Inbox tab** (Mode 2 receiver):
- Mute toggle (persists via `set_muted`); muted badge in header.
- Toast stack at top of page for unacknowledged incoming offers: sender name, IP, file list with "⚠ no hash" warning on each file lacking a hash, Accept / Ignore buttons.
- Full inbox list below: status per offer, cancel download button for active downloads.

### Polling

`refreshMode2()` runs every 2 seconds on a `setInterval`. It compares incoming inbox IDs against the previous state to detect new arrivals and push them to the toast stack. Status updates (download progress etc.) are reflected on the next poll.

---

## 8. Guest Page (`ui/guest/index.html`)

A single self-contained HTML file embedded into the binary via `rust-embed`. No build step, no CDN, no external requests.

Features:
- JSON directory listing via `fetch(base + "?json")`.
- Breadcrumb navigation.
- Grid / list view toggle (persisted to `localStorage`).
- Search (client-side filter).
- Sort by name / size / modified, ascending/descending.
- Image thumbnails (lazy-loaded).
- Preview panel for images, video, audio, text (text preview capped at 200 KB via `Range` header).
- Keyboard navigation in preview: arrow keys, Escape.
- Per-file download button (`?dl`).
- "Download all as ZIP" button (`?zip`) in header.
- 401/403 shown as friendly error message.

---

## 9. Security Properties

| Property | Implementation |
|---|---|
| Token comparison | Constant-time (`fold` with XOR accumulator) |
| Path traversal | `canonicalize` + `starts_with(root)` check |
| Symlink escape | Canonicalization + symlink skip in ZIP walk |
| Hidden files | Filtered at listing and path resolution layers |
| Served file isolation | `Content-Security-Policy: sandbox` header |
| Open mode | Explicit toggle with confirmation dialog; reverts on restart (not persisted) |
| Inbox spoofing | `offer.url` host validated against TCP connecting IP |
| Inbox flood | Rate limiter: 10 offers/IP/60 s |
| Inbox payload | Hard cap: 16 KiB body |
| File integrity | Optional BLAKE3 hash; "no hash" warning shown when absent |
| Virtual adapters | Detected and flagged with a warning in the UI |

---

## 10. What Is Not Yet Done (Deferred to Later Versions)

Per the v1 spec, these are explicitly out of scope:

- VLAN / cross-subnet support
- HTTPS / TLS
- Link expiry
- Per-guest revoke
- Access log
- Per-sender mute / timed mute
- Push transfer (sender decides, no receiver consent)
- Phone client
- Native UI (replaces webview)
- Sniffing protection

Known gaps vs v1 spec that are not explicitly deferred but not yet implemented:

- **Filename conflict prompt**: the spec says ask the receiver on conflict; currently the download overwrites or resumes based on file size alone.
- **Download progress reporting**: `SendState::Downloading { bytes_done, bytes_total }` exists in the type system and the UI renders it, but `accept_offer` does not update it mid-download (it jumps from `Downloading` to `Done`).
- **Sender sees receiver downloading status**: `ReceiverStatus` transitions only go to `Delivered` (offer accepted by inbox), not to `Downloading`/`Done` based on actual download progress. A push notification from receiver back to sender is not implemented.

---

## 11. Running the Project

**Prerequisites**: Rust stable, Node 20+, Tauri 2 system deps (on Linux: `webkit2gtk-4.1`, `libgtk-3-dev`, `libayatana-appindicator3-dev`).

```bash
# Install JS deps
npm install

# Dev mode (hot reload)
npm run dev

# Production build
npm run build
```

The dev script (`node dev.mjs`) handles port negotiation automatically. If 5173 is busy, Vite picks the next free port and `dev.mjs` passes it to Tauri via `--config`.

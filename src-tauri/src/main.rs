#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use lan_core::{
    config::Config,
    net::IfaceInfo,
    state::{InboxOffer, OutboxEntry, Peer, ShareInfo},
    transfer, Core,
};
use tauri::{
    async_runtime,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder,
};
use tokio_util::sync::CancellationToken;

// ─── Managed state ────────────────────────────────────────────────────────

struct CoreHandle(Arc<Core>);

/// Active download cancellation tokens, keyed by offer_id.
struct Downloads(Mutex<HashMap<String, CancellationToken>>);

// ─── Shared types for IPC ─────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct Status {
    port: u16,
    device_name: String,
    share: Option<ShareInfo>,
    muted: bool,
}

// ─── Mode 1 commands ──────────────────────────────────────────────────────

#[tauri::command]
async fn status(core: State<'_, CoreHandle>) -> Result<Status, String> {
    Ok(Status {
        port: core.0.port,
        device_name: core.0.state.device_name.clone(),
        share: core.0.state.share_info().await,
        muted: core.0.state.is_muted().await,
    })
}

#[tauri::command]
fn interfaces() -> Vec<IfaceInfo> {
    lan_core::net::list_interfaces()
}

#[tauri::command]
async fn start_share(core: State<'_, CoreHandle>, path: String) -> Result<String, String> {
    if !PathBuf::from(&path).is_dir() {
        return Err("That is not a folder".into());
    }
    let token = core.0.state.start_share(path.clone().into()).await;
    // Persist so the session survives closing the window.
    core.0.state.config.save_share(std::path::Path::new(&path), &token);
    Ok(token)
}

#[tauri::command]
async fn stop_share(core: State<'_, CoreHandle>) -> Result<(), String> {
    core.0.state.stop_share().await;
    // Clear persistence so we don't restore a stopped share on next launch.
    core.0.state.config.clear_share();
    Ok(())
}

#[tauri::command]
async fn regenerate_token(core: State<'_, CoreHandle>) -> Result<String, String> {
    let token = core.0.state.regenerate_token().await
        .ok_or_else(|| "Not sharing".to_string())?;
    // Update persisted token so restart still uses the new one.
    if let Some(info) = core.0.state.share_info().await {
        core.0.state.config.save_share(std::path::Path::new(&info.path), &token);
    }
    Ok(token)
}

#[tauri::command]
async fn set_open_mode(core: State<'_, CoreHandle>, enabled: bool) -> Result<bool, String> {
    Ok(core.0.state.set_open_mode(enabled).await)
}

#[tauri::command]
fn qr_svg(text: String) -> Result<String, String> {
    lan_core::qr::svg(&text)
}

// ─── Mode 2 commands ──────────────────────────────────────────────────────

#[tauri::command]
async fn get_peers(core: State<'_, CoreHandle>) -> Result<Vec<Peer>, String> {
    Ok(core.0.state.get_peers().await)
}

/// Pick files and broadcast an offer to all discovered peers.
/// `file_paths`  – list of absolute paths chosen via the file picker.
/// `include_hashes` – whether to compute BLAKE3 digests (slow for large files).
#[tauri::command]
async fn send_files(
    core: State<'_, CoreHandle>,
    file_paths: Vec<String>,
    include_hashes: bool,
) -> Result<String, String> {
    let paths: Vec<PathBuf> = file_paths.iter().map(PathBuf::from).collect();

    // Verify all paths exist and are files
    for p in &paths {
        if !p.is_file() {
            return Err(format!("{} is not a file", p.display()));
        }
    }

    let peers = core.0.state.get_peers().await;
    if peers.is_empty() {
        return Err("No peers discovered on the network".into());
    }

    let ip = lan_core::net::default_ip()
        .map(|i| i.to_string())
        .ok_or_else(|| "No usable network address".to_string())?;

    // Ensure a share session exists (creates a scoped share for these files)
    let share_token = {
        let si = core.0.state.share_info().await;
        match si {
            Some(s) => s.token,
            None => {
                // Use the parent directory of the first file as the share root
                let parent = paths[0]
                    .parent()
                    .ok_or("Cannot determine share folder")?
                    .to_path_buf();
                core.0.state.start_share(parent).await
            }
        }
    };

    let offer = transfer::build_offer(
        &core.0.state.device_name,
        &ip,
        core.0.port,
        &share_token,
        &paths,
        include_hashes,
    )
    .await?;

    let offer_id = offer.id.clone();
    transfer::broadcast_offer(core.0.state.clone(), offer, peers).await;
    Ok(offer_id)
}

#[tauri::command]
async fn get_inbox(core: State<'_, CoreHandle>) -> Result<Vec<InboxOffer>, String> {
    Ok(core.0.state.get_inbox().await)
}

#[tauri::command]
async fn get_outbox(core: State<'_, CoreHandle>) -> Result<Vec<OutboxEntry>, String> {
    Ok(core.0.state.get_outbox().await)
}

/// Accept an inbox offer: mark it as accepted then kick off the download.
#[tauri::command]
async fn accept_offer(
    core: State<'_, CoreHandle>,
    downloads: State<'_, Downloads>,
    offer_id: String,
) -> Result<(), String> {
    let inbox = core.0.state.get_inbox().await;
    let entry = inbox
        .into_iter()
        .find(|e| e.offer.id == offer_id)
        .ok_or_else(|| "Offer not found".to_string())?;

    core.0.state.accept_inbox(&offer_id).await;

    let cancel = CancellationToken::new();
    downloads
        .0
        .lock()
        .unwrap()
        .insert(offer_id.clone(), cancel.clone());

    let state = core.0.state.clone();
    let save_dir = core.0.state.config.save_dir.clone();

    tokio::spawn(async move {
        transfer::accept_offer(state, entry.offer, save_dir, cancel).await;
    });

    Ok(())
}

#[tauri::command]
async fn ignore_offer(
    core: State<'_, CoreHandle>,
    offer_id: String,
) -> Result<(), String> {
    if !core.0.state.ignore_inbox(&offer_id).await {
        return Err("Offer not found".into());
    }
    Ok(())
}

/// Cancel an outgoing offer (sender side).
#[tauri::command]
async fn cancel_send(
    core: State<'_, CoreHandle>,
    offer_id: String,
) -> Result<(), String> {
    transfer::cancel_send(core.0.state.clone(), &offer_id).await;
    Ok(())
}

/// Cancel an incoming download (receiver side).
#[tauri::command]
async fn cancel_receive(
    downloads: State<'_, Downloads>,
    offer_id: String,
) -> Result<(), String> {
    if let Some(token) = downloads.0.lock().unwrap().remove(&offer_id) {
        token.cancel();
    }
    Ok(())
}

#[tauri::command]
async fn set_muted(core: State<'_, CoreHandle>, muted: bool) -> Result<(), String> {
    core.0.state.set_muted(muted).await;
    Ok(())
}

// ─── Window helpers ───────────────────────────────────────────────────────

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    // Webview was destroyed on close; recreate it. Core keeps running.
    let _ = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
        .title("Subnet Share")
        .inner_size(560.0, 720.0)
        .build();
}

// ─── Entry point ──────────────────────────────────────────────────────────

fn main() {
    let core = async_runtime::block_on(Core::start(Config::default()))
        .expect("failed to start core");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .manage(CoreHandle(Arc::new(core)))
        .manage(Downloads(Mutex::new(HashMap::new())))
        .invoke_handler(tauri::generate_handler![
            // Mode 1
            status,
            interfaces,
            start_share,
            stop_share,
            regenerate_token,
            set_open_mode,
            qr_svg,
            // Mode 2
            get_peers,
            send_files,
            get_inbox,
            get_outbox,
            accept_offer,
            ignore_offer,
            cancel_send,
            cancel_receive,
            set_muted,
        ])
        .setup(|app| {
            let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, e| match e.id.as_ref() {
                    "open" => show_window(app),
                    "quit" => {
                        // Warn if transfers are active
                        let core = app.state::<CoreHandle>();
                        let state = core.0.state.clone();
                        let app = app.clone();
                        async_runtime::spawn(async move {
                            if state.has_active_transfers().await {
                                // Surface a dialog. If the user confirms, exit.
                                if let Some(w) = app.get_webview_window("main") {
                                    let _ = w.show();
                                    let _ = w.set_focus();
                                    // Emit an event the UI can intercept to show the warning
                                    let _ = w.emit("quit-requested", ());
                                } else {
                                    // No window — just quit
                                    app.exit(0);
                                }
                            } else {
                                app.exit(0);
                            }
                        });
                    }
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building app")
        .run(|_app, event| {
            // Closing the last window must not exit the app (tray keeps core alive)
            if let RunEvent::ExitRequested { code: None, api, .. } = event {
                api.prevent_exit();
            }
        });
}

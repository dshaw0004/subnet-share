# Subnet Share
Prereqs: Rust, Node 20+, Tauri 2 system deps (webkit2gtk on Linux).
```
npm install
mkdir -p src-tauri/icons && npx tauri icon <1024px-png>   # generates icons
npm run dev
```
Layout: `crates/core` (server, mDNS, transfers) · `src-tauri` (tray, IPC) · `ui/owner` (Vue) · `ui/guest/index.html` (single-file guest page, embedded as-is, no build)

---
# SubnetShare
A lightweight, fully offline LAN file sharing application for small offices and flat networks.  
Unlike cloud-dependent tools or push-based utilities that force files onto your machine, SubnetShare uses a secure, pull-based model. It combines a read-only local File Server with a P2P offer-manifest system, ensuring the receiver always has the final say on what enters their system.  
Key Features:  

 -  🌐 LAN Only & Fully Offline: Uses mDNS/DNS-SD for zero-config peer discovery.  
 -  🛡️ Pull-Based Security: Receivers explicitly accept offer manifests before any data transfers.  
 -  📂 Dual Mode: Instant read-only Folder Sharing (with ZIP download) or direct P2P file transfers.  
 -  ⚡ Lightweight: Built with Rust and Tauri, running as a discreet system tray application with a single listening port.

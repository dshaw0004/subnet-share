import { invoke } from '@tauri-apps/api/core'

// ── Mode 1 types ──────────────────────────────────────────────────────────

export interface Iface { name: string; ip: string; is_default: boolean; virtual_hint: boolean }
export interface ShareInfo { path: string; token: string; open_mode: boolean }
export interface Status {
  port: number
  device_name: string
  share: ShareInfo | null
  muted: boolean
}

// ── Mode 2 types ──────────────────────────────────────────────────────────

export interface Peer { name: string; ip: string; port: number }

export interface OfferFile { name: string; size: number; hash: string | null }
export interface Offer {
  id: string
  from_name: string
  from_ip: string
  files: OfferFile[]
  url: string
  token: string
}

export type InboxStatus =
  | 'pending'
  | 'accepted'
  | 'ignored'
  | 'downloading'
  | 'done'
  | 'cancelled'
  | { failed: string }

export interface InboxOffer { offer: Offer; queued: boolean; status: InboxStatus }

export type SendState =
  | { status: 'pending' }
  | { status: 'delivered' }
  | { status: 'unreachable' }
  | { status: 'downloading'; bytes_done: number; bytes_total: number }
  | { status: 'done' }
  | { status: 'muted' }
  | { status: 'cancelled' }

export interface ReceiverStatus { peer_name: string; peer_ip: string; status: SendState }
export interface OutboxEntry { offer: Offer; receivers: ReceiverStatus[] }

// ── API ───────────────────────────────────────────────────────────────────

export const api = {
  // Mode 1
  status: () => invoke<Status>('status'),
  interfaces: () => invoke<Iface[]>('interfaces'),
  startShare: (path: string) => invoke<string>('start_share', { path }),
  stopShare: () => invoke<void>('stop_share'),
  regenerate: () => invoke<string>('regenerate_token'),
  setOpenMode: (enabled: boolean) => invoke<boolean>('set_open_mode', { enabled }),
  qr: (text: string) => invoke<string>('qr_svg', { text }),
  // Mode 2
  getPeers: () => invoke<Peer[]>('get_peers'),
  sendFiles: (filePaths: string[], includeHashes: boolean) =>
    invoke<string>('send_files', { filePaths, includeHashes }),
  getInbox: () => invoke<InboxOffer[]>('get_inbox'),
  getOutbox: () => invoke<OutboxEntry[]>('get_outbox'),
  acceptOffer: (offerId: string) => invoke<void>('accept_offer', { offerId }),
  ignoreOffer: (offerId: string) => invoke<void>('ignore_offer', { offerId }),
  cancelSend: (offerId: string) => invoke<void>('cancel_send', { offerId }),
  cancelReceive: (offerId: string) => invoke<void>('cancel_receive', { offerId }),
  setMuted: (muted: boolean) => invoke<void>('set_muted', { muted }),
}

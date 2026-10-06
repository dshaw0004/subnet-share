<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue'
import { open, confirm, message } from '@tauri-apps/plugin-dialog'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { api, type Iface, type Status, type Peer, type InboxOffer, type OutboxEntry, type SendState } from './api'

// ─── State ────────────────────────────────────────────────────────────────

const status   = ref<Status | null>(null)
const ifaces   = ref<Iface[]>([])
const selectedIp = ref('')
const qrSvg    = ref('')
const err      = ref('')
const copied   = ref(false)
const tab      = ref<'share' | 'send' | 'inbox'>('share')

// Mode 2
const peers    = ref<Peer[]>([])
const outbox   = ref<OutboxEntry[]>([])
const inbox    = ref<InboxOffer[]>([])
const toasts   = ref<InboxOffer[]>([]) // pending inbox items not yet acknowledged
const selectedPeers = ref<Set<string>>(new Set())
const includeHashes = ref(false)
const sending  = ref(false)

const share = computed(() => status.value?.share ?? null)
const muted = computed(() => status.value?.muted ?? false)

const pendingInboxCount = computed(() =>
  inbox.value.filter(e => e.status === 'pending' || e.queued).length
)

const url = computed(() => {
  if (!share.value || !selectedIp.value || !status.value) return ''
  const base = `http://${selectedIp.value}:${status.value.port}/share`
  return share.value.open_mode ? base : `${base}?t=${share.value.token}`
})

const ifaceLabel = (i: Iface) =>
  `${i.name} — ${i.ip}${i.is_default ? ' (default)' : ''}${i.virtual_hint ? ' (virtual?)' : ''}`

// ─── Helpers ──────────────────────────────────────────────────────────────

async function run<T>(fn: () => Promise<T>): Promise<T | undefined> {
  err.value = ''
  try { return await fn() } catch (e) { err.value = String(e) }
}

function fmtSize(b: number): string {
  if (!b) return '0 B'
  const u = ['B', 'KB', 'MB', 'GB', 'TB']
  let i = 0
  while (b >= 1024 && i < 4) { b /= 1024; i++ }
  return `${i ? b.toFixed(1) : b} ${u[i]}`
}

function statusLabel(s: SendState | string): string {
  if (typeof s === 'object' && 'status' in s) {
    const st = (s as any).status
    if (st === 'downloading') {
      const { bytes_done, bytes_total } = s as any
      const pct = bytes_total > 0 ? Math.round(bytes_done / bytes_total * 100) : 0
      return `${pct}% (${fmtSize(bytes_done)} / ${fmtSize(bytes_total)})`
    }
    return st
  }
  if (typeof s === 'string') return s
  return '?'
}

function inboxStatusLabel(s: InboxOffer['status']): string {
  if (typeof s === 'object' && 'failed' in (s as any)) return `Failed: ${(s as any).failed}`
  return String(s)
}

// ─── Refresh ──────────────────────────────────────────────────────────────

const refresh = async () => {
  status.value = (await run(api.status)) ?? status.value
}

const refreshMode2 = async () => {
  peers.value = (await run(api.getPeers)) ?? peers.value
  outbox.value = (await run(api.getOutbox)) ?? outbox.value
  const newInbox = (await run(api.getInbox)) ?? inbox.value

  // Find newly arrived pending items to promote to toasts.
  // Also catch offers that were queued (arrived while muted) and are now promoted.
  const existingMap = new Map(inbox.value.map(e => [e.offer.id, e]))
  const toastIds = new Set(toasts.value.map(t => t.offer.id))

  for (const e of newInbox) {
    if (toastIds.has(e.offer.id)) continue        // already toasted
    if (e.status !== 'pending') continue           // not actionable
    const prev = existingMap.get(e.offer.id)
    const isNew = !prev                            // brand-new offer
    const wasPromoted = prev?.queued && !e.queued  // was queued, now unqueued
    if (isNew || wasPromoted) {
      toasts.value.push(e)
    }
  }
  inbox.value = newInbox
}

async function loadIfaces() {
  ifaces.value = (await run(api.interfaces)) ?? []
  if (!ifaces.value.some(i => i.ip === selectedIp.value)) {
    selectedIp.value = ifaces.value[0]?.ip ?? ''
  }
}

// ─── Mode 1 actions ───────────────────────────────────────────────────────

async function pickFolder() {
  const dir = await open({ directory: true })
  if (typeof dir !== 'string') return
  await run(() => api.startShare(dir))
  await refresh()
}

async function stop() { await run(api.stopShare); await refresh() }
async function regenerate() { await run(api.regenerate); await refresh() }

async function toggleOpen(ev: Event) {
  const box = ev.target as HTMLInputElement
  if (box.checked) {
    const ok = await confirm(
      'Anyone on this network will be able to browse and download this folder without a link. ' +
      'Open mode switches off again when the app restarts.',
      { title: 'Enable open mode?', kind: 'warning' },
    )
    if (!ok) { box.checked = false; return }
  }
  await run(() => api.setOpenMode(box.checked))
  await refresh()
}

async function copy() {
  try {
    await navigator.clipboard.writeText(url.value)
    copied.value = true
    setTimeout(() => (copied.value = false), 1500)
  } catch { err.value = 'Could not copy — select the link and copy it manually.' }
}

watch(url, async u => {
  qrSvg.value = u ? ((await run(() => api.qr(u))) ?? '') : ''
}, { immediate: true })

// ─── Mode 2: send actions ─────────────────────────────────────────────────

function togglePeer(ip: string) {
  if (selectedPeers.value.has(ip)) selectedPeers.value.delete(ip)
  else selectedPeers.value.add(ip)
}

function selectAllPeers() {
  for (const p of peers.value) selectedPeers.value.add(p.ip)
}

async function pickAndSend() {
  const chosen = await open({ multiple: true, directory: false })
  if (!chosen) return
  const paths = Array.isArray(chosen) ? chosen : [chosen]
  if (!paths.length) return

  sending.value = true
  err.value = ''
  try {
    await api.sendFiles(paths, includeHashes.value)
    await refreshMode2()
  } catch (e) {
    err.value = String(e)
  } finally {
    sending.value = false
  }
}

async function cancelSend(offerId: string) {
  await run(() => api.cancelSend(offerId))
  await refreshMode2()
}

// ─── Mode 2: inbox actions ────────────────────────────────────────────────

async function accept(offerId: string) {
  toasts.value = toasts.value.filter(t => t.offer.id !== offerId)
  await run(() => api.acceptOffer(offerId))
  await refreshMode2()
}

async function ignore(offerId: string) {
  toasts.value = toasts.value.filter(t => t.offer.id !== offerId)
  await run(() => api.ignoreOffer(offerId))
  await refreshMode2()
}

async function cancelReceive(offerId: string) {
  await run(() => api.cancelReceive(offerId))
  await refreshMode2()
}

async function toggleMute() {
  await run(() => api.setMuted(!muted.value))
  await refresh()
  // Refresh inbox so offers promoted by unmute surface as toasts immediately.
  await refreshMode2()
}

// ─── Quit guard ───────────────────────────────────────────────────────────

let unlistenQuit: (() => void) | null = null
onMounted(async () => {
  const win = getCurrentWindow()
  unlistenQuit = await win.listen('quit-requested', async () => {
    const ok = await confirm(
      'You have active file transfers. Are you sure you want to quit? Transfers will be cancelled.',
      { title: 'Quit Subnet Share?', kind: 'warning' }
    )
    if (ok) {
      const { exit } = await import('@tauri-apps/plugin-process')
      exit(0)
    }
  })
})

onUnmounted(() => { unlistenQuit?.() })

// ─── Polling ─────────────────────────────────────────────────────────────

let pollTimer: ReturnType<typeof setInterval> | null = null

onMounted(async () => {
  await Promise.all([refresh(), loadIfaces()])
  await refreshMode2()
  pollTimer = setInterval(refreshMode2, 2000)
})

onUnmounted(() => { if (pollTimer) clearInterval(pollTimer) })
</script>

<template>
  <main>
    <!-- ── Header ─────────────────────────────────────────────────── -->
    <header>
      <h1>Subnet Share</h1>
      <span v-if="status" class="mut">{{ status.device_name }} · port {{ status.port }}</span>
      <span v-if="muted" class="badge mute-badge" title="Muted — incoming offers queued silently">🔇 Muted</span>
    </header>

    <!-- ── Tabs ───────────────────────────────────────────────────── -->
    <nav class="tabs">
      <button :class="{ active: tab === 'share' }" @click="tab = 'share'">Share</button>
      <button :class="{ active: tab === 'send' }"  @click="tab = 'send'">Send</button>
      <button :class="{ active: tab === 'inbox' }" @click="tab = 'inbox'">
        Inbox
        <span v-if="pendingInboxCount" class="badge">{{ pendingInboxCount }}</span>
      </button>
    </nav>

    <!-- ── Toast notifications (inbox) ──────────────────────────── -->
    <div v-if="toasts.length" class="toast-stack" aria-live="polite">
      <div v-for="t in toasts" :key="t.offer.id" class="toast" role="alert">
        <div class="toast-info">
          <strong>{{ t.offer.from_name }}</strong>
          <span class="mut"> ({{ t.offer.from_ip }})</span>
          wants to send
          <strong>{{ t.offer.files.length }} file{{ t.offer.files.length !== 1 ? 's' : '' }}</strong>
          ({{ fmtSize(t.offer.files.reduce((a, f) => a + f.size, 0)) }})
        </div>
        <div class="toast-files">
          <span v-for="f in t.offer.files" :key="f.name" class="file-chip">
            {{ f.name }}
            <span v-if="!f.hash" class="no-hash" title="File hash not present">⚠</span>
          </span>
        </div>
        <div class="toast-actions">
          <button class="primary" @click="accept(t.offer.id)">Accept</button>
          <button @click="ignore(t.offer.id)">Ignore</button>
        </div>
      </div>
    </div>

    <p v-if="err" class="err" role="alert">{{ err }}</p>

    <!-- ════════════════════════════════════════════════════════════ -->
    <!-- TAB: Share (Mode 1)                                         -->
    <!-- ════════════════════════════════════════════════════════════ -->
    <section v-if="tab === 'share'">
      <div v-if="!share" class="card center">
        <p>Share a folder with people on your network.</p>
        <button class="primary" @click="pickFolder">Choose folder…</button>
      </div>

      <div v-else class="card">
        <div class="row">
          <div class="path" :title="share.path">📁 {{ share.path }}</div>
          <button @click="pickFolder">Change</button>
        </div>

        <label class="field">
          <span>Network</span>
          <select v-model="selectedIp">
            <option v-for="i in ifaces" :key="i.ip" :value="i.ip">{{ ifaceLabel(i) }}</option>
          </select>
          <button title="Rescan adapters" @click="loadIfaces">↻</button>
        </label>
        <p v-if="ifaces.find(i => i.ip === selectedIp)?.virtual_hint" class="warn">
          This looks like a virtual adapter (Docker/WSL/VPN). Other devices probably can't reach it.
        </p>

        <div v-if="url" class="link">
          <input readonly :value="url" @focus="($event.target as HTMLInputElement).select()" />
          <button @click="copy">{{ copied ? 'Copied' : 'Copy' }}</button>
        </div>
        <p v-else class="warn">No usable network address found.</p>

        <div v-if="qrSvg" class="qr" v-html="qrSvg" />

        <p v-if="share.open_mode" class="warn strong">
          Open mode is ON — anyone on this network can access the folder without a link.
        </p>
        <label class="toggle">
          <input type="checkbox" :checked="share.open_mode" @change="toggleOpen" />
          Open mode (no link needed)
        </label>

        <div class="row actions">
          <button @click="regenerate" title="Old links and guest sessions stop working">New link</button>
          <button class="danger" @click="stop">Stop sharing</button>
        </div>
      </div>
    </section>

    <!-- ════════════════════════════════════════════════════════════ -->
    <!-- TAB: Send (Mode 2 sender)                                   -->
    <!-- ════════════════════════════════════════════════════════════ -->
    <section v-if="tab === 'send'">

      <!-- Peers list -->
      <div class="card">
        <div class="row">
          <h2>Peers on this network</h2>
          <button @click="refreshMode2" title="Rescan">↻</button>
        </div>
        <p v-if="!peers.length" class="mut small">No peers discovered yet. Wait a moment or check mDNS is working.</p>
        <ul v-else class="peer-list">
          <li v-for="p in peers" :key="p.ip" class="peer-row">
            <label>
              <input type="checkbox"
                :checked="selectedPeers.has(p.ip)"
                @change="togglePeer(p.ip)" />
              <span class="peer-name">{{ p.name }}</span>
              <span class="mut small">{{ p.ip }}:{{ p.port }}</span>
            </label>
          </li>
        </ul>
        <div v-if="peers.length" class="row">
          <button @click="selectAllPeers" class="small-btn">Select all</button>
        </div>
      </div>

      <!-- Send action -->
      <div class="card">
        <label class="toggle">
          <input type="checkbox" v-model="includeHashes" />
          Include BLAKE3 file hashes (slower for large files)
        </label>
        <button class="primary" :disabled="sending || !peers.length" @click="pickAndSend">
          {{ sending ? 'Sending…' : 'Pick files & send to all peers…' }}
        </button>
        <p class="mut small">The offer will be broadcast to all discovered peers.</p>
      </div>

      <!-- Outbox -->
      <div v-if="outbox.length" class="card">
        <h2>Sent offers</h2>
        <div v-for="entry in outbox" :key="entry.offer.id" class="outbox-entry">
          <div class="row">
            <span class="offer-files">
              {{ entry.offer.files.map(f => f.name).join(', ') }}
            </span>
            <button class="small-btn danger"
              @click="cancelSend(entry.offer.id)"
              title="Cancel this offer for all receivers">
              Cancel
            </button>
          </div>
          <div class="receiver-list">
            <div v-for="r in entry.receivers" :key="r.peer_ip" class="receiver-row">
              <span class="peer-name">{{ r.peer_name }}</span>
              <span class="mut small">{{ r.peer_ip }}</span>
              <span :class="['recv-status', r.status]">{{ statusLabel(r as any) }}</span>
            </div>
          </div>
        </div>
      </div>
    </section>

    <!-- ════════════════════════════════════════════════════════════ -->
    <!-- TAB: Inbox (Mode 2 receiver)                                -->
    <!-- ════════════════════════════════════════════════════════════ -->
    <section v-if="tab === 'inbox'">

      <!-- Mute toggle -->
      <div class="card row">
        <label class="toggle" style="flex:1">
          <input type="checkbox" :checked="muted" @change="toggleMute" />
          <span>Mute all — queue offers silently</span>
        </label>
        <span v-if="muted" class="mut small">Offers are queuing. Senders see "muted".</span>
      </div>

      <!-- Empty state -->
      <div v-if="!inbox.length" class="card center">
        <p class="mut">No offers received yet.</p>
      </div>

      <!-- Inbox list -->
      <div v-else class="card">
        <div v-for="entry in inbox" :key="entry.offer.id" class="inbox-entry"
             :class="{ 'queued': entry.queued }">
          <div class="row">
            <div class="inbox-from">
              <strong>{{ entry.offer.from_name }}</strong>
              <span class="mut small"> {{ entry.offer.from_ip }}</span>
              <span v-if="entry.queued" class="badge small" title="Arrived while muted">queued</span>
            </div>
            <span :class="['inbox-status', typeof entry.status === 'string' ? entry.status : 'failed']">
              {{ inboxStatusLabel(entry.status) }}
            </span>
          </div>

          <div class="file-list">
            <div v-for="f in entry.offer.files" :key="f.name" class="file-row">
              <span class="nm">{{ f.name }}</span>
              <span class="mut small">{{ fmtSize(f.size) }}</span>
              <span v-if="!f.hash" class="no-hash" title="File hash not present — integrity cannot be verified">⚠ no hash</span>
            </div>
          </div>

          <div class="row" v-if="entry.status === 'pending' || entry.queued">
            <button class="primary" @click="accept(entry.offer.id)">Accept</button>
            <button @click="ignore(entry.offer.id)">Ignore</button>
          </div>
          <div class="row" v-if="entry.status === 'downloading'">
            <button class="danger" @click="cancelReceive(entry.offer.id)">Cancel download</button>
          </div>
        </div>
      </div>
    </section>
  </main>
</template>

<style>
:root {
  --bg:#fff; --fg:#1c1f24; --mut:#6b7280; --card:#f3f4f6; --bd:#e5e7eb;
  --ac:#2563eb; --bad:#dc2626; --warn:#b45309; --ok:#16a34a
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg:#0f1115; --fg:#e6e8eb; --mut:#9aa0a6; --card:#181b21; --bd:#2a2e36;
    --ac:#6ea0ff; --bad:#f87171; --warn:#fbbf24; --ok:#4ade80
  }
}
* { box-sizing: border-box }
body { margin:0; font:14px/1.45 system-ui, sans-serif; background:var(--bg); color:var(--fg) }
main { padding:1rem; display:flex; flex-direction:column; gap:.8rem; max-width:600px; margin:0 auto }
header { display:flex; align-items:baseline; gap:.7rem; flex-wrap:wrap }
h1 { margin:0; font-size:1.2rem }
h2 { margin:0; font-size:1rem }
.mut { color:var(--mut) }
.small { font-size:.82em }

/* ── Card ─────────────────────────────────────────────────────────── */
.card {
  background:var(--card); border:1px solid var(--bd); border-radius:10px;
  padding:1rem; display:flex; flex-direction:column; gap:.8rem
}
.center { align-items:center; text-align:center; padding:2rem 1rem }

/* ── Tabs ─────────────────────────────────────────────────────────── */
.tabs { display:flex; gap:.3rem }
.tabs button {
  flex:1; padding:.4rem .6rem; border:1px solid var(--bd); border-radius:6px;
  background:var(--bg); cursor:pointer; position:relative
}
.tabs button.active { background:var(--ac); color:#fff; border-color:var(--ac) }
.tabs button:hover:not(.active) { border-color:var(--ac) }

/* ── Badge ────────────────────────────────────────────────────────── */
.badge {
  display:inline-block; min-width:18px; padding:1px 5px; border-radius:10px;
  background:var(--bad); color:#fff; font-size:.75em; text-align:center; margin-left:.3rem
}
.mute-badge { background:var(--warn); font-size:.85em; padding:2px 7px; border-radius:8px }

/* ── Toasts ───────────────────────────────────────────────────────── */
.toast-stack { display:flex; flex-direction:column; gap:.5rem }
.toast {
  background:var(--card); border:1px solid var(--bd); border-radius:10px;
  padding:.8rem; display:flex; flex-direction:column; gap:.5rem;
  border-left:4px solid var(--ac);
}
.toast-info { font-size:.9em }
.toast-files { display:flex; flex-wrap:wrap; gap:.3rem }
.toast-actions { display:flex; gap:.5rem }
.file-chip {
  background:var(--bg); border:1px solid var(--bd); border-radius:5px;
  padding:2px 6px; font-size:.82em; display:flex; align-items:center; gap:.3rem
}

/* ── Shared row/field helpers ─────────────────────────────────────── */
.row { display:flex; gap:.5rem; align-items:center }
.path { flex:1; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; direction:rtl; text-align:left }
.field { display:flex; gap:.5rem; align-items:center }
.field span { color:var(--mut) }
.field select { flex:1; min-width:0 }
.link { display:flex; gap:.5rem }
.link input { flex:1; min-width:0 }
.qr { align-self:center; width:200px; height:200px; border-radius:8px; overflow:hidden; background:#fff }
.qr svg { width:100%; height:100%; display:block }
.toggle { display:flex; gap:.5rem; align-items:center; cursor:pointer }
.actions { justify-content:space-between }
.warn { margin:0; color:var(--warn); font-size:.9em }
.strong { font-weight:600 }
.err { margin:0; color:var(--bad) }

/* ── Peer list ────────────────────────────────────────────────────── */
.peer-list { margin:0; padding:0; list-style:none; display:flex; flex-direction:column; gap:.3rem }
.peer-row label { display:flex; align-items:center; gap:.5rem; cursor:pointer }
.peer-name { font-weight:500 }
.small-btn { font-size:.82em; padding:.2rem .5rem }

/* ── Outbox ──────────────────────────────────────────────────────── */
.outbox-entry { border-top:1px solid var(--bd); padding-top:.6rem }
.outbox-entry:first-child { border-top:none; padding-top:0 }
.offer-files { flex:1; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; font-weight:500 }
.receiver-list { display:flex; flex-direction:column; gap:.2rem; margin-left:.5rem }
.receiver-row { display:flex; gap:.5rem; align-items:center; font-size:.9em }
.recv-status { margin-left:auto; font-size:.82em; color:var(--mut) }
.recv-status.done { color:var(--ok) }
.recv-status.unreachable, .recv-status.cancelled { color:var(--bad) }
.recv-status.muted { color:var(--warn) }

/* ── Inbox ──────────────────────────────────────────────────────── */
.inbox-entry { border-top:1px solid var(--bd); padding-top:.7rem }
.inbox-entry:first-child { border-top:none; padding-top:0 }
.inbox-entry.queued { opacity:.75 }
.inbox-from { display:flex; align-items:baseline; gap:.3rem; flex:1 }
.inbox-status { font-size:.82em; color:var(--mut); margin-left:auto }
.inbox-status.done { color:var(--ok) }
.inbox-status.failed { color:var(--bad) }
.inbox-status.cancelled { color:var(--bad) }
.inbox-status.downloading { color:var(--ac) }
.file-list { display:flex; flex-direction:column; gap:.2rem }
.file-row { display:flex; align-items:center; gap:.5rem; font-size:.9em }
.nm { overflow:hidden; text-overflow:ellipsis; white-space:nowrap; flex:1 }
.no-hash { color:var(--warn); font-size:.8em; white-space:nowrap }

/* ── Form controls ───────────────────────────────────────────────── */
input, select, button { font:inherit; color:inherit; background:var(--bg); border:1px solid var(--bd); border-radius:6px; padding:.4rem .65rem }
button { cursor:pointer }
button:hover:not(:disabled) { border-color:var(--ac) }
button:disabled { opacity:.5; cursor:default }
button.primary { background:var(--ac); border-color:var(--ac); color:#fff }
button.danger { color:var(--bad) }
</style>

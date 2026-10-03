#!/usr/bin/env node
// dev.mjs — starts Vite (which writes its real port to .vite-port via a
// plugin), then starts `tauri dev` with that port passed via --config so
// tauri.conf.json is never modified on disk.
//
// Usage: node dev.mjs   (invoked by `npm run dev` in root package.json)

import { spawn } from 'node:child_process'
import { existsSync, readFileSync, rmSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dir = dirname(fileURLToPath(import.meta.url))
const portFile = resolve(__dir, '.vite-port')

// Remove stale port file from a previous run
if (existsSync(portFile)) rmSync(portFile)

// ── 1. Start Vite ─────────────────────────────────────────────────────────

const vite = spawn('npm', ['run', 'dev', '--workspace', 'ui/owner'], {
  stdio: 'inherit',
  shell: true,
})

vite.on('error', err => { console.error('[dev] Vite failed to start:', err); process.exit(1) })

// ── 2. Wait for Vite to write .vite-port (polled every 200 ms, 30 s timeout)

const port = await new Promise((resolve, reject) => {
  const deadline = Date.now() + 30_000

  const interval = setInterval(() => {
    if (existsSync(portFile)) {
      const p = parseInt(readFileSync(portFile, 'utf8').trim(), 10)
      if (!isNaN(p)) {
        clearInterval(interval)
        return resolve(p)
      }
    }
    if (Date.now() > deadline) {
      clearInterval(interval)
      reject(new Error('Timed out waiting for Vite to start (30 s)'))
    }
  }, 200)

  vite.on('exit', () => {
    clearInterval(interval)
    reject(new Error('Vite exited before writing .vite-port'))
  })
})

console.log(`[dev] Vite ready on :${port} — starting Tauri`)

// ── 3. Start tauri dev with the real devUrl injected via --config ─────────
// beforeDevCommand is cleared so Tauri doesn't try to start Vite a second time.

const tauri = spawn(
  'npx',
  [
    'tauri', 'dev',
    '--config',
    JSON.stringify({
      build: {
        devUrl: `http://localhost:${port}`,
        beforeDevCommand: '',
      },
    }),
  ],
  { stdio: 'inherit', shell: true }
)

tauri.on('error', err => { console.error('[dev] Tauri failed to start:', err); process.exit(1) })

// ── 4. Lifecycle: if either process dies, kill the other ──────────────────

const cleanup = () => {
  try { vite.kill('SIGTERM') } catch {}
  try { tauri.kill('SIGTERM') } catch {}
}

process.on('SIGINT', cleanup)
process.on('SIGTERM', cleanup)

tauri.on('exit', () => { vite.kill('SIGTERM'); process.exit(0) })
vite.on('exit',  () => { tauri.kill('SIGTERM'); process.exit(0) })

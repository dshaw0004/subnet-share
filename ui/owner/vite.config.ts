import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { writeFileSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))

// Writes the actual bound port to <repo-root>/.vite-port so dev.mjs can pick
// it up without parsing console output.
function writePortPlugin(): import('vite').Plugin {
  return {
    name: 'write-port',
    configureServer(server) {
      server.httpServer?.once('listening', () => {
        const addr = server.httpServer!.address()
        const port = typeof addr === 'object' && addr !== null ? addr.port : 5173
        const out = resolve(__dirname, '../../.vite-port')
        writeFileSync(out, String(port))
      })
    },
  }
}

export default defineConfig({
  plugins: [vue(), writePortPlugin()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: false, // fall back to next free port instead of crashing
  },
})

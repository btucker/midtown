import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import tailwindcss from '@tailwindcss/vite'
import { fileURLToPath, URL } from 'node:url'
import { readFileSync, existsSync } from 'node:fs'
import { join } from 'node:path'
import { homedir } from 'node:os'
import { VitePWA } from 'vite-plugin-pwa'
import istanbul from 'vite-plugin-istanbul'

// Auto-detect webserver URL from ~/.midtown/config.toml (TLS, port).
// Override with MIDTOWN_WEBSERVER_URL env var if needed.
function detectWebserverTarget() {
  if (process.env.MIDTOWN_WEBSERVER_URL) {
    return process.env.MIDTOWN_WEBSERVER_URL
  }
  const port = process.env.MIDTOWN_WEBSERVER_PORT || 47022
  const configPath = join(homedir(), '.midtown', 'config.toml')
  let useTls = false
  if (existsSync(configPath)) {
    try {
      const config = readFileSync(configPath, 'utf-8')
      // Uncommented tls_cert line means TLS is active
      useTls = /^\s*tls_cert\s*=/m.test(config)
    } catch { /* fall back to http */ }
  }
  return `${useTls ? 'https' : 'http'}://localhost:${port}`
}

const webserverTarget = detectWebserverTarget()

export default defineConfig({
  resolve: {
    alias: {
      $lib: fileURLToPath(new URL('./src/lib', import.meta.url)),
    },
  },
  plugins: [
    svelte(),
    tailwindcss(),
    // Istanbul code coverage instrumentation — only active when COVERAGE=true
    ...(process.env.COVERAGE === 'true' ? [istanbul({
      include: 'src/**/*',
      exclude: ['node_modules', 'e2e', 'src/sw.ts'],
      extension: ['.js', '.ts', '.svelte'],
      requireEnv: true,
    })] : []),
    VitePWA({
      strategies: 'injectManifest',
      srcDir: 'src',
      filename: 'sw.ts',
      registerType: 'autoUpdate',
      injectManifest: {
        globPatterns: ['**/*.{js,css,html,svg,png,ico}'],
      },
      manifest: {
        name: 'Midtown Mobile',
        short_name: 'Midtown',
        description: 'Midtown team coordination app',
        theme_color: '#1A232D',
        background_color: '#1A232D',
        display: 'standalone',
        icons: [
          {
            src: 'pwa-64x64.png',
            sizes: '64x64',
            type: 'image/png',
          },
          {
            src: 'pwa-192x192.png',
            sizes: '192x192',
            type: 'image/png',
          },
          {
            src: 'pwa-512x512.png',
            sizes: '512x512',
            type: 'image/png',
          },
          {
            src: 'maskable-icon-512x512.png',
            sizes: '512x512',
            type: 'image/png',
            purpose: 'maskable',
          },
        ],
      },
      devOptions: {
        enabled: false,
      },
    }),
  ],
  server: {
    proxy: {
      '/api': {
        target: webserverTarget,
        ws: true,
        secure: false, // accept self-signed / local TLS certs
      },
    },
  },
  build: {
    emptyOutDir: true,
    sourcemap: true,
  },
  test: {
    exclude: ['e2e/**', 'node_modules/**'],
  },
})

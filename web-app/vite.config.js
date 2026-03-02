import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import tailwindcss from '@tailwindcss/vite'
import { fileURLToPath, URL } from 'node:url'

const nodeMajor = Number(process.versions.node.split('.')[0])
const shouldEnablePwa = nodeMajor >= 1 && nodeMajor < 24

const vitePwaPlugin = async () => {
  if (!shouldEnablePwa) {
    console.log(
      'Node.js >=24 is not supported by the current vite-plugin-pwa toolchain in this repo,',
      'so source builds skip service worker generation.',
    )
    return []
  }

  const { VitePWA } = await import('vite-plugin-pwa')
  return [
    VitePWA({
      strategies: 'injectManifest',
      srcDir: 'src',
      filename: 'sw.js',
      registerType: 'autoUpdate',
      injectManifest: {
        globPatterns: ['**/*.{js,css,html,svg,png,ico}'],
      },
      manifest: {
        name: 'Midtown Mobile',
        short_name: 'Midtown',
        description: 'Midtown team coordination app',
        theme_color: '#0a0a0a',
        background_color: '#0a0a0a',
        display: 'standalone',
        icons: [
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
            src: 'pwa-512x512.png',
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
  ]
}

export default defineConfig(async () => ({
  resolve: {
    alias: {
      $lib: fileURLToPath(new URL('./src/lib', import.meta.url)),
    },
  },
  plugins: [svelte(), tailwindcss(), ...(await vitePwaPlugin())],
  server: {
    proxy: {
      '/api': {
        target: `http://localhost:${process.env.MIDTOWN_WEBSERVER_PORT || 47022}`,
        ws: true,
      },
    },
  },
  build: {
    emptyOutDir: true,
  },
  test: {
    exclude: ['e2e/**', 'node_modules/**'],
  },
}))

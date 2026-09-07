import react from '@vitejs/plugin-react'
import { createRequire } from 'node:module'
import { VitePWA } from 'vite-plugin-pwa'
import { defineConfig } from 'vitest/config'

const require = createRequire(import.meta.url)

export default defineConfig({
  worker: {
    plugins: () => [{
      name: 'farhelm-worker-parsers',
      enforce: 'pre',
      // Browser exports use document/DOMParser. Default exports provide a static
      // entity table and parse5, so Markdown and KaTeX can run without a DOM.
      resolveId(source) { if (['decode-named-character-reference', 'hast-util-from-html-isomorphic'].includes(source)) return require.resolve(source) },
    }],
  },
  plugins: [
    react(),
    VitePWA({
      strategies: 'injectManifest',
      srcDir: 'src',
      filename: 'sw.ts',
      registerType: 'autoUpdate',
      includeAssets: ['farhelm-mark.svg'],
      manifest: {
        name: 'FarHelm Console',
        short_name: 'FarHelm',
        description: 'Mobile-first control surface for FarHelm',
        theme_color: '#0B0F14',
        background_color: '#0B0F14',
        display: 'standalone',
        start_url: '/',
        icons: [
          {
            src: '/farhelm-mark.svg',
            sizes: 'any',
            type: 'image/svg+xml',
            purpose: 'any maskable',
          },
        ],
      },
    }),
  ],
  server: {
    host: '127.0.0.1',
    port: 5173,
    proxy: {
      '/api': 'http://127.0.0.1:8787',
    },
  },
  preview: {
    host: '127.0.0.1',
    port: 4173,
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    css: true,
    include: ['src/**/*.test.{ts,tsx}'],
  },
})

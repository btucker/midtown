import { defineConfig } from '@vite-pwa/assets-generator/config'

// Source SVG already includes its own background and padding,
// so set padding to 0 for all icon types.
export default defineConfig({
  preset: {
    transparent: {
      sizes: [64, 192, 512],
      favicons: [[48, 'favicon.ico']],
      padding: 0,
    },
    maskable: {
      sizes: [512],
      padding: 0,
    },
    apple: {
      sizes: [180],
      padding: 0,
    },
  },
  images: ['public/pwa-source.svg'],
})

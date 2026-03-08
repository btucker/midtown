import {
  createAppleSplashScreens,
  defineConfig,
} from '@vite-pwa/assets-generator/config'

// Source SVG already includes its own background and padding,
// so set padding to 0 for all icon types.
//
// Apple splash screens use padding 0.3 to center the logo within
// the splash area, with the dark theme background color.
const appleSplashScreens = createAppleSplashScreens({
  padding: 0.3,
  resizeOptions: { background: '#1A232D', fit: 'contain' },
  linkMediaOptions: { addMediaScreen: true, xhtml: false },
}, [
  'iPhone 16 Pro Max',
  'iPhone 16 Pro',
  'iPhone 16 Plus',
  'iPhone 16',
  'iPhone 15 Pro Max',
  'iPhone 15 Pro',
  'iPhone 15 Plus',
  'iPhone 15',
  'iPhone 14 Pro Max',
  'iPhone 14 Pro',
  'iPhone 14',
  'iPhone 13 Pro Max',
  'iPhone 13 Pro',
  'iPhone 13',
  'iPhone SE 4.7"',
  'iPad Pro 12.9"',
  'iPad Pro 11"',
  'iPad Air 13"',
  'iPad Air 11"',
  'iPad mini 8.3"',
])

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
    appleSplashScreens,
  },
  images: ['public/pwa-source.svg'],
})

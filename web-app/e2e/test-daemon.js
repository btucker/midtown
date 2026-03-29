// @ts-check
/**
 * Shared test daemon harness for E2E tests.
 * Starts a v2 daemon on the midtown-e2e-test repo with a dedicated port.
 * Tests hit this daemon directly (no shared webserver proxy needed).
 */

import { execSync, spawn } from 'child_process'
import { unlinkSync, rmSync } from 'fs'
import { join } from 'path'

const TEST_REPO = '/Users/btucker/projects/midtown-e2e-test'
const MIDTOWN_BIN = execSync('which midtown').toString().trim()
const HOME = process.env.HOME || '/tmp'
const STATE_DIR = join(HOME, '.local/state/midtown/midtown-e2e-test')
const PROJECT_DIR = join(HOME, '.midtown/projects/midtown-e2e-test')

export const TEST_PORT = 47098
export const API = `http://localhost:${TEST_PORT}`

/** @type {import('child_process').ChildProcess | null} */
let daemonProcess = null

export function cleanupDaemon() {
  if (daemonProcess) {
    daemonProcess.kill('SIGKILL')
    daemonProcess = null
  }
  try { unlinkSync(join(STATE_DIR, 'daemon.sock')) } catch {}
  try { unlinkSync(join(PROJECT_DIR, 'daemon.pid')) } catch {}
  try { rmSync(join(PROJECT_DIR, 'events'), { recursive: true, force: true }) } catch {}
}

export async function startDaemon() {
  cleanupDaemon()

  daemonProcess = spawn(MIDTOWN_BIN, [
    'daemon-v2',
    '--workdir', 'midtown-e2e-test',
    '--channel', 'main',
    '--web-port', TEST_PORT.toString(),
  ], {
    cwd: TEST_REPO,
    env: {
      ...process.env,
      MIDTOWN_CHAT_MONITOR: '0',
      MIDTOWN_WEBHOOK_PORT: '0',
    },
    stdio: 'ignore',
    detached: true,
  })

  // Wait for the web server to be ready
  for (let i = 0; i < 30; i++) {
    try {
      const res = await fetch(`${API}/api/health`)
      if (res.ok) return
    } catch {}
    await new Promise(r => setTimeout(r, 500))
  }
  throw new Error(`Test daemon did not start within 15s on port ${TEST_PORT}`)
}

#!/usr/bin/env node
import { chromium } from '@playwright/test';

async function takeScreenshot() {
  const browser = await chromium.launch();
  const context = await browser.newContext({
    viewport: { width: 390, height: 844 }, // iPhone 14 Pro size
    deviceScaleFactor: 3,
    hasTouch: true,
    isMobile: true,
  });

  const page = await context.newPage();

  // Mock the API responses
  await page.route('**/api/projects', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify([
        { name: 'test-project', status: 'running', webhook_port: 3001 }
      ])
    })
  );

  await page.route('**/api/channel', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify([
        { from: 'lead', content: 'Test message', timestamp: new Date().toISOString(), channel: 'lead' }
      ])
    })
  );

  await page.route('**/api/status', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        coworkers: [],
        tasks: [],
        kanban: { open: [], review: [], done: [] },
        repo: { fullName: 'test/repo', branch: 'main' }
      })
    })
  );

  await page.route('**/ws', (route) => route.abort());

  // Navigate and wait for content
  await page.goto('http://localhost:5173/');
  await page.waitForLoadState('networkidle');

  // Click Board tab
  await page.click('nav button:has-text("Board")');
  await page.waitForTimeout(500);

  // Take screenshot
  await page.screenshot({
    path: 'screenshots/mobile-after-fix.png',
    fullPage: false
  });

  console.log('Screenshot saved to screenshots/mobile-after-fix.png');

  await browser.close();
}

takeScreenshot().catch(console.error);

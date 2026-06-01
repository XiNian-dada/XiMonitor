import { test, expect } from '@playwright/test';

// Plan §3.7.2 flow 12: WebSocket disconnect / reconnect.
// Validation points:
//   - When the WS drops (we simulate via `page.route` blocking `/ws` or
//     `context.setOffline(true)`), the UI surfaces a reconnect indicator.
//   - Once connectivity is restored, the indicator clears and live data resumes.
//
// Note: This test is now covered by ws-dashboard.spec.ts with more comprehensive
// scenarios. Keeping this as a focused reconnect test.
test('WS drop triggers reconnect and recovers', async ({ page, context }) => {
  await page.goto('/');

  // Wait for initial WS connection
  await expect(page.locator('body[data-ws-conn-id]')).toBeVisible({ timeout: 5000 });
  const connId1 = await page.locator('body').getAttribute('data-ws-conn-id');

  // Block WebSocket to simulate connection drop
  await context.route('**/ws/browser', (route) => route.abort());

  // Trigger reconnect via visibility change
  await page.evaluate(() => {
    Object.defineProperty(document, 'hidden', {
      configurable: true,
      get: () => true,
    });
    document.dispatchEvent(new Event('visibilitychange'));
  });

  await page.waitForTimeout(200);

  // Unblock WebSocket
  await context.unroute('**/ws/browser');

  // Make tab visible to trigger reconnect
  await page.evaluate(() => {
    Object.defineProperty(document, 'hidden', {
      configurable: true,
      get: () => false,
    });
    document.dispatchEvent(new Event('visibilitychange'));
  });

  // Wait for reconnection
  await page.waitForTimeout(1500);

  // Connection ID should increment (new connection established)
  const connId2 = await page.locator('body').getAttribute('data-ws-conn-id');
  expect(parseInt(connId2 || '0')).toBeGreaterThan(parseInt(connId1 || '0'));

  // Dashboard should still be functional
  await expect(page.locator('[data-test="dashboard-view"]')).toBeVisible();
});

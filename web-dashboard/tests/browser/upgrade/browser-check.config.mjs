// Upgrading an installed app (review M1, Astra and Sol, 2026-11-14): the previous release's service worker and cached
// classic page against this checkout's packaged UI (dist/ui and dist/web-server, so run `bun run build` first; the
// UI comes from a build host), on a fixture server with synthetic APIs. Service workers are allowed here, unlike the
// other kits.
//   browser-check run web-dashboard/tests/browser/upgrade
import path from 'node:path';

export default async (ctx) => {
  const repo = path.resolve(ctx.configDir, '..', '..', '..', '..');
  const port = await ctx.freePort();
  return {
    name: 'aac-upgrade',
    server: {
      command: [process.execPath, path.join(ctx.configDir, 'fixture-server.cjs'), String(port), repo],
      url: `http://${ctx.reachHost}:${port}/__upgrade/ready`,
      timeoutMs: 30000,
      env: {
        AAC_CURSOR_BILLING_ENABLED: '0',
        AAC_CURSOR_API_BASE: 'http://127.0.0.1:1',
        AAC_UPGRADE_PREVIOUS_UI: String(ctx.args.previousUi || ''),
        AAC_UPGRADE_FIX1_UI: String(ctx.args.fix1Ui || ''),
      },
    },
    viewport: { width: 1280, height: 900 },
    isolate: true,
    testTimeoutMs: 90000,
    // No hint socket on the fixture server; an offline launch's own reads fail by design (the frame says so).
    consoleErrors: { fail: true, allow: [/WebSocket.*(?:failed|handshake)/i, /ERR_INTERNET_DISCONNECTED/] },
    failedRequests: { fail: false },
  };
};

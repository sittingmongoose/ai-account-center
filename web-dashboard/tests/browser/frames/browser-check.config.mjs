// Whole-page images of the Leptos Status and Scoreboard pages in the five approved formats, from the design fixtures
// (web-dashboard/leptos/fixtures, made-up data) on the fixture server, for layout review against the mockups.
//   browser-check run web-dashboard/tests/browser/frames --set out=<folder>
// Needs web-dashboard/leptos/pkg built (see web-dashboard/leptos/NOTES.md). Output: <page>-<format>-<theme>.png.
import path from 'node:path';
import fs from 'node:fs';
import os from 'node:os';

export default async (ctx) => {
  const leptos = path.resolve(ctx.configDir, '..', '..', '..', 'leptos');
  const port = await ctx.freePort();
  return {
    name: 'aac-leptos-frames',
    setup: () => ({ home: fs.mkdtempSync(path.join(os.tmpdir(), 'aac-frames-')) }),
    teardown: (_, s) => {
      if (s?.home) fs.rmSync(s.home, { recursive: true, force: true });
    },
    server: (_, s) => ({
      command: [process.execPath, path.join(leptos, 'dev', 'serve.mjs'), String(port)],
      url: `http://${ctx.reachHost}:${port}/app/status`,
      timeoutMs: 30000,
      env: {
        CCS_HOME: s.home,
        HOME: s.home,
        CLAUDE_CONFIG_DIR: path.join(s.home, '.claude'),
        CODEX_HOME: path.join(s.home, '.codex'),
      },
    }),
    viewport: { width: 1440, height: 900 },
    isolate: true,
    // The fixture server has no /ws: the page's hint socket retrying is expected.
    consoleErrors: { fail: true, allow: [/WebSocket.*(?:failed|handshake)/i] },
    failedRequests: { fail: false },
  };
};

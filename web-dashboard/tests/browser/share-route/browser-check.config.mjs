import fs from 'node:fs';
import path from 'node:path';
export default (ctx) => ({
  name: 'aac-share-sandbox',
  async setup() {
    const home = path.join(ctx.dir, 'fixture');
    fs.mkdirSync(home, { recursive: true });
    return { home, port: await ctx.freePort() };
  },
  server: (_, s) => ({
    command: ['bun', path.join(ctx.configDir, 'server.ts'), String(s.port), s.home],
    url: `http://127.0.0.1:${s.port}/`,
    env: { CCS_HOME: path.join(s.home, 'ccs'), CCS_DASHBOARD_AUTH_ENABLED: 'true' },
  }),
  teardown(_, s) { if (s) fs.rmSync(s.home, { recursive: true, force: true }); },
  consoleErrors: { fail: false }, // hostile content is expected to generate CSP refusals
  failedRequests: { fail: false },
});

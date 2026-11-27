// Upgrading an installed app against the native Rust dashboard server (dist/native/ai-account-center and
// dist/native/aac-http, serving the built Leptos UI from dist/ui via Rust StaticUi). Uses a temporary CCS_HOME and
// real loopback test login only, never real account data. Service workers are allowed here, unlike the other kits.
//   browser-check run web-dashboard/tests/browser/upgrade
import fs from 'node:fs';
import path from 'node:path';

export default (ctx) => {
  const repo = ctx.args.repo ? path.resolve(ctx.args.repo) : path.resolve(ctx.configDir, '..', '..', '..', '..');
  const ext = process.platform === 'win32' ? '.exe' : '';
  const cli = path.join(repo, 'dist', 'native', `ai-account-center${ext}`);
  const httpBin = path.join(repo, 'dist', 'native', `aac-http${ext}`);
  const uiDir = ctx.args.uiDir ? path.resolve(String(ctx.args.uiDir)) : path.join(repo, 'dist', 'ui');
  if (!fs.existsSync(cli) || !fs.existsSync(httpBin) || !fs.existsSync(path.join(uiDir, 'app', 'index.html'))) {
    throw new Error(
      `no built native dashboard in ${repo}/dist: run bun run build on a Rust build host (Nas1 or the 1070ti; never the VM), or copy a built dist/ (see web-dashboard/tests/browser/README.md)`
    );
  }
  if (ctx.fleet) {
    throw new Error(
      "--host: the native upgrade server listens on 127.0.0.1 and loopback setup is local-only, so a browser on another machine can't use it. Run from a checkout on that machine."
    );
  }
  return {
    name: 'aac-upgrade',
    async setup() {
      const tempDir = path.join(ctx.dir, 'upgrade-runtime');
      fs.mkdirSync(tempDir, { recursive: true });
      const port = +ctx.args.port || (await ctx.freePort());
      return { tempDir, port };
    },
    server: (_, s) => ({
      command: [process.execPath, path.join(ctx.configDir, 'native-server.mjs'), String(s.port), repo, s.tempDir],
      cwd: repo,
      url: `http://127.0.0.1:${s.port}/__upgrade/ready`,
      timeoutMs: +ctx.args.serverTimeout || 60000,
      env: {
        AAC_UPGRADE_UI_DIR: uiDir,
        AAC_UPGRADE_OLDER_UI: ctx.args.olderUi ? path.resolve(String(ctx.args.olderUi)) : '',
        AAC_UPGRADE_PREVIOUS_UI: ctx.args.previousUi ? path.resolve(String(ctx.args.previousUi)) : '',
        AAC_UPGRADE_FIX1_UI: ctx.args.fix1Ui ? path.resolve(String(ctx.args.fix1Ui)) : '',
      },
    }),
    async teardown(_, s) {
      if (s?.tempDir && !ctx.args.keep) fs.rmSync(s.tempDir, { recursive: true, force: true });
    },
    viewport: { width: 1280, height: 900 },
    isolate: true,
    testTimeoutMs: 120000,
    // Offline launches intentionally disconnect the network and /ws; tests assert zero online errors explicitly.
    consoleErrors: { fail: true, allow: [/WebSocket.*(?:failed|handshake)/i, /ERR_INTERNET_DISCONNECTED/] },
    failedRequests: { fail: false },
    helpers: {
      // Real loopback setup/login against the temporary CCS_HOME on native ai-account-center (test login only).
      async signIn(t, { username = 'labadmin', password = 'Lab-Correct-Horse-Battery-9' } = {}) {
        const origin = new URL(t.run.baseURL).origin;
        if (!t.page.url().startsWith(origin)) await t.goto('/__upgrade/ready', { settleMs: 0 });
        const r = await t.page.evaluate(async (cred) => {
          const check = await (await fetch('/api/auth/check', { credentials: 'same-origin' })).json();
          if (check.authenticated) return { ok: true, status: 200, body: 'already signed in' };
          const setup = await (await fetch('/api/auth/setup', { credentials: 'same-origin' })).json();
          const res = await fetch(setup.configured ? '/api/auth/login' : '/api/auth/setup', {
            method: 'POST',
            credentials: 'same-origin',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify(cred),
          });
          return { ok: res.ok, status: res.status, body: (await res.text()).slice(0, 200) };
        }, { username, password });
        return t.check('signed in', r.ok, `setup/login answered ${r.status}: ${r.body}`, { hard: true });
      },
      // Control the native harness from Node so switching builds never navigates or disturbs the page or its worker.
      async control(t, values) {
        const res = await fetch(new URL('/__upgrade/control', t.run.baseURL), {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(values),
        });
        if (!res.ok) throw new Error(`control answered ${res.status}`);
        return res.json();
      },
      async builds(t) {
        const res = await fetch(new URL('/__upgrade/control', t.run.baseURL), { cache: 'no-store' });
        if (!res.ok) throw new Error(`control read answered ${res.status}`);
        const data = await res.json();
        return data.builds;
      },
    },
  };
};

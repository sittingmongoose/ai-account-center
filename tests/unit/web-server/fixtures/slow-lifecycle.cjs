'use strict';
// Runs in a separate Node process with a syscall shim and one owned fixture root.
const fs = require('node:fs');
const path = require('node:path');
const root = process.argv[2];
const kind = process.argv[3];
const mode = process.argv[4];
const repo = path.resolve(__dirname, '../../../..');
const ccs = path.join(root, '.ccs');
fs.mkdirSync(path.join(ccs, 'run'), { recursive: true, mode: 0o700 });
fs.writeFileSync(path.join(ccs, 'run/aac-http.json'), '{}', { mode: 0o600 });
Object.assign(process.env, {
  CCS_HOME: root,
  CCS_DIR: ccs,
  CLAUDE_CONFIG_DIR: path.join(root, 'claude'),
  CODEX_HOME: path.join(root, 'codex'),
  AAC_AGENT_REPORTS_DIR: path.join(root, 'absent-reports'),
  AAC_PM_MAIL_REGISTRY_DIR: path.join(root, 'absent-registry'),
  AAC_STATUS_DOORBELL: 'off',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_RUST_STORE: 'off',
  AAC_STORE: 'off',
});
require('node:os').homedir = () => root;
require(
  path.join(repo, 'dist/web-server/services/codex-profile-renewal-service')
).CodexProfileRenewalService.prototype.start = () => {};
// Public startup stays real. Unrelated background readers/producers have no work here.
require(
  path.join(repo, 'dist/web-server/services/account-lifecycle-runtime')
).startAccountLifecycleMaintenance = () => {};
global.fetch = () => Promise.reject(new Error('fixture refuses outbound fetch'));
const binary = path.join(root, 'aac-http');
fs.copyFileSync(path.join(__dirname, 'fake-aac-http.mjs'), binary);
fs.chmodSync(binary, 0o755);
const { startServer } = require(path.join(repo, 'dist/web-server'));
const http = require('node:http');
(async () => {
  // Arm only after fixture and module preparation. The shim affects this exact path once.
  process.env[kind === 'metadata' ? 'AAC_SLOW_STAT_PATH' : 'AAC_SLOW_READ_PATH'] = path.join(
    ccs,
    kind === 'metadata' ? 'run' : 'run/aac-http.json'
  );
  process.env.AAC_SLOW_MARKER = path.join(root, 'delayed');
  const began = performance.now();
  let previous = began,
    maxGapMs = 0,
    maxHttpMs = 0,
    responses = 0;
  const heartbeat = setInterval(() => {
    const now = performance.now();
    maxGapMs = Math.max(maxGapMs, now - previous);
    previous = now;
  }, 20);
  const instance = await startServer({
    port: 0,
    host: '127.0.0.1',
    rustService: mode,
    rustServiceBinary: binary,
  });
  try {
    const port = instance.server.address().port;
    const until = began + (kind === 'metadata' ? 5500 : 1800);
    do {
      const started = performance.now();
      await new Promise((resolve, reject) => {
        http
          .get(`http://127.0.0.1:${port}/api/health`, (res) => {
            if (res.statusCode !== 200) reject(new Error('health status'));
            res.resume();
            res.on('end', resolve);
          })
          .on('error', reject);
      });
      maxHttpMs = Math.max(maxHttpMs, performance.now() - started);
      responses++;
      await new Promise((resolve) => setTimeout(resolve, 25));
    } while (performance.now() < until);
    console.log(
      JSON.stringify({
        maxHttpMs,
        maxGapMs,
        responses,
        delayed: fs.existsSync(path.join(root, 'delayed')),
        elapsed: performance.now() - began,
        diagnostics: instance.rustService?.diagnostics?.(),
      })
    );
  } finally {
    clearInterval(heartbeat);
    instance.cleanup();
    await new Promise((resolve) => instance.server.close(resolve));
  }
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});

/**
 * L1 measurement worker: the real dashboard server from a built dist/ under Node, in a fixture
 * home, inside the same closed boundary as tests/spec/capture.ts: no process but the given
 * aac-http binary (with no arguments), no network but loopback and Unix sockets, no private path
 * outside the fixture, and the background owners that would collect from providers stubbed. It is
 * forked by bridge-measure.mjs and answers its IPC requests: `stats` (RSS of TS and of the Rust
 * child, event-loop delay since the last read, bridge counters, the L1 diagnostics when the build
 * has them), `heap` (a heap snapshot summary) and `stop`.
 *
 *   node tests/spec/bridge-measure-server.cjs <dist> <binary> <mode> <home>
 */
'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');
const net = require('net');
const v8 = require('v8');
const { monitorEventLoopDelay } = require('perf_hooks');

const [dist, binary, given, home] = process.argv.slice(2);
// `off+owners`: off, with the Status and Scoreboard owners running on the fixture share (heap).
// R1: `on+home` offers Home with D7's path (AAC_RUST_HOME_VIEW=on); `on+fed` answers it from fed generations
// (the Home group on).
const [mode, ...flags] = given.split('+');
const owners = flags.includes('owners');
const repo = path.resolve(__dirname, '../..');
const ccs = path.join(home, '.ccs');
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: home,
  CCS_DIR: ccs,
  CODEX_HOME: path.join(home, '.codex'),
  CLAUDE_CONFIG_DIR: path.join(home, '.claude'),
  CCS_SESSION_SECRET: 'fixture-session-secret-for-measurement-only',
  AAC_NO_LOCAL_RUST_BUILD: '1',
  AAC_RUST_SERVICE: mode,
  AAC_AGENT_REPORTS_DIR: path.join(home, 'share'),
  AAC_RUST_HOME_VIEW: flags.includes('home') || flags.includes('fed') ? 'on' : 'off',
  AAC_RUST_GROUP_HOME: flags.includes('fed') ? 'on' : 'off',
});

// ---- the closed boundary ----------------------------------------------------------------
os.homedir = () => home;
// The measured build's own checkout (its dist/ and node_modules/) may be another worktree.
const built = path.dirname(path.resolve(dist));
const allowed = [home, repo, built, path.resolve(dist), path.dirname(path.resolve(binary))];
for (const root of [repo, built])
  try {
    allowed.push(fs.realpathSync(path.join(root, 'node_modules')));
  } catch {
    /* No modules there. */
  }
function guard(input) {
  if (typeof input !== 'string' && !(input instanceof URL)) return;
  const target = path.resolve(input instanceof URL ? input.pathname : input);
  if (allowed.some((base) => target === base || target.startsWith(base + '/'))) return;
  if (
    target.startsWith('/home/') ||
    target.startsWith('/mnt/') ||
    target.startsWith('/Volumes/') ||
    /\/(?:\.ccs|\.claude|\.codex|\.t3|\.config|\.local)(?:\/|$)/.test(target)
  )
    throw Object.assign(new Error('Outside fixture filesystem'), { code: 'EACCES' });
}
for (const name of [
  'readFileSync',
  'writeFileSync',
  'readdirSync',
  'statSync',
  'lstatSync',
  'openSync',
  'existsSync',
  'mkdirSync',
  'readFile',
  'writeFile',
  'readdir',
  'stat',
  'lstat',
  'open',
]) {
  const original = fs[name];
  if (typeof original !== 'function') continue;
  fs[name] = function (input, ...args) {
    if (name === 'existsSync') {
      try {
        guard(input);
      } catch {
        return false;
      }
    } else guard(input);
    return original.call(this, input, ...args);
  };
}
for (const name of ['readFile', 'writeFile', 'readdir', 'stat', 'lstat', 'open', 'mkdir']) {
  const original = fs.promises[name];
  fs.promises[name] = function (input, ...args) {
    guard(input);
    return original.call(this, input, ...args);
  };
}
const cp = require('child_process');
const realSpawn = cp.spawn;
const refuse = () => {
  throw new Error('Fixture process boundary');
};
cp.spawn = function (file, args, options) {
  if (mode !== 'off' && file === binary && Array.isArray(args) && args.length === 0)
    return realSpawn.call(this, file, args, options);
  return refuse();
};
for (const name of ['spawnSync', 'exec', 'execSync', 'execFile', 'execFileSync', 'fork'])
  cp[name] = refuse;
const connect = net.Socket.prototype.connect;
net.Socket.prototype.connect = function (...args) {
  const options = args[0];
  if (typeof options === 'object' && options && options.path) guard(options.path);
  else {
    const host = typeof options === 'object' && options ? options.host : args[1];
    if (!['127.0.0.1', '::1', 'localhost', undefined].includes(host))
      throw new Error('External socket forbidden');
  }
  return connect.apply(this, args);
};
const https = require('https');
https.request = https.get = () => {
  throw new Error('External HTTPS forbidden');
};
const realFetch = globalThis.fetch;
globalThis.fetch = (url, options) => {
  const parsed = new URL(typeof url === 'string' ? url : (url.url ?? String(url)));
  if (parsed.hostname !== '127.0.0.1') throw new Error('External fetch forbidden');
  return realFetch(url, options);
};

// ---- the owners that would collect stay stubbed (capture.ts's list) -----------------------
function stub(file, replacements) {
  const module = require(path.join(dist, file));
  for (const [name, value] of Object.entries(replacements))
    if (name in module) module[name] = value;
}
const noop = () => {};
stub('utils/managed-process.js', {
  startProcessBroker: async () => {},
  stopProcessBroker: async () => {},
  runManagedCommand: async () => ({ stdout: '', stderr: '', exitCode: 1, timedOut: false }),
});
stub('web-server/services/account-analytics-service.js', {
  startAccountAnalyticsSampling: noop,
  stopAccountAnalyticsSampling: noop,
});
stub('web-server/services/account-lifecycle-runtime.js', {
  startAccountLifecycleMaintenance: noop,
  stopAccountLifecycleMaintenance: noop,
});
if (!owners) {
  stub('web-server/status/status-service.js', { startStatusService: () => noop });
  stub('web-server/scoreboard/scoreboard-service.js', { startScoreboardService: noop });
}
stub('antigravity/production-runtime.js', { getInstalledAntigravityRuntimeFactory: () => null });
stub('store/store-service.js', {
  openAacStore: async () => ({ store: null, reason: 'disabled' }),
});
// R1: the large home's dashboard rows reach the real AccountDashboardService through the additional-accounts
// collector's snapshot (no provider is asked).
const accountsFile = path.join(home, 'dashboard-accounts.json');
if (fs.existsSync(accountsFile)) {
  const rows = JSON.parse(fs.readFileSync(accountsFile, 'utf8'));
  stub('web-server/services/additional-account-service.js', {
    getAdditionalDashboardSnapshot: async () => ({ registry: 'v1', accounts: structuredClone(rows) }),
    getConfiguredAdditionalAccounts: () => null,
  });
  stub('web-server/services/opencode-console-wallet-service.js', {
    getOpenCodeConsoleWalletAccounts: async () => [],
  });
}
const renewal = require(path.join(dist, 'web-server/services/codex-profile-renewal-service.js'));
renewal.CodexProfileRenewalService.prototype.start = noop;

// ---- start -----------------------------------------------------------------------------
const delay = monitorEventLoopDelay({ resolution: 10 });
delay.enable();
(async () => {
  const { createEmptyUnifiedConfig } = require(path.join(dist, 'config/unified-config-types.js'));
  const { saveUnifiedConfig } = require(path.join(dist, 'config/unified-config-loader.js'));
  const bcrypt = require(path.join(repo, 'node_modules/bcrypt'));
  const config = createEmptyUnifiedConfig();
  config.dashboard_auth = {
    enabled: true,
    username: 'fixture-admin',
    password_hash: bcrypt.hashSync('fixture-password-only', 4),
    session_timeout_hours: 24,
  };
  saveUnifiedConfig(config);
  const ui = path.join(home, 'ui');
  fs.mkdirSync(path.join(ui, 'app'), { recursive: true });
  fs.writeFileSync(path.join(ui, 'app', 'index.html'), '<div id="aac-app-fixture"></div>');
  const { startServer } = require(path.join(dist, 'web-server/index.js'));
  const instance = await startServer({
    port: 0,
    host: '127.0.0.1',
    staticDir: ui,
    rustServiceBinary: binary,
  });
  const rust = instance.rustService;
  function rustPid() {
    const pid = rust.lease && rust.lease.pid;
    if (pid) return pid;
    // Older bridges have no pid on the lease: find this worker's aac-http child.
    for (const entry of fs.readdirSync('/proc')) {
      if (!/^\d+$/.test(entry)) continue;
      try {
        const status = fs.readFileSync(`/proc/${entry}/status`, 'utf8');
        if (/^Name:\s+aac-http\S*$/m.test(status) && new RegExp(`^PPid:\\s+${process.pid}$`, 'm').test(status))
          return Number(entry);
      } catch {
        /* Gone. */
      }
    }
    return null;
  }
  function rss(pid) {
    try {
      const match = /^VmRSS:\s+(\d+)\s+kB$/m.exec(fs.readFileSync(`/proc/${pid}/status`, 'utf8'));
      return match ? Number(match[1]) * 1024 : null;
    } catch {
      return null;
    }
  }
  process.on('message', (message) => {
    if (message.type === 'stats') {
      const pid = rustPid();
      const reply = {
        type: 'stats',
        tsRssBytes: process.memoryUsage().rss,
        heapUsedBytes: process.memoryUsage().heapUsed,
        rustRssBytes: pid ? rss(pid) : null,
        eventLoop: {
          p50Ms: delay.percentile(50) / 1e6,
          p99Ms: delay.percentile(99) / 1e6,
          maxMs: delay.max / 1e6,
        },
        bridge: { ...rust.stats },
        diagnostics: typeof rust.diagnostics === 'function' ? rust.diagnostics() : null,
      };
      if (message.reset) delay.reset();
      process.send(reply);
    } else if (message.type === 'heap') {
      global.gc && global.gc();
      const file = v8.writeHeapSnapshot(message.file);
      process.send({ type: 'heap', file });
    } else if (message.type === 'stop') {
      instance.cleanup();
      instance.wss.close();
      instance.server.closeAllConnections();
      instance.server.close(() => process.exit(0));
      setTimeout(() => process.exit(0), 2000).unref();
    }
  });
  process.send({ type: 'ready', port: instance.server.address().port, pid: process.pid });
})().catch((error) => {
  process.send({ type: 'error', message: String(error && error.stack ? error.stack : error) });
  process.exit(1);
});

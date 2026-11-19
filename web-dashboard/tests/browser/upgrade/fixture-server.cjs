// The upgrade kit's server: the packaged dist/ui served by the production static helpers (dist/web-server/static-ui.js,
// so the page routes, the redirect of unknown paths and the headers are the product's), synthetic APIs only (no
// collectors, no account data), and a control endpoint that plays the previous release: its service worker
// (fixtures/previous: f52e1a7c6's sw.js and sw-route.js, byte for byte, under an older stamp), slower classic page
// routes, or one failing address.
// Adapted from the Astra max review's witness (2026-11-14-parallel/astra-leptos-evidence/witness).
//   node fixture-server.cjs <port> <repo>
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { createRequire } = require('node:module');

const port = Number(process.argv[2]);
const repo = path.resolve(process.argv[3]);
const load = createRequire(path.join(repo, 'package.json'));
const express = load('express');
const { loadStaticUi, precompressedStatic, uiStaticHeaders, pageRouteHandler } = load(
  './dist/web-server/static-ui.js'
);
const uiDir = path.join(repo, 'dist', 'ui');
const ui = loadStaticUi(uiDir);
const OLD = path.join(__dirname, 'fixtures', 'previous');
/** The previous release's shell stamp, as its installed worker carries it (any 12 hex digits other than ours). */
const OLD_STAMP = 'f52e1a7c6000';
const PROVIDERS = new Set(['claude', 'codex', 'antigravity', 'cursor', 'muse', 'kimi-code', 'qwen', 'zai']);

const state = { oldWorker: false, uiVersion: 'current', delayClassicMs: 0, failPath: null };
const previous = {};
for (const [name, env] of [['previous', 'AAC_UPGRADE_PREVIOUS_UI'], ['fix1', 'AAC_UPGRADE_FIX1_UI']]) {
  if (process.env[env]) previous[name] = loadStaticUi(path.resolve(process.env[env]));
}
// A second build of this checkout (reload.check.mjs): dist/ui copied with its runtime folder under another id and a
// new shell stamp, so its worker installs as a different build. Only the ids are rewritten; the precompressed copies
// are left out (identity serving). Removed when the server stops.
const SECOND_DIR = path.join(repo, 'dist', '.upgrade-second-ui');
function secondBuild() {
  const manifest = JSON.parse(fs.readFileSync(path.join(uiDir, 'ui-build-manifest.json'), 'utf8'));
  const flip = (id) => id.replace(/[0-9a-f]/g, (c) => ((parseInt(c, 16) + 8) % 16).toString(16));
  const pkg = manifest.buildId;
  const stamp = manifest.shellBuildId;
  const ids = { pkg: flip(pkg), stamp: flip(stamp) };
  fs.rmSync(SECOND_DIR, { recursive: true, force: true });
  fs.cpSync(uiDir, SECOND_DIR, {
    recursive: true,
    filter: (source) => !/\.(br|gz)$/.test(source),
  });
  fs.renameSync(path.join(SECOND_DIR, 'pkg', pkg), path.join(SECOND_DIR, 'pkg', ids.pkg));
  const rewrite = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const file = path.join(dir, entry.name);
      if (entry.isDirectory()) rewrite(file);
      else if (/\.(html|js|mjs|json)$/.test(entry.name)) {
        const text = fs.readFileSync(file, 'utf8');
        let next = text.replaceAll(`pkg/${pkg}/`, `pkg/${ids.pkg}/`).replaceAll(`pkg/${pkg}`, `pkg/${ids.pkg}`);
        if (entry.name === 'sw.js' && dir === SECOND_DIR) next = next.replace(`'${stamp}'`, `'${ids.stamp}'`);
        if (next !== text) fs.writeFileSync(file, next);
      }
    }
  };
  rewrite(SECOND_DIR);
  const record = { ...manifest, buildId: ids.pkg, shellBuildId: ids.stamp, precompressed: [] };
  if (manifest.wasm?.path) record.wasm = { ...manifest.wasm, path: manifest.wasm.path.replace(pkg, ids.pkg) };
  fs.writeFileSync(path.join(SECOND_DIR, 'ui-build-manifest.json'), JSON.stringify(record, null, 2));
  return loadStaticUi(SECOND_DIR);
}
if (fs.existsSync(path.join(uiDir, 'ui-build-manifest.json'))) {
  previous.second = secondBuild();
  const drop = () => fs.rmSync(SECOND_DIR, { recursive: true, force: true });
  process.on('exit', drop);
  for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => process.exit(0));
}
const app = express();
app.use(express.json());

// plain text: an HTML page here would be mirrored to the start page by the old worker
app.get('/__upgrade/ready', (req, res) => res.type('text/plain').send('fixture only'));
app.post('/__upgrade/control', (req, res) => {
  for (const key of Object.keys(state)) if (key in req.body) state[key] = req.body[key];
  res.json(state);
});

// the previous release's worker, while the test plays it
app.get('/sw.js', (req, res, next) => {
  if (!state.oldWorker) return next();
  const source = fs.readFileSync(path.join(OLD, 'sw.js'), 'utf8').replace('__AAC_BUILD_ID__', OLD_STAMP);
  res.set('Cache-Control', 'no-cache').type('js').send(source);
});
app.get('/sw-route.js', (req, res, next) => {
  if (!state.oldWorker) return next();
  res.set('Cache-Control', 'no-cache').type('js').send(fs.readFileSync(path.join(OLD, 'sw-route.js')));
});
// one address that fails (a new worker's install must then fail and leave the installed copy alone)
app.use((req, res, next) => {
  if (state.failPath && req.path === state.failPath) {
    res.status(503).type('text/plain').send('Fixture failure.');
    return;
  }
  next();
});

const signedIn = { authenticated: true, authRequired: true, accessMode: 'password', username: 'fixture' };
const api = {
  '/api/auth/check': () => signedIn,
  '/api/auth/setup': () => ({ configured: true, sessionTimeoutHours: 24 }),
  '/api/auth/session': () => ({ username: 'fixture', otherBrowsers: 0, secureTransport: true, managedBy: 'config' }),
  '/api/auth/devices': () => ({ devices: [] }),
  '/api/auth/network': () => ({
    trustLocalNetwork: false,
    connection: { peer: '127.0.0.1', trusted: true },
    canTurnOn: false,
  }),
  '/api/accounts/dashboard': () => ({
    schemaVersion: 1,
    updatedAt: new Date().toISOString(),
    accounts: [],
    providers: [],
    settings: { refreshIntervalSeconds: 60, hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] },
    codexAutoSwitch: { enabled: false, thresholdPercent: 5 },
  }),
  '/api/accounts/settings': () => ({ refreshIntervalSeconds: 60 }),
  '/api/accounts/preferences': () => ({ timeZone: 'UTC' }),
  '/api/accounts/registry': () => ({ providers: [], accounts: [], jobs: [], trash: [] }),
  '/api/claude/desktop-profiles': () => ({ profiles: [] }),
  '/api/antigravity/profiles': () => ({ schemaVersion: 1, hostId: 'ubuntu', activationSupported: false, profiles: [] }),
  '/api/antigravity/auto-switch': () => ({
    enabled: false,
    thresholdUsedPercent: 95,
    pollIntervalSeconds: 60,
    maxQuotaAgeSeconds: 300,
    cooldownSeconds: 300,
    selectedHostIds: [],
    requestedPoolId: null,
    outcome: 'setup_required',
    message: 'Fixture server.',
    activationInProgress: false,
  }),
  '/api/app-updates/status': () => ({ job: null }),
  '/api/accounts/analytics': () => load('./web-dashboard/tests/browser/device/analytics-fixture.mjs').analyticsFixture({ range: '7d', now: Date.parse('2026-11-16T02:00:00Z'), accounts: load('./web-dashboard/tests/browser/home-fixture.mjs').homeAccounts().accounts }),
};
app.use('/api', (req, res) => {
  const answer = api[`/api${req.path}`];
  res
    .set('Cache-Control', 'no-store')
    .status(answer ? 200 : 404)
    .json(answer ? answer() : { error: 'Fixture endpoint not supplied.' });
});

// A slow start: the classic page routes (not /app/<tab>, not files) answer late, as a busy server does on the first
// launch after an update.
app.use((req, res, next) => {
  const classicRoute = !req.path.startsWith('/app') && !path.posix.basename(req.path).includes('.');
  if (state.delayClassicMs > 0 && classicRoute) setTimeout(next, state.delayClassicMs);
  else next();
});
app.use((req, res, next) => {
  const selected = state.uiVersion === 'current' ? ui : previous[state.uiVersion];
  if (!selected) return res.status(500).type('text/plain').send('Supply the retained UI paths for the cross-version check.');
  precompressedStatic(selected)(req, res, () =>
    express.static(selected.root, { index: false, setHeaders: uiStaticHeaders(selected) })(req, res, () =>
      pageRouteHandler(selected, PROVIDERS)(req, res, next)));
});

app.listen(port, process.env.AAC_UPGRADE_BIND || '127.0.0.1', () => console.log(`upgrade fixture server on http://127.0.0.1:${port}/`));

// Development server for the Leptos pages with fixture data. Never real data, never real answers.
// Usage: node web-dashboard/leptos/dev/serve.mjs [port] [fixture.json]
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const crate = path.resolve(here, '..');
const dashboard = path.resolve(crate, '..');
const port = Number(process.argv[2] || 8790);
const fixture = process.argv[3] || path.join(crate, 'fixtures/status-design-view.json');
const fixtureJson = (name) => JSON.parse(fs.readFileSync(path.join(crate, 'fixtures', name), 'utf8'));
let view = JSON.parse(fs.readFileSync(fixture, 'utf8'));
const types = { '.mjs': 'text/javascript', '.webmanifest': 'application/manifest+json', '.mp4': 'video/mp4', '.jpg': 'image/jpeg', '.png': 'image/png', '.js': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css', '.ttf': 'font/ttf', '.svg': 'image/svg+xml', '.html': 'text/html; charset=utf-8', '.json': 'application/json' };

function file(res, p) {
  fs.readFile(p, (err, body) => {
    if (err) { res.writeHead(404).end('not found'); return; }
    res.writeHead(200, { 'Content-Type': types[path.extname(p)] || 'application/octet-stream', 'Cache-Control': 'no-store' });
    res.end(body);
  });
}

// Media with HTTP Range support: iOS Safari plays a video only when the server answers ranges.
function ranged(req, res, p) {
  fs.stat(p, (err, st) => {
    if (err) { res.writeHead(404).end('not found'); return; }
    const type = types[path.extname(p)] || (p.endsWith('.mp4') ? 'video/mp4' : p.endsWith('.jpg') ? 'image/jpeg' : 'application/octet-stream');
    const m = /bytes=(\d*)-(\d*)/.exec(req.headers.range || '');
    if (!m) { res.writeHead(200, { 'Content-Type': type, 'Content-Length': st.size, 'Accept-Ranges': 'bytes' }); fs.createReadStream(p).pipe(res); return; }
    const start = m[1] ? Number(m[1]) : st.size - Number(m[2]);
    const end = m[1] && m[2] ? Math.min(Number(m[2]), st.size - 1) : st.size - 1;
    res.writeHead(206, { 'Content-Type': type, 'Content-Length': end - start + 1, 'Content-Range': `bytes ${start}-${end}/${st.size}`, 'Accept-Ranges': 'bytes' });
    fs.createReadStream(p, { start, end }).pipe(res);
  });
}

// The fixture's relative times, resolved against now, so ages read the same whenever the page is shot.
function liveView() {
  const now = Date.now();
  const today = (hhmm) => { const [h, m] = hhmm.split(':').map(Number); const d = new Date(); d.setHours(h, m, 0, 0); return d.toISOString(); };
  const cards = { ...view.cards, open: view.cards.open.map((c) => (c.askedAgo === undefined ? c : { ...c, askedAt: new Date(now - c.askedAgo * 1000).toISOString(), ageSeconds: c.askedAgo })) };
  const leases = view.simlock.leases.map((l) => (l.sinceClock ? { ...l, since: today(l.sinceClock), expiresAt: today(l.expiresClock) } : l));
  return { ...view, generatedAt: new Date(now).toISOString(), cards, simlock: { ...view.simlock, leases }, spendByStream: view.spendByStream ?? fixtureJson('spend-by-stream.json') };
}

// The reads of the frame's dashboard controller (public/frame-dashboard.mjs) for Home and Accounts: an honest empty
// dashboard (no accounts reported), so a frame check that visits those tabs draws their empty state with nothing
// failing. Made-up, never real. A check that needs rows hands its own view to the page (the `aac-dash` event).
const DASHBOARD_READS = {
  '/api/accounts/dashboard': () => ({ schemaVersion: 1, updatedAt: new Date().toISOString(), accounts: [], providers: [], settings: { refreshIntervalSeconds: 60, hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] }, codexAutoSwitch: { enabled: false, thresholdPercent: 5 } }),
  '/api/accounts/settings': () => ({ refreshIntervalSeconds: 60 }),
  '/api/accounts/preferences': () => ({ timeZone: 'America/New_York' }),
  '/api/claude/desktop-profiles': () => ({ profiles: [] }),
  '/api/antigravity/profiles': () => ({ schemaVersion: 1, hostId: 'ubuntu', activationSupported: false, profiles: [] }),
  '/api/antigravity/auto-switch': () => ({ enabled: false, thresholdUsedPercent: 95, pollIntervalSeconds: 60, maxQuotaAgeSeconds: 300, cooldownSeconds: 300, selectedHostIds: [], requestedPoolId: null, outcome: 'setup_required', message: 'Fixture server.', activationInProgress: false }),
  '/api/app-updates/status': () => ({ job: null }),
  '/api/accounts/registry': () => ({ providers: [], accounts: [], jobs: [], trash: [] }),
  '/api/auth/session': () => ({ username: 'fixture', otherBrowsers: 0, secureTransport: true, managedBy: 'config' }),
  '/api/auth/devices': () => ({ devices: [] }),
  '/api/auth/network': () => ({ trustLocalNetwork: false, connection: { peer: '127.0.0.1', trusted: true }, canTurnOn: false }),
};

function json(res, status, value) {
  res.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
  res.end(JSON.stringify(value));
}

function answer(card, choice, note, via) {
  view.cards.open = view.cards.open.filter((c) => c.id !== card.id);
  view.cards.answered.unshift({ card, answer: { kind: 'answer', choice, note, via, answeredAt: new Date().toISOString(), delivery: { state: 'rang', at: new Date().toISOString(), detail: 'Fixture server: nothing was sent.' } } });
  view.cards.batches = view.cards.batches.map((b) => ({ ...b, cardIds: b.cardIds.filter((id) => id !== card.id) }));
}

http.createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  const p = url.pathname;
  if (p === '/__reset') { view = JSON.parse(fs.readFileSync(fixture, 'utf8')); json(res, 200, { ok: true }); return; }
  if (/^\/app\/(home|analytics|accounts|status|scoreboard)$/.test(p)) {
    const html = fs.readFileSync(path.join(crate, 'index.html'), 'utf8').replaceAll('{{PKG}}', '/app/pkg').replaceAll('{{CSS}}', '/app/app.css');
    res.writeHead(200, { 'Content-Type': types['.html'], 'Cache-Control': 'no-store' }).end(html);
  } else if (p === '/__fixture/start-cover') file(res, path.join(dashboard, 'public/index.html'));
  // The frame warms the Home Screen start page 4 s after it starts (frame.rs warm_offline_copies): answer it as the
  // product does, so a check that runs longer has no 404 in its console.
  else if (p === '/' && !url.search) file(res, path.join(dashboard, 'public/index.html'));
  else if (p === '/__fixture/offline-cover') {
    const source = fs.readFileSync(path.join(dashboard, 'public/sw.js'), 'utf8');
    const html = vm.runInNewContext(source.match(/var OFFLINE_PAGE =([\s\S]*?);\r?\n/)[1]);
    res.writeHead(200, { 'Content-Type': types['.html'], 'Cache-Control': 'no-store' }).end(html);
  } else if (p === '/status-bar.css') file(res, path.join(dashboard, 'public/status-bar.css'));
  // The frame registers the product's service worker (frame.rs): serve it as the dashboard does, so a
  // fixture run has no 404 in its console (browser-check gap f8992b).
  else if (p === '/sw.js') file(res, path.join(dashboard, 'public/sw.js'));
  else if (/^\/[a-z-]+\.mjs$/.test(p)) file(res, path.join(dashboard, 'public', path.basename(p)));
  else if (p === '/computers.json') file(res, path.join(dashboard, 'public/computers.json'));
  else if (p === '/manifest.webmanifest') file(res, path.join(dashboard, 'public', 'manifest.webmanifest'));
  else if (p === '/api/auth/check') json(res, 200, { authenticated: true, authRequired: true, username: 'fixture', accessMode: 'password' });
  else if (p === '/api/auth/setup') json(res, 200, { configured: true, sessionTimeoutHours: 720 });
  else if (req.method === 'GET' && p in DASHBOARD_READS) json(res, 200, DASHBOARD_READS[p]());
  else if (p.startsWith('/app/marks/')) file(res, path.join(crate, 'src/ui/marks', path.basename(p)));
  else if (p.startsWith('/app/pkg/')) file(res, path.join(crate, 'pkg', path.basename(p)));
  else if (p === '/app/app.css') file(res, path.join(crate, 'style/app.css'));
  else if (p === '/app/MartianMono.ttf') file(res, path.join(dashboard, 'ui/fonts/MartianMono.ttf'));
  else if (p.startsWith('/assets/')) file(res, path.join(dashboard, 'public/assets', path.basename(p)));
  else if (p.startsWith('/api/status/files/')) ranged(req, res, path.join(crate, 'fixtures/media', path.basename(p)));
  else if (p === '/api/scoreboard' && req.method === 'GET') json(res, 200, { ...fixtureJson('scoreboard-view.json'), byModel: fixtureJson('scoreboard-by-model.json') });
  else if (p === '/api/status' && req.method === 'GET') json(res, 200, liveView());
  else if (p === '/api/status/answers' && req.method === 'POST') {
    let body = '';
    req.on('data', (d) => (body += d));
    req.on('end', () => {
      const data = JSON.parse(body || '{}');
      const results = (data.answers || []).map((e) => {
        const card = view.cards.open.find((x) => x.id === e.cardId);
        if (!card || card.seq !== e.cardSeq || card.cardSha256 !== e.cardSha256) return { cardId: e.cardId, outcome: 'card_changed', code: 'card_changed', error: 'The question changed.' };
        if (e.kind === 'done' || e.kind === 'object') {
          answer(card, [e.kind], e.note, e.via);
          return { cardId: e.cardId, outcome: 'recorded', code: '', error: '' };
        }
        if (!e.choice.length) {
          card.notes.push({ note: e.note, at: new Date().toISOString(), delivery: { state: 'rang', at: new Date().toISOString(), detail: '' } });
          return { cardId: e.cardId, outcome: 'recorded', code: '', error: '' };
        }
        answer(card, e.choice, e.note, e.via);
        return { cardId: e.cardId, outcome: 'recorded', code: '', error: '' };
      });
      json(res, 200, { results });
    });
  } else res.writeHead(404).end('not found');
}).listen(port, '0.0.0.0', () => console.log(`fixture server on http://0.0.0.0:${port}/app/status`));

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
  '/api/accounts/dashboard': () => ({ schemaVersion: 1, updatedAt: new Date().toISOString(), accounts: [], providers: [], settings: { refreshIntervalSeconds: 60, hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] } }),
  '/api/accounts/settings': () => ({ refreshIntervalSeconds: 60 }),
  '/api/accounts/preferences': () => ({ timeZone: 'America/New_York' }),
  '/api/claude/desktop-profiles': () => ({ profiles: [] }),
  '/api/antigravity/profiles': () => ({ schemaVersion: 1, hostId: 'ubuntu', profiles: [] }),
  '/api/app-updates/status': () => ({ job: null }),
  '/api/accounts/registry': () => ({ providers: [], accounts: [], jobs: [], trash: [] }),
  '/api/auth/session': () => ({ username: 'fixture', otherBrowsers: 0, secureTransport: true, managedBy: 'config' }),
  '/api/auth/devices': () => ({ devices: [] }),
  '/api/auth/network': () => ({ trustLocalNetwork: false, connection: { peer: '127.0.0.1', trusted: true }, canTurnOn: false }),
};

// An honest empty Analytics answer in the product's shape (account-analytics-compose.ts): no accounts, no
// samples, the requested range. Made-up, never real; the frame draws its empty state instead of a console
// 404 (TOOL-ISSUES rail3-T1). Custom ranges resolve like the product does (account-analytics-range.ts):
// whole local days in the dashboard's zone, and the same 400 for a shape the product refuses.
const ANALYTICS_TZ = 'America/New_York';
const DAY_MS = 24 * 3600 * 1000;
const PRESETS = ['24h', '7d', '30d', '90d', 'month', 'all', 'custom'];
const MAX_CUSTOM_DAYS = 31;
const BAD_RANGE = {
  status: 400,
  body: {
    error: 'Select whole days from the oldest stored day through today, with the start date on or before the end date.',
    code: 'invalid_range',
  },
};
const BAD_TZ = {
  status: 400,
  body: { error: 'Select a valid IANA time zone.', code: 'invalid_tz' },
};

// A real calendar date in `YYYY-MM-DD` form (isCalendarDate in account-analytics-range.ts).
function isCalendarDate(value) {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
  const [year, month, day] = value.split('-').map(Number);
  const epoch = Date.UTC(year, month - 1, day);
  return (
    year >= 1970 &&
    new Date(epoch).getUTCFullYear() === year &&
    new Date(epoch).getUTCMonth() === month - 1 &&
    new Date(epoch).getUTCDate() === day
  );
}

// The first instant of a local calendar day (localMidnight in account-analytics-range.ts).
function localMidnight(date, tz) {
  const asUTCText = (epoch) => {
    const parts = Object.fromEntries(
      new Intl.DateTimeFormat('en-US', {
        timeZone: tz,
        hourCycle: 'h23',
        year: 'numeric',
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
      })
        .formatToParts(new Date(epoch))
        .map((part) => [part.type, part.value])
    );
    return Date.parse(`${parts.year}-${parts.month}-${parts.day}T${parts.hour}:${parts.minute}:${parts.second}Z`);
  };
  const guess = Date.parse(`${date}T00:00:00Z`);
  let epoch = guess - (asUTCText(guess) - guess);
  epoch = guess - (asUTCText(epoch) - epoch);
  return epoch;
}

function addCalendarDays(date, days) {
  return new Date(Date.parse(`${date}T00:00:00Z`) + days * DAY_MS).toISOString().slice(0, 10);
}

// A valid IANA zone (isAccountAnalyticsTimeZone in account-analytics-range.ts): UTC, a zone the
// engine lists, or any name Intl.DateTimeFormat accepts; offset strings and non-zones are refused.
function isTimeZone(value) {
  if (typeof value !== 'string' || !/^[A-Za-z][A-Za-z0-9_+-]*(?:\/[A-Za-z0-9_+-]+)*$/.test(value))
    return false;
  if (value.length > 64) return false;
  if (value === 'UTC') return true;
  try {
    if (Intl.supportedValuesOf?.('timeZone').includes(value)) return true;
  } catch {
    /* the engine may not list zones; the formatter below still decides */
  }
  try {
    new Intl.DateTimeFormat('en-US', { timeZone: value }).format(0);
    return true;
  } catch {
    return false;
  }
}

// The spelling a valid zone is echoed under (canonicalAccountAnalyticsTimeZone): a listed name as
// sent, a case variant of a listed name in its listed spelling, else the engine's resolved name.
function canonicalTimeZone(value) {
  if (value === 'UTC') return 'UTC';
  let zones = null;
  try {
    zones = Intl.supportedValuesOf?.('timeZone') ?? null;
  } catch {
    zones = null;
  }
  if (zones?.includes(value)) return value;
  if (zones) {
    const listed = zones.find((zone) => zone.toLowerCase() === value.toLowerCase());
    if (listed !== undefined) return listed;
  }
  try {
    return new Intl.DateTimeFormat('en-US', { timeZone: value }).resolvedOptions().timeZone;
  } catch {
    return value;
  }
}

function emptyAnalytics(url) {
  const q = url.searchParams;
  const now = Date.now();
  const preset = q.get('range') ?? '30d';
  const from = q.get('from');
  const to = q.get('to');
  const dayNumber = (date) => Math.round(Date.parse(`${date}T00:00:00Z`) / DAY_MS);
  // The shape checks the product makes before anything else (validateAccountAnalyticsRangeShape); the
  // clock-dependent checks (retention, future days) stay the product's, not a fixture's.
  if (!PRESETS.includes(preset)) return BAD_RANGE;
  // The zone the request asks for, validated like the product (invalid_tz); absent keeps the
  // dashboard's zone. Canonicalized so a case variant or alias echoes the engine's spelling.
  const tzParam = q.get('tz');
  if (tzParam !== null && !isTimeZone(tzParam)) return BAD_TZ;
  const tz = tzParam === null ? ANALYTICS_TZ : canonicalTimeZone(tzParam);
  if (preset !== 'custom' && (from !== null || to !== null)) return BAD_RANGE;
  if (
    preset === 'custom' &&
    (!isCalendarDate(from) ||
      !isCalendarDate(to) ||
      from > to ||
      dayNumber(to) - dayNumber(from) + 1 > MAX_CUSTOM_DAYS)
  )
    return BAD_RANGE;
  const span = preset === '24h' ? DAY_MS : preset === '7d' ? 7 * DAY_MS : preset === '90d' ? 90 * DAY_MS : 30 * DAY_MS;
  const custom = preset === 'custom';
  const rangeFrom = custom ? localMidnight(from, tz) : preset === 'all' ? now : now - span;
  const rangeTo = custom ? localMidnight(addCalendarDays(to, 1), tz) - 1000 : now;
  // Guard the date arithmetic before constructing the response: a day whose next-day conversion
  // leaves the calendar (e.g. 9999-12-31 + 1) yields a non-finite instant; the product refuses such
  // a range, and the fixture must answer 400, never throw and disconnect the client.
  if (!Number.isFinite(rangeFrom) || !Number.isFinite(rangeTo)) return BAD_RANGE;
  return {
    status: 200,
    body: {
      schemaVersion: 1,
      updatedAt: new Date(now).toISOString(),
      range: {
        preset,
        from: new Date(rangeFrom).toISOString(),
        to: new Date(rangeTo).toISOString(),
        bucketMinutes: 60,
        tz,
        dayBucketTz: tz,
        availableFrom: new Date(rangeFrom).toISOString(),
        resolution: 'hour',
        ...(custom ? { days: { from, to } } : {}),
      },
      filters: {
        platform: q.get('platform') || 'all',
        provider: q.get('provider') || 'all',
        account: q.get('account') || 'all',
      },
      history: {
        status: 'ok',
        retentionDays: 30,
        collectedSince: null,
        oldestSampleAt: null,
        newestSampleAt: null,
        sampleCount: 0,
        message: 'No measured quota history is available for this selection yet.',
      },
      summary: {
        accountCount: 0,
        availableAccounts: 0,
        activeCodexAccountId: null,
        activeAccountIds: { codex: null, antigravity: null },
        sampleCount: 0,
      },
      providers: [],
      accounts: [],
      // The kept-view validator requires an activity object (frame-analytics.mjs); this is the
      // product's empty one (account-analytics-service.ts unavailableActivity): nothing in it.
      activity: {
        status: 'unavailable',
        scope: 'multi-host-cli',
        timezone: tz,
        accountAttribution: 'unavailable',
        costBasis: 'estimated-api-equivalent',
        fetchedAt: null,
        message: 'Fixture server: no local CLI activity.',
        totals: null,
        providers: [],
        byDay: [],
        byHour: [],
        models: [],
        byDayModel: [],
        sessions: null,
        anomalies: null,
        sources: [],
      },
    },
  };
}

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
  // The frame page answers its own addresses, the Home Screen start page (/, which the frame also warms 4 s after it
  // starts, frame.rs warm_offline_copies) and the start-cover fixture, as the product does since the classic page went.
  if (/^\/app\/(home|analytics|accounts|status|scoreboard)$/.test(p) || p === '/' || p === '/__fixture/start-cover') {
    const html = fs.readFileSync(path.join(crate, 'index.html'), 'utf8').replaceAll('{{PKG}}', '/app/pkg').replaceAll('{{CSS}}', '/app/app.css');
    res.writeHead(200, { 'Content-Type': types['.html'], 'Cache-Control': 'no-store' }).end(html);
  } else if (p === '/__fixture/offline-cover') {
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
  else if (p === '/app/MartianMono.ttf') file(res, path.join(crate, 'fonts/MartianMono.ttf'));
  else if (p.startsWith('/assets/')) file(res, path.join(dashboard, 'public/assets', path.basename(p)));
  else if (p.startsWith('/api/status/files/')) ranged(req, res, path.join(crate, 'fixtures/media', path.basename(p)));
  else if (p === '/api/scoreboard' && req.method === 'GET') json(res, 200, { ...fixtureJson('scoreboard-view.json'), byModel: fixtureJson('scoreboard-by-model.json') });
  // Details reads the reset inventory of the account it opens (reset-rows.mjs). The fixture has no reset
  // backend; answer the honest empty inventory of the contract (docs/reset-claims.md), never a 404
  // (TOOL-ISSUES Perf G5 row 39).
  else if (req.method === 'GET' && /^\/api\/accounts\/[^/]+\/reset-claims$/.test(p)) {
    json(res, 200, { status: 'ok', grants: [], history: [], pendingAttempt: null });
  } else if (p === '/api/accounts/analytics' && req.method === 'GET') {
    const answer = emptyAnalytics(url);
    json(res, answer.status, answer.body);
  }
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

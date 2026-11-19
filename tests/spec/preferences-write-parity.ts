/** Preferences HTTP and on-disk contract transcribed from account-preferences.test.ts. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import type { Server } from 'node:http';
import os from 'node:os';
import path from 'node:path';
import express from 'express';

const tsOnly = process.argv.includes('--ts-only');
const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http');
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-prefs-write-parity-'));
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { mode: 0o700 });
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: home,
  CCS_DIR: ccs,
  AAC_STORE: 'off',
  AAC_RUST_GROUP_WRITES: 'on',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_NO_LOCAL_RUST_BUILD: '1',
});
const { RustService } = await import('../../src/web-server/rust-service');
const { createAccountPreferencesRouter } = await import(
  '../../src/web-server/routes/account-preferences-routes'
);

const primary = path.join(ccs, 'dashboard-preferences.json');
const extra = path.join(ccs, 'dashboard-preferences-extra-sources.json');
const legacySource = {
  id: 'harness',
  tool: 'jsonl',
  host: 'ubuntu',
  path: '/var/log/fixture-harness',
  fieldMapping: { timestamp: 'ts', model: 'model' },
};
const nas1 = { id: 'omp-nas1', tool: 'omp', host: 'nas1', path: '/data/nas1/.omp/sessions' };
const alexis = { id: 'omp-alexis', tool: 'omp', host: 'alexis', path: 'C:\\fixture\\omp' };
const remoteJsonl = { ...legacySource, host: 'nas1' };
const clean = { timeZone: 'Europe/Paris', snapshotCleanup: { auto: true }, usageLogSources: [] };
const badSource = 'Generic JSONL usage log sources are supported only on ubuntu';
const badPreferences = 'Preferences are not valid.';

interface Probe {
  method: 'PUT' | 'PATCH';
  body: unknown;
  status: number;
  error?: string;
  session?: boolean;
  origin?: string;
  query?: string;
  type?: string;
}

const probes: Probe[] = [
  { method: 'PUT', body: clean, session: false, status: 401, error: 'Authentication required' },
  {
    method: 'PUT',
    body: clean,
    origin: 'http://evil.test',
    status: 403,
    error: 'Preferences require the dashboard origin.',
  },
  {
    method: 'PATCH',
    body: { timeZone: 'UTC' },
    origin: '',
    status: 403,
    error: 'Preferences require the dashboard origin.',
  },
  {
    method: 'PATCH',
    body: { timeZone: 'UTC' },
    query: '?extra=1',
    status: 400,
    error: 'Unexpected preferences query.',
  },
  {
    method: 'PUT',
    body: clean,
    type: 'text/plain',
    status: 415,
    error: 'Preferences require application/json.',
  },
  {
    method: 'PUT',
    body: { ...clean, timeZone: 'Mars/Olympus' },
    status: 400,
    error: badPreferences,
  },
  { method: 'PATCH', body: { timeZone: 'Mars/Olympus' }, status: 400, error: badPreferences },
  {
    method: 'PUT',
    body: { ...clean, usageLogSources: [{ ...nas1, path: 'C:\\logs' }] },
    status: 400,
    error: badPreferences,
  },
  {
    method: 'PUT',
    body: { ...clean, usageLogSources: [remoteJsonl] },
    status: 400,
    error: badSource,
  },
  { method: 'PATCH', body: { addUsageLogSource: remoteJsonl }, status: 400, error: badSource },
  { method: 'PATCH', body: {}, status: 400, error: badPreferences },
  {
    method: 'PUT',
    body: {
      ...clean,
      usageLogSources: [
        legacySource,
        { ...legacySource, id: 'overlap', path: legacySource.path + '/nested' },
      ],
    },
    status: 400,
    error: 'Usage log sources harness and overlap overlap',
  },
];

async function check(mode: 'off' | 'shadow' | 'on') {
  const service = new RustService(mode);
  let server: Server | undefined;
  try {
    if (mode === 'on') {
      await service.start(binary);
      assert.equal(service.stats.fallbacks, 0);
    }
    fs.rmSync(primary, { force: true });
    fs.rmSync(extra, { force: true });
    const app = express();
    app.use(express.json());
    app.use((req, res, next) => {
      res.locals.rustService = service;
      if (req.headers['x-fixture-session'] === 'yes')
        Object.assign(req, { sessionID: 'fixture-preferences', session: { authenticated: true } });
      next();
    });
    app.use('/api/accounts', createAccountPreferencesRouter());
    server = await new Promise<Server>((resolve) => {
      const listener = app.listen(0, '127.0.0.1', () => resolve(listener));
    });
    const address = server.address();
    assert.ok(address && typeof address !== 'string');
    const origin = 'http://127.0.0.1:' + address.port;
    const url = origin + '/api/accounts/preferences';
    const request = (probe: Probe) =>
      fetch(url + (probe.query ?? ''), {
        method: probe.method,
        headers: {
          origin: probe.origin ?? origin,
          'content-type': probe.type ?? 'application/json',
          ...(probe.session === false ? {} : { 'x-fixture-session': 'yes' }),
        },
        body: JSON.stringify(probe.body),
      });
    for (const probe of probes) {
      const response = await request(probe);
      assert.equal(response.status, probe.status, mode + ' ' + JSON.stringify(probe));
      assert.equal(response.headers.get('cache-control'), 'no-store');
      assert.equal(await response.text(), JSON.stringify({ error: probe.error }));
      assert.equal(fs.existsSync(primary), false);
      assert.equal(fs.existsSync(extra), false);
    }

    const saves = [
      [
        'PUT',
        { ...clean, usageLogSources: [alexis, legacySource] },
        { ...clean, usageLogSources: [legacySource, alexis] },
        [legacySource],
        [alexis],
      ],
      [
        'PATCH',
        { timeZone: 'UTC', snapshotCleanup: { auto: false } },
        {
          ...clean,
          timeZone: 'UTC',
          snapshotCleanup: { auto: false },
          usageLogSources: [legacySource, alexis],
        },
        [legacySource],
        [alexis],
      ],
      [
        'PATCH',
        { removeUsageLogSource: 'omp-alexis' },
        {
          ...clean,
          timeZone: 'UTC',
          snapshotCleanup: { auto: false },
          usageLogSources: [legacySource],
        },
        [legacySource],
        [],
      ],
      [
        'PATCH',
        { addUsageLogSource: alexis },
        {
          ...clean,
          timeZone: 'UTC',
          snapshotCleanup: { auto: false },
          usageLogSources: [legacySource, alexis],
        },
        [legacySource],
        [alexis],
      ],
      [
        'PUT',
        { ...clean, usageLogSources: [nas1] },
        { ...clean, usageLogSources: [nas1] },
        [nas1],
        [],
      ],
      ['PUT', clean, clean, [], []],
    ] as const;
    for (const [method, body, result, primarySources, extraSources] of saves) {
      const response = await request({ method, body, status: 200 });
      assert.equal(response.status, 200, mode + ' ' + JSON.stringify(body));
      assert.equal(await response.text(), JSON.stringify(result));
      assert.equal(
        fs.readFileSync(primary, 'utf8'),
        JSON.stringify({ ...result, usageLogSources: primarySources }) + '\n'
      );
      if (extraSources.length === 0) assert.equal(fs.existsSync(extra), false);
      else
        assert.equal(
          fs.readFileSync(extra, 'utf8'),
          JSON.stringify({ usageLogSources: extraSources }) + '\n'
        );
    }
    assert.equal(service.stats.served, mode === 'on' ? saves.length : 0, mode);
    assert.equal(service.stats.fallbacks, 0, mode);
  } finally {
    service.stop();
    if (server) await new Promise<void>((resolve) => server!.close(() => resolve()));
  }
}

try {
  await check('off');
  await check('shadow');
  if (!tsOnly) await check('on');
  console.log('preferences write parity: 18 cases x ' + (tsOnly ? 'off/shadow' : 'off/shadow/on'));
} finally {
  fs.rmSync(home, { recursive: true, force: true });
}

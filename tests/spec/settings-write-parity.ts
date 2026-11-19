/** The refresh settings route contract, with its real stores and an isolated front door. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import type { Server } from 'node:http';
import os from 'node:os';
import path from 'node:path';
import express from 'express';

const tsOnly = process.argv.includes('--ts-only');
const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http');
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-write-parity-'));
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { mode: 0o700 });
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: home,
  CCS_DIR: ccs,
  AAC_STORE: 'off',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_NO_LOCAL_RUST_BUILD: '1',
});

const { RustService } = await import('../../src/web-server/rust-service');
const { createAccountRefreshSettingsRouter } = await import(
  '../../src/web-server/routes/account-refresh-settings-routes'
);

const file = path.join(ccs, 'account-refresh-settings.json');
const cases = [
  {
    body: '{"refreshIntervalSeconds":120}',
    session: false,
    status: 401,
    error: 'Authentication required',
  },
  {
    body: '{"refreshIntervalSeconds":120}',
    origin: 'https://untrusted.example',
    status: 403,
    error: 'Settings require the dashboard origin.',
  },
  {
    body: '{"refreshIntervalSeconds":120}',
    origin: '',
    status: 403,
    error: 'Settings require the dashboard origin.',
  },
  {
    body: '{"refreshIntervalSeconds":120}',
    query: '?unexpected=1',
    status: 400,
    error: 'Unexpected settings query.',
  },
  {
    body: '{"refreshIntervalSeconds":120}',
    contentType: 'text/plain',
    status: 415,
    error: 'Settings require application/json.',
  },
  ...[
    '[]',
    '{}',
    '{"refreshIntervalSeconds":29}',
    '{"refreshIntervalSeconds":3601}',
    '{"refreshIntervalSeconds":60.5}',
    '{"refreshIntervalSeconds":"60"}',
    '{"refreshIntervalSeconds":60,"command":"untrusted"}',
  ].map((body) => ({
    body,
    status: 400,
    error: 'Choose a whole number of seconds from 30 to 3600.',
  })),
  ...[
    ['{"refreshIntervalSeconds":120}', 120],
    ['{"refreshIntervalSeconds":30}', 30],
    ['{"refreshIntervalSeconds":3.6e3}', 3600],
  ].map(([body, interval]) => ({ body: String(body), interval: Number(interval), status: 200 })),
] satisfies Array<{
  body: string;
  session?: boolean;
  origin?: string;
  query?: string;
  contentType?: string;
  status: number;
  error?: string;
  interval?: number;
}>;

async function run(mode: 'off' | 'shadow' | 'on'): Promise<void> {
  const service = new RustService(mode);
  let server: Server | undefined;
  try {
    if (mode === 'on') {
      await service.start(binary);
      assert.equal(service.stats.fallbacks, 0, 'the real writer must start');
    }

    fs.writeFileSync(file, '{"refreshIntervalSeconds":60}\n', { mode: 0o600 });
    const app = express();
    app.use(express.json());
    app.use((req, res, next) => {
      res.locals.rustService = service;
      if (req.headers['x-fixture-session'] === 'yes')
        Object.assign(req, { sessionID: 'fixture', session: { authenticated: true } });
      next();
    });
    let changed = 0;
    app.use('/api/accounts', createAccountRefreshSettingsRouter({ onChanged: () => changed++ }));
    server = await new Promise<Server>((resolve) => {
      const listener = app.listen(0, '127.0.0.1', () => resolve(listener));
    });
    const address = server.address();
    assert.ok(address && typeof address !== 'string');
    const base = `http://127.0.0.1:${address.port}`;
    let saved = '{"refreshIntervalSeconds":60}';
    let writes = 0;

    for (const probe of cases as ReadonlyArray<{
      body: string;
      session?: boolean;
      origin?: string;
      query?: string;
      contentType?: string;
      status: number;
      error?: string;
      interval?: number;
    }>) {
      const response = await fetch(base + '/api/accounts/settings' + (probe.query ?? ''), {
        method: 'PUT',
        headers: {
          origin: probe.origin ?? base,
          'content-type': probe.contentType ?? 'application/json',
          ...(probe.session === false ? {} : { 'x-fixture-session': 'yes' }),
          'x-aac-principal': '{"kind":"session","id":"forged","role":"admin"}',
        },
        body: probe.body,
      });
      const label = `${mode} ${JSON.stringify(probe)}`;
      assert.equal(response.status, probe.status, label);
      assert.equal(response.headers.get('cache-control'), 'no-store', label);
      const expected = probe.error
        ? JSON.stringify({ error: probe.error })
        : JSON.stringify({ refreshIntervalSeconds: probe.interval });
      assert.equal(await response.text(), expected, label);
      if (probe.interval !== undefined) {
        saved = expected;
        writes++;
      }
      assert.equal(changed, writes, label);
      assert.equal(fs.readFileSync(file, 'utf8'), saved + '\n', label);
      assert.equal(service.stats.served, mode === 'on' ? writes : 0, label);
      assert.equal(service.stats.fallbacks, 0, label);
    }
  } finally {
    service.stop();
    if (server) await new Promise<void>((resolve) => server!.close(() => resolve()));
  }
}

try {
  await run('off');
  await run('shadow');
  if (!tsOnly) await run('on');
  console.log(
    `settings write parity: ${cases.length} cases x ${tsOnly ? 'off/shadow' : 'off/shadow/on'}`
  );
} finally {
  fs.rmSync(home, { recursive: true, force: true });
}

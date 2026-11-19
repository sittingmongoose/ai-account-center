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
  AAC_RUST_GROUP_WRITES: 'on',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_NO_LOCAL_RUST_BUILD: '1',
});

const { withTsPrivateFileLeases } = await import('../../src/shared/private-files');
const { RustService } = await import('../../src/web-server/rust-service');
const { createAccountRefreshSettingsRouter } = await import(
  '../../src/web-server/routes/account-refresh-settings-routes'
);

const { createAccountAnalyticsRouter } = await import(
  '../../src/web-server/routes/account-analytics-routes'
);

const { createAccountVisibilityRouter } = await import(
  '../../src/web-server/routes/account-visibility-routes'
);
const visibilityFile = path.join(ccs, 'account-visibility.json');

const file = path.join(ccs, 'account-refresh-settings.json');
const captureFile = path.join(ccs, 'analytics-capture.json');
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
    let clears = 0;
    app.use(
      '/api/accounts',
      createAccountAnalyticsRouter({
        clearCwds: () => {
          assert.equal(fs.readFileSync(captureFile, 'utf8'), '{"cwd":false}\n');
          clears++;
          return { folders: 3, caches: 2 };
        },
      })
    );
    let visibilityChanges = 0;
    app.use(
      '/api/accounts',
      createAccountVisibilityRouter({
        onChanged: () => visibilityChanges++,
        audit: () => {},
      })
    );
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

    const capture = (
      body: string,
      origin = base,
      session = true,
      query = '',
      contentType = 'application/json'
    ) =>
      fetch(base + '/api/accounts/analytics/capture' + query, {
        method: 'PUT',
        headers: {
          origin,
          'content-type': contentType,
          ...(session ? { 'x-fixture-session': 'yes' } : {}),
        },
        body,
      });
    fs.writeFileSync(captureFile, '{"cwd":true}\n', { mode: 0o600 });
    for (const [request, status, expected] of [
      [
        () => capture('{"cwd":false}', base, false),
        401,
        '{"error":"Authentication required","code":"auth_required"}',
      ],
      [
        () => capture('{"cwd":false}', 'https://untrusted.example'),
        403,
        '{"error":"Capture settings require the dashboard origin.","code":"origin_required"}',
      ],
      [
        () => capture('{"cwd":false}', ''),
        403,
        '{"error":"Capture settings require the dashboard origin.","code":"origin_required"}',
      ],
      [
        () => capture('{"cwd":false}', base, true, '?unexpected=1'),
        400,
        '{"error":"Unexpected capture settings query.","code":"invalid_query"}',
      ],
      [
        () => capture('{"cwd":false}', base, true, '', 'text/plain'),
        415,
        '{"error":"Capture settings require application/json."}',
      ],
      ...['{}', '{"cwd":0}', '{"cwd":true,"extra":false}'].map(
        (body) =>
          [
            () => capture(body),
            400,
            JSON.stringify({
              error: 'Send {"cwd": true} or {"cwd": false}.',
              code: 'invalid_body',
            }),
          ] as const
      ),
    ] as const) {
      const response = await request();
      assert.equal(response.status, status, mode);
      assert.equal(await response.text(), expected, mode);
      assert.equal(fs.readFileSync(captureFile, 'utf8'), '{"cwd":true}\n');
      assert.equal(clears, 0);
    }
    for (const cwd of [false, true]) {
      const response = await capture(JSON.stringify({ cwd }));
      assert.equal(response.status, 200, mode);
      assert.equal(response.headers.get('cache-control'), 'no-store', mode);
      assert.equal(
        await response.text(),
        cwd ? '{"cwd":true}' : '{"cwd":false,"cleared":{"folders":3,"caches":2}}',
        mode
      );
      assert.equal(clears, 1, mode);
      assert.equal(fs.readFileSync(captureFile, 'utf8'), JSON.stringify({ cwd }) + '\n');
      writes++;
      assert.equal(service.stats.served, mode === 'on' ? writes : 0, mode);
      assert.equal(service.stats.fallbacks, 0, mode);
    }
    const visibility = (method: string, body: unknown, origin = base, session = true) =>
      fetch(base + '/api/accounts/visibility', {
        method,
        headers: {
          origin,
          'content-type': 'application/json',
          ...(session ? { 'x-fixture-session': 'yes' } : {}),
        },
        body: JSON.stringify(body),
      });
    const codex = 'codex:fixture@example.com';
    const claude = 'claude:fixture@example.com';
    fs.writeFileSync(
      visibilityFile,
      JSON.stringify({
        version: 1,
        hiddenProviders: ['codex', 'removed-provider'],
        hiddenAccountIds: [codex],
      }),
      { mode: 0o600 }
    );
    for (const [request, status, code] of [
      [() => visibility('PUT', { hiddenProviders: [] }, base, false), 401, 'auth_required'],
      [
        () =>
          visibility(
            'PATCH',
            { hiddenProviders: { show: ['codex'] } },
            'https://untrusted.example'
          ),
        403,
        'origin_required',
      ],
      [() => visibility('PUT', { hiddenProviders: ['unknown'] }), 400, 'invalid_body'],
      [
        () => visibility('PATCH', { hiddenAccountIds: { show: [codex], hide: [codex] } }),
        400,
        'invalid_body',
      ],
    ] as const) {
      const response = await request();
      assert.equal(response.status, status, mode);
      assert.equal((await response.json()).code, code, mode);
      assert.equal(visibilityChanges, 0, mode);
    }
    const patched = {
      hiddenProviders: ['codex'],
      hiddenAccountIds: [claude],
      trayHiddenProviders: [],
      trayHiddenAccountIds: [],
    };
    const replaced = { ...patched, trayHiddenProviders: ['claude'] };
    const empty = {
      hiddenProviders: [],
      hiddenAccountIds: [],
      trayHiddenProviders: [],
      trayHiddenAccountIds: [],
    };
    for (const [request, expected] of [
      [
        () => visibility('PATCH', { hiddenAccountIds: { show: [codex], hide: [claude, claude] } }),
        patched,
      ],
      [() => visibility('PUT', { trayHiddenProviders: ['claude', 'claude'] }), replaced],
      [
        () => {
          fs.writeFileSync(visibilityFile, 'torn');
          return visibility('PATCH', { hiddenProviders: { show: ['claude'] } });
        },
        null,
      ],
      [() => visibility('PUT', empty), empty],
    ] as const) {
      const response = await request();
      if (expected) {
        assert.equal(response.status, 200, mode);
        assert.equal(await response.text(), JSON.stringify(expected), mode);
        assert.equal(
          fs.readFileSync(visibilityFile, 'utf8'),
          JSON.stringify({ version: 1, ...expected }, null, 2) + '\n',
          mode
        );
        writes++;
      } else {
        assert.equal(response.status, 500, mode);
        assert.equal(
          await response.text(),
          '{"error":"Account visibility could not be read safely.","code":"visibility_unavailable"}',
          mode
        );
        assert.equal(fs.readFileSync(visibilityFile, 'utf8'), 'torn', mode);
      }
    }
    assert.equal(visibilityChanges, 3, mode);
    assert.equal(service.stats.served, mode === 'on' ? writes + 1 : 0, mode);
    assert.equal(service.stats.fallbacks, 0, mode);
    if (mode === 'on') {
      const save = (seconds: number) =>
        fetch(base + '/api/accounts/settings', {
          method: 'PUT',
          headers: { origin: base, 'content-type': 'application/json', 'x-fixture-session': 'yes' },
          body: JSON.stringify({ refreshIntervalSeconds: seconds }),
        });
      const before = fs.readFileSync(file, 'utf8');
      let release!: () => void;
      const gate = new Promise<void>((resolve) => {
        release = resolve;
      });
      const held = withTsPrivateFileLeases(ccs, ['account-refresh-settings'], () => gate, true);
      try {
        const blocked = await save(123);
        assert.equal(blocked.status, 500);
        await blocked.arrayBuffer();
        assert.equal(
          fs.readFileSync(file, 'utf8'),
          before,
          'live TS descriptor excludes the Rust writer'
        );
      } finally {
        release();
        await held;
      }
      const accepted = await save(123);
      assert.equal(accepted.status, 200);
      assert.equal(await accepted.text(), '{"refreshIntervalSeconds":123}');
      service.stop();
      const handedOff = await save(124);
      assert.equal(handedOff.status, 200, 'TS can take the lock after the Rust writer goes away');
      assert.equal(await handedOff.text(), '{"refreshIntervalSeconds":124}');
      assert.equal(fs.readFileSync(file, 'utf8'), '{"refreshIntervalSeconds":124}\n');
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
    `settings write parity: ${cases.length} refresh + 10 capture + 8 visibility cases x ${tsOnly ? 'off/shadow' : 'off/shadow/on'}`
  );
} finally {
  fs.rmSync(home, { recursive: true, force: true });
}

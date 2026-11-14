/** Real TS parser / Rust socket parity, using only a temporary private home. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Request, Response } from 'express';
const root = fileURLToPath(new URL('../..', import.meta.url));
const tsOnly = process.argv.includes('--ts-only');
const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.join(root, 'target/debug/aac-http');
if (!tsOnly)
  assert.ok(fs.existsSync(binary), 'Build the real Rust service on Nas1 or the Mac first');
const rows = JSON.parse(
  fs.readFileSync(path.join(root, 'tests/spec/settings-numeric-forms.json'), 'utf8')
) as { name: string; file: string; ts: number; owner: 'rust' | 'ts-fallback' }[];
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-parity-'));
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { mode: 0o700 });
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, { CCS_HOME: home, CCS_DIR: ccs, AAC_NO_LOCAL_RUST_BUILD: '1' });
const { RustService } = await import('../../src/web-server/rust-service');
const { readAccountRefreshSettings } =
  await import('../../src/web-server/services/account-refresh-settings');
const { requestRust } = await import('../../src/web-server/rust-service-transport');
const on = new RustService('on');
const shadow = new RustService('shadow');
const off = new RustService('off');
const active = new Set<import('node:http').ClientRequest>();
try {
  if (!tsOnly) {
    await on.start(binary);
    assert.equal(on.stats.fallbacks, 0, 'real service must start; no all-TS false pass');
    await shadow.start(binary);
    assert.equal(shadow.stats.fallbacks, 0, 'shadow must attach to the real service');
  }
  const boot = tsOnly
    ? null
    : JSON.parse(fs.readFileSync(path.join(ccs, 'run/aac-http.json'), 'utf8'));
  const req = {
    method: 'GET',
    originalUrl: '/api/accounts/settings',
    sessionID: 'fixture-session',
    session: { authenticated: true },
  } as unknown as Request;
  for (const row of rows) {
    fs.writeFileSync(path.join(ccs, 'account-refresh-settings.json'), row.file, { mode: 0o600 });
    const expected = readAccountRefreshSettings(ccs);
    assert.deepEqual(
      expected,
      { refreshIntervalSeconds: row.ts },
      `${row.name}: real TS parser result`
    );
    if (tsOnly) continue;
    const raw = await requestRust(
      boot,
      '/api/accounts/settings',
      { kind: 'session', id: 'fixture-admin', role: 'admin' },
      active
    );
    for (const service of [off, on, shadow]) {
      const before = { ...service.stats };
      let answer: unknown;
      let tsCalls = 0;
      const res = {
        json: (body: unknown) => {
          answer = body;
        },
      } as Response;
      await service.respond(req, res, '/api/accounts/settings', () => {
        tsCalls++;
        return readAccountRefreshSettings(ccs);
      });
      assert.deepEqual(
        answer,
        expected,
        `${row.name}: ${service.mode} must match the real TS parser`
      );
      if (service.mode === 'on') {
        assert.equal(
          tsCalls,
          row.owner === 'rust' ? 0 : 1,
          `${row.name}: TS fallback called exactly when unsupported`
        );
        assert.equal(
          service.stats.served - before.served,
          row.owner === 'rust' ? 1 : 0,
          `${row.name}: no Rust guess counted as served`
        );
        assert.equal(
          service.stats.fallbacks - before.fallbacks,
          row.owner === 'rust' ? 0 : 1,
          `${row.name}: unsupported inputs use the logged fallback path`
        );
      } else {
        assert.equal(tsCalls, 1, `${row.name}: ${service.mode} serves TS exactly once`);
        if (service.mode === 'shadow') {
          const deadline = Date.now() + 2000;
          while (service.stats.shadowed === before.shadowed && Date.now() < deadline)
            await new Promise((r) => setTimeout(r, 5));
          assert.equal(
            service.stats.shadowed - before.shadowed,
            1,
            `${row.name}: real shadow result observed`
          );
          assert.equal(
            service.stats.differences - before.differences,
            0,
            `${row.name}: unsupported is fallback, not a guessed comparison`
          );
          assert.equal(
            service.stats.fallbacks - before.fallbacks,
            row.owner === 'rust' ? 0 : 1,
            `${row.name}: shadow logs unsupported fallback`
          );
        }
      }
    }
    assert.equal(
      raw.status,
      row.owner === 'rust' ? 200 : 503,
      `${row.name}: explicit wire fallback status`
    );
    if (raw.status === 200)
      assert.deepEqual(raw.body, expected, `${row.name}: Rust answer proven equal`);
    assert.equal(
      fs.readFileSync(path.join(ccs, 'account-refresh-settings.json'), 'utf8'),
      row.file,
      `${row.name}: read only`
    );
  }
  console.log(
    tsOnly
      ? `${rows.length} numeric/document forms checked against the real TS parser`
      : `${rows.length} numeric/document forms x off/on/shadow: TS parity, explicit fallback and no writes passed`
  );
} finally {
  for (const request of active) request.destroy();
  shadow.stop();
  on.stop();
  off.stop();
  fs.rmSync(home, { recursive: true, force: true });
}

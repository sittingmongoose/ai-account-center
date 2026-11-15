/**
 * Big review M4/L4: on-mode admission of Home's number witnesses through the production bridge. GET
 * /api/views/home composes with `RustService.compose` from the front door's own envelope (`homeReadInput`) and
 * serves only an answer `isHomeView` accepts. The reviewers' tie witnesses (100000000000000.12, 140737488355327.12,
 * 2251799813685247.2) and both neighbouring doubles of each must reach the page's own view (one logged fallback,
 * legacy called); the ordinary control must be served, equal to the JS view. Cases come from
 * tests/spec/views/home/cases.json (their JS views are the recorded `expected`).
 *
 *   AAC_RUST_SERVICE_BINARY=<Nas1-built aac-http> bun tests/spec/home-number-admission.ts
 *   bun tests/spec/home-number-admission.ts --ts-only     (checks the stored envelopes only)
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('../..', import.meta.url));
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-home-numbers-'));
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { mode: 0o700 });
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, { CCS_HOME: home, CCS_DIR: ccs, AAC_NO_LOCAL_RUST_BUILD: '1' });
const { RustService } = await import('../../src/web-server/rust-service');
const { homeReadInput, isHomeView } = await import('../../src/web-server/views/home-context');
const tsOnly = process.argv.includes('--ts-only');
type Case = {
  name: string;
  dashboard: unknown;
  profiles: unknown;
  antigravity: unknown;
  ctx: any;
  presentation: unknown;
  expected: string | null;
};
const table: { cases: Case[] } = JSON.parse(
  fs.readFileSync(path.join(root, 'tests/spec/views/home/cases.json'), 'utf8')
);
const byName = new Map(table.cases.map((c) => [c.name, c]));
const witnesses = ['1e14', '2^47', '2^51'].flatMap((label) =>
  [0, 1, 2].map((i) => `decline/number-${label}#${i}`)
);
const control = 'edge/number-control';
const service = new RustService('on');
const req = {
  method: 'GET',
  originalUrl: '/api/views/home',
  sessionID: 'fixture-session',
  session: { authenticated: true },
  headers: {},
} as any;
try {
  for (const name of [...witnesses, control]) {
    const c = byName.get(name);
    assert.ok(c && c.expected, `${name}: the JS view is recorded`);
    // The stored presentation is the one the front door computes for these inputs today.
    const envelope: any = homeReadInput(c.ctx, c.dashboard, c.profiles, c.antigravity);
    assert.deepEqual(envelope.presentation, c.presentation, `${name}: envelope`);
  }
  if (!tsOnly) {
    await service.start(
      process.env.AAC_RUST_SERVICE_BINARY ?? path.join(root, 'dist/native/aac-http')
    );
    assert.equal(service.stats.fallbacks, 0, 'the service started');
    for (const name of witnesses) {
      const c = byName.get(name)!;
      const before = { ...service.stats };
      let legacy = 0;
      const view = await service.compose(
        req,
        '/api/views/home',
        () => homeReadInput(c.ctx, c.dashboard, c.profiles, c.antigravity),
        () => {
          legacy++;
          return null;
        },
        isHomeView
      );
      assert.equal(view, null, `${name}: no Rust view is served`);
      assert.equal(legacy, 1, `${name}: the page builds its own view`);
      assert.equal(service.stats.served, before.served, `${name}: served`);
      assert.equal(service.stats.fallbacks, before.fallbacks + 1, `${name}: one logged fallback`);
    }
    const c = byName.get(control)!;
    const served = service.stats.served;
    const view = await service.compose(
      req,
      '/api/views/home',
      () => homeReadInput(c.ctx, c.dashboard, c.profiles, c.antigravity),
      () => assert.fail('the control must not fall back'),
      isHomeView
    );
    assert.deepEqual(view, JSON.parse(c.expected!), 'control: the JS view');
    assert.equal(service.stats.served, served + 1, 'control: served by Rust');
  }
  console.log(
    `PASS Home number admission: ${witnesses.length} witnesses ${tsOnly ? 'have their envelopes' : 'fall back'}, control ${tsOnly ? 'recorded' : 'served'}`
  );
} finally {
  service.stop();
  fs.rmSync(home, { recursive: true, force: true });
}

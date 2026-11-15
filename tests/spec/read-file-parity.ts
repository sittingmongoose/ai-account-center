/** Actual guarded HTTP media/deliverable routes, fixture filesystem only, all three modes. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import express from 'express';
import type { Server } from 'node:http';
const repo = fileURLToPath(new URL('../..', import.meta.url));
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-http-files-'));
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { mode: 0o700 });
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: home,
  CCS_DIR: ccs,
  CCS_DASHBOARD_AUTH_ENABLED: 'true',
  AAC_NO_LOCAL_RUST_BUILD: '1',
});
const { RustService } = await import('../../src/web-server/rust-service');
const { createStatusRouter } = await import('../../src/web-server/routes/status-routes');
const { createShareRouter } = await import('../../src/web-server/routes/share-route');
const { apiAuthMiddleware } = await import('../../src/web-server/middleware/auth-middleware');
const { apiErrorHandler } = await import('../../src/web-server/routes/api-router');
const machine = path.join(home, 'machine');
const reports = path.join(machine, 'agent-reports');
const writer = path.join(reports, 'v1/writers/vm.fixture');
fs.mkdirSync(path.join(writer, 'media'), { recursive: true });
fs.mkdirSync(path.join(machine, 'share'), { recursive: true });
const png = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4]);
const video = Buffer.alloc(128, 5);
video.write('ftyp', 4);
video.write('isom', 8);
fs.writeFileSync(path.join(writer, 'media/image.png'), png);
fs.writeFileSync(path.join(writer, 'media/video.mp4'), video);
fs.writeFileSync(path.join(writer, 'media/not.txt'), 'fixture text');
fs.writeFileSync(path.join(writer, '.hidden.png'), png);
fs.symlinkSync(path.join(writer, '.hidden.png'), path.join(writer, 'media/inside.png'));
fs.writeFileSync(path.join(machine, 'outside.png'), png);
fs.symlinkSync(path.join(machine, 'outside.png'), path.join(writer, 'media/outside.png'));
fs.writeFileSync(path.join(machine, 'share/concept.html'), '<h1>Fixture concept</h1>');
fs.writeFileSync(path.join(machine, 'share/notes.md'), '# Fixture notes');
fs.writeFileSync(path.join(machine, 'share/video.mp4'), video);
fs.writeFileSync(
  path.join(machine, 'share/vector.svg'),
  '<svg xmlns="http://www.w3.org/2000/svg"/>'
);
fs.writeFileSync(path.join(machine, 'share/empty.txt'), '');
fs.writeFileSync(path.join(machine, 'share/.hidden.txt'), 'hidden fixture');
fs.symlinkSync(
  path.join(machine, 'share/.hidden.txt'),
  path.join(machine, 'share/inside-hidden.txt')
);
fs.writeFileSync(path.join(machine, 'share/large.txt'), Buffer.alloc(1024 * 1024 + 1, 65));
const altReports = path.join(home, 'aliased-reports');
fs.mkdirSync(path.join(altReports, 'v1'), { recursive: true });
fs.symlinkSync(path.join(reports, 'v1/writers'), path.join(altReports, 'v1/writers'));
const share = (file: string) => '/api/status/deliverable?path=' + encodeURIComponent(file);
const cases = [
  { name: 'media-image', url: '/api/status/files/vm.fixture/media/image.png' },
  { name: 'media-image-head', url: '/api/status/files/vm.fixture/media/image.png', method: 'HEAD' },
  { name: 'media-range', url: '/api/status/files/vm.fixture/media/video.mp4', range: 'bytes=3-10' },
  { name: 'media-suffix', url: '/api/status/files/vm.fixture/media/video.mp4', range: 'bytes=-8' },
  {
    name: 'media-unsatisfiable',
    url: '/api/status/files/vm.fixture/media/video.mp4',
    range: 'bytes=999-',
  },
  {
    name: 'media-ignore-multiple',
    url: '/api/status/files/vm.fixture/media/video.mp4',
    range: 'bytes=0-1,4-5',
  },
  { name: 'media-not-media', url: '/api/status/files/vm.fixture/media/not.txt' },
  { name: 'media-missing', url: '/api/status/files/vm.fixture/media/missing.png' },
  { name: 'media-internal-link', url: '/api/status/files/vm.fixture/media/inside.png' },
  { name: 'media-outside-link', url: '/api/status/files/vm.fixture/media/outside.png' },
  { name: 'media-writers-alias', url: '/api/status/files/vm.fixture/media/image.png', alias: true },
  { name: 'media-query-refusal', url: '/api/status/files/vm.fixture/media/image.png?unexpected=1' },
  {
    name: 'media-anonymous',
    url: '/api/status/files/vm.fixture/media/image.png',
    role: 'anonymous',
  },
  { name: 'media-device', url: '/api/status/files/vm.fixture/media/image.png', role: 'device' },
  { name: 'share-concept', url: share('share/concept.html') },
  { name: 'share-markdown', url: share('share/notes.md') },
  { name: 'share-head', url: share('share/notes.md'), method: 'HEAD' },
  { name: 'share-empty', url: share('share/empty.txt') },
  { name: 'share-svg', url: share('share/vector.svg') },
  { name: 'share-range', url: share('share/video.mp4'), range: 'bytes=2-7' },
  { name: 'share-invalid-range', url: share('share/video.mp4'), range: 'bytes=-0' },
  { name: 'share-missing', url: share('share/missing.txt') },
  { name: 'share-unsupported', url: share('share/test.exe') },
  { name: 'share-traversal', url: share('share/../outside.png') },
  { name: 'share-internal-dot-link', url: share('share/inside-hidden.txt') },
  { name: 'share-query-refusal', url: share('share/notes.md') + '&unexpected=1' },
  { name: 'share-wrong-origin', url: share('share/notes.md'), origin: 'https://invalid.example' },
  { name: 'share-write-refusal', url: share('share/notes.md'), method: 'POST' },
  { name: 'share-large-fallback', url: share('share/large.txt') },
];
const selected = [
  'content-type',
  'content-length',
  'cache-control',
  'content-range',
  'accept-ranges',
  'content-disposition',
  'content-security-policy',
  'x-content-type-options',
  'referrer-policy',
  'cross-origin-resource-policy',
  'allow',
  'etag',
];
let reference: unknown;
try {
  for (const mode of process.argv.includes('--ts-only') ? ['off'] : ['off', 'on', 'shadow']) {
    const service = new RustService(mode);
    await service.start(
      process.env.AAC_RUST_SERVICE_BINARY ?? path.join(repo, 'dist/native/aac-http')
    );
    if (mode !== 'off') assert.equal(service.stats.fallbacks, 0);
    let activeReports = reports;
    const app = express();
    app.use((req, res, next) => {
      res.locals.rustService = service;
      if (req.headers['x-session'])
        Object.assign(req, {
          sessionID: 'fixture-session',
          session: { authenticated: true, epoch: 0 },
        });
      if (req.headers['x-device'])
        Object.assign(req, { auth: { kind: 'device', deviceId: 'fixture-device' } });
      next();
    });
    app.use('/api', apiAuthMiddleware);
    app.use(
      '/api/status/deliverable',
      createShareRouter(() => machine)
    );
    app.use(
      '/api/status',
      createStatusRouter(
        () =>
          ({
            reader: {
              get root() {
                return activeReports;
              },
            },
            view: () => ({}),
            answers: {},
          }) as any
      )
    );
    app.use(apiErrorHandler);
    const server = await new Promise<Server>((resolve) => {
      const server = app.listen(0, '127.0.0.1', () => resolve(server));
    });
    try {
      const origin = `http://127.0.0.1:${(server.address() as any).port}`;
      const rows = [];
      for (const row of cases) {
        activeReports = (row as any).alias ? altReports : reports;
        const headers: Record<string, string> = {};
        if ((row as any).role !== 'anonymous') headers['x-session'] = 'yes';
        if ((row as any).role === 'device') headers['x-device'] = 'yes';
        if ((row as any).range) headers.range = (row as any).range;
        if ((row as any).origin) headers.origin = (row as any).origin;
        const response = await fetch(origin + row.url, {
          method: (row as any).method ?? 'GET',
          headers,
        });
        const bytes = Buffer.from(await response.arrayBuffer());
        const fields = Object.fromEntries(
          selected
            .filter((key) => response.headers.has(key))
            .map((key) => [key, response.headers.get(key)])
        );
        rows.push({
          name: row.name,
          status: response.status,
          headers: fields,
          length: bytes.length,
          body:
            bytes.length < 4096
              ? bytes.toString('base64')
              : createHash('sha256').update(bytes).digest('hex'),
        });
      }
      if (mode === 'off') {
        reference = rows;
        const file = path.join(repo, 'tests/spec/contract/m3-read-files.json');
        const bytes = JSON.stringify(rows, null, 2) + '\n';
        if (process.argv.includes('--regenerate')) fs.writeFileSync(file, bytes);
        else assert.equal(fs.readFileSync(file, 'utf8'), bytes, 'HTTP file golden bytes changed');
      } else
        assert.deepEqual(
          rows,
          reference,
          `${mode}: HTTP bytes/status/security headers must match TS`
        );
      if (mode === 'on')
        assert.ok(
          service.stats.served >= 20,
          'on mode must actually own successful and refused file reads'
        );
      if (mode === 'shadow') {
        for (let i = 0; i < 200 && service.stats.shadowed < 8; i++)
          await new Promise((r) => setTimeout(r, 5));
        assert.ok(service.stats.shadowed >= 8);
        assert.equal(service.stats.differences, 0);
      }
      console.log(
        `${cases.length} real HTTP file cases: ${mode}, Rust served ${service.stats.served}, shadow ${service.stats.shadowed}, fallbacks ${service.stats.fallbacks}`
      );
    } finally {
      await new Promise<void>((r) => server.close(() => r()));
      service.stop();
    }
  }
} finally {
  fs.rmSync(home, { recursive: true, force: true });
}

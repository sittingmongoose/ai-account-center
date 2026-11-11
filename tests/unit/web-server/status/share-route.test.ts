import { afterEach, beforeEach, expect, test, spyOn } from 'bun:test';
import express from 'express';
import fs from 'fs';
import os from 'os';
import path from 'path';
import type { Server } from 'http';
import type { AddressInfo } from 'net';
import { apiAuthMiddleware } from '../../../../src/web-server/middleware/auth-middleware';
import {
  createShareRouter,
  MAX_SHARE_BYTES,
  SHARE_CSP,
} from '../../../../src/web-server/routes/share-route';

let root: string;
let server: Server;
let url: string;
let oldHome: string | undefined;
let oldAuth: string | undefined;
beforeEach(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-share-test-'));
  oldHome = process.env.CCS_HOME;
  oldAuth = process.env.CCS_DASHBOARD_AUTH_ENABLED;
  process.env.CCS_HOME = path.join(root, 'ccs');
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
  fs.mkdirSync(path.join(root, 'share'));
  const app = express();
  app.use((req, _res, next) => {
    if (req.headers['x-session'])
      Object.assign(req, {
        session: { authenticated: true, epoch: req.headers['x-stale'] ? -1 : 0 },
      });
    if (req.headers['x-device']) Object.assign(req, { auth: { kind: 'device' } });
    next();
  });
  app.use('/api', apiAuthMiddleware);
  app.use(
    '/api/status/deliverable',
    createShareRouter(() => root)
  );
  server = await new Promise<Server>((resolve) => {
    const s = app.listen(0, '127.0.0.1', () => resolve(s));
  });
  url = `http://127.0.0.1:${(server.address() as AddressInfo).port}/api/status/deliverable`;
});
afterEach(async () => {
  await new Promise<void>((resolve) => server.close(() => resolve()));
  fs.rmSync(root, { recursive: true, force: true });
  if (oldHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = oldHome;
  if (oldAuth === undefined) delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
  else process.env.CCS_DASHBOARD_AUTH_ENABLED = oldAuth;
});
const session = { 'x-session': 'yes' };
function get(file: string, headers = session, method = 'GET') {
  return fetch(`${url}?path=${encodeURIComponent(file)}`, { headers, method });
}
test('requires a browser session, including when sign-in is disabled; refuses wrong origin and writes', async () => {
  expect((await get('share/a.txt', {})).status).toBe(401);
  expect((await get('share/a.txt', { ...session, 'x-stale': 'yes' })).status).toBe(401);
  expect((await get('share/a.txt', { ...session, 'x-device': 'yes' })).status).toBe(403);
  expect((await get('share/a.txt', { ...session, origin: 'https://evil.example' })).status).toBe(
    403
  );
  expect((await get('share/a.txt', { ...session, origin: 'null' })).status).toBe(403);
  for (const method of ['POST', 'PUT', 'DELETE', 'OPTIONS'])
    expect((await get('share/a.txt', session, method)).status).toBe(405);
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
  expect((await get('share/a.txt', {})).status).toBe(403);
});
test('rejects traversal, absolute paths, dotfiles, encoded tricks and query ambiguity', async () => {
  for (const file of [
    '/share/a.txt',
    'C:/a.txt',
    '//server/a.txt',
    'share/../a.txt',
    'share/./a.txt',
    'share/.secret',
    'share/.hidden/a.txt',
    'share/a\0.txt',
    'share/a\\b.txt',
    'share/%2e%2e/a.txt',
    'share/%252e%252e/a.txt',
    'share//a.txt',
    'agent-reports/v1/writers/one/status.json',
    'agent-reports/v1/writers/one/cards/a.html',
  ])
    expect((await get(file)).status).toBe(400);
  expect(
    (await fetch(`${url}?path=share/a.txt&path=share/b.txt`, { headers: session })).status
  ).toBe(400);
  expect((await fetch(`${url}?path=share/a.txt&extra=1`, { headers: session })).status).toBe(400);
});
test('confines symlinks, canonical dot paths, directories and missing files', async () => {
  fs.writeFileSync(path.join(root, 'secret.txt'), 'secret');
  fs.writeFileSync(path.join(root, 'share/.hidden.txt'), 'secret');
  fs.symlinkSync('../secret.txt', path.join(root, 'share/escape.txt'));
  fs.symlinkSync('.hidden.txt', path.join(root, 'share/hidden.txt'));
  fs.mkdirSync(path.join(root, 'share/folder.txt'));
  for (const name of ['escape.txt', 'hidden.txt', 'folder.txt', 'missing.txt'])
    expect((await get(`share/${name}`)).status).toBe(404);
  const media = path.join(root, 'agent-reports/v1/writers/one/media');
  fs.mkdirSync(media, { recursive: true });
  fs.writeFileSync(path.join(media, 'ok.txt'), 'ok');
  expect(await (await get('agent-reports/v1/writers/one/media/ok.txt')).text()).toBe('ok');
  fs.symlinkSync(path.join(root, 'share'), path.join(media, 'escape'));
  fs.writeFileSync(path.join(root, 'share/out.txt'), 'outside media');
  expect((await get('agent-reports/v1/writers/one/media/escape/out.txt')).status).toBe(404);
});
test('refuses an allowlisted root redirected outside the machine share', async () => {
  fs.mkdirSync(path.join(root, 'outside'));
  fs.writeFileSync(path.join(root, 'outside/a.txt'), 'secret');
  fs.rmdirSync(path.join(root, 'share'));
  fs.symlinkSync('outside', path.join(root, 'share'));
  expect((await get('share/a.txt')).status).toBe(404);
});
test('descriptor check refuses a parent swapped after realpath, before open', async () => {
  const folder = path.join(root, 'share/parent');
  const outside = path.join(root, 'outside');
  fs.mkdirSync(folder);
  fs.mkdirSync(outside);
  fs.writeFileSync(path.join(folder, 'a.txt'), 'public');
  fs.writeFileSync(path.join(outside, 'a.txt'), 'secret');
  const realpath = fs.promises.realpath.bind(fs.promises);
  const spy = spyOn(fs.promises, 'realpath').mockImplementation(async (file, options) => {
    const result = await realpath(file, options as never);
    if (String(file) === path.join(folder, 'a.txt')) {
      fs.renameSync(folder, `${folder}-saved`);
      fs.symlinkSync(outside, folder);
    }
    return result;
  });
  try {
    expect((await get('share/parent/a.txt')).status).toBe(404);
  } finally {
    spy.mockRestore();
  }
});
test('types allowlisted files, keeps markdown inert and sandboxes hostile concepts', async () => {
  const types: Record<string, string> = {
    html: 'text/html',
    md: 'text/plain',
    txt: 'text/plain',
    json: 'application/json',
    png: 'image/png',
    jpg: 'image/jpeg',
    jpeg: 'image/jpeg',
    gif: 'image/gif',
    webp: 'image/webp',
    svg: 'image/svg+xml',
    mp4: 'video/mp4',
    webm: 'video/webm',
    pdf: 'application/pdf',
  };
  const hostile =
    '<h1>Concept</h1><script>document.body.dataset.cookie=document.cookie;fetch("/api/status")</script>';
  for (const [ext, type] of Object.entries(types)) {
    fs.writeFileSync(path.join(root, `share/a.${ext}`), hostile);
    const res = await get(`share/a.${ext}`);
    expect(res.status).toBe(200);
    expect(res.headers.get('content-type')).toStartWith(type);
    expect(res.headers.get('content-security-policy')).toBe(SHARE_CSP);
    expect(res.headers.get('x-content-type-options')).toBe('nosniff');
    expect(res.headers.get('cache-control')).toBe('no-store');
    if (ext === 'svg') expect(res.headers.get('content-disposition')).toBe('attachment');
    expect(await res.text()).toBe(hostile);
    const head = await get(`share/a.${ext}`, session, 'HEAD');
    expect(head.status).toBe(200);
    expect(await head.text()).toBe('');
  }
  expect((await get('share/a.js')).status).toBe(415);
});
test('bounds video byte ranges, empty files and oversized sparse files', async () => {
  fs.writeFileSync(path.join(root, 'share/a.mp4'), '0123456789');
  for (const [range, bytes] of [
    ['bytes=2-4', '234'],
    ['bytes=-3', '789'],
    ['bytes=8-', '89'],
  ]) {
    const res = await get('share/a.mp4', { ...session, range });
    expect(res.status).toBe(206);
    expect(await res.text()).toBe(bytes);
  }
  expect((await get('share/a.mp4', { ...session, range: 'bytes=10-' })).status).toBe(416);
  fs.writeFileSync(path.join(root, 'share/empty.txt'), '');
  expect(await (await get('share/empty.txt')).text()).toBe('');
  fs.truncateSync(path.join(root, 'share/a.mp4'), MAX_SHARE_BYTES + 1);
  expect((await get('share/a.mp4')).status).toBe(413);
});

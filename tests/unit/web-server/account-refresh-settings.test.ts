import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import express from 'express';
import type { Server } from 'http';
import {
  readAccountRefreshSettings,
  writeAccountRefreshSettings,
} from '../../../src/web-server/services/account-refresh-settings';
import { createAccountRefreshSettingsRouter } from '../../../src/web-server/routes/account-refresh-settings-routes';

const temporaryDirs: string[] = [];
let server: Server | undefined;
afterEach(async () => {
  if (server) await new Promise<void>((resolve) => server!.close(() => resolve()));
  server = undefined;
  for (const directory of temporaryDirs.splice(0)) fs.rmSync(directory, { recursive: true });
});

function directory(): string {
  const result = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-refresh-settings-'));
  temporaryDirs.push(result);
  return result;
}

describe('account usage refresh settings', () => {
  it('persists a private setting atomically and survives a fresh read', () => {
    const dir = directory();
    expect(readAccountRefreshSettings(dir)).toEqual({ refreshIntervalSeconds: 60 });
    writeAccountRefreshSettings({ refreshIntervalSeconds: 300 }, dir);
    expect(readAccountRefreshSettings(dir)).toEqual({ refreshIntervalSeconds: 300 });
    expect(fs.statSync(path.join(dir, 'account-refresh-settings.json')).mode & 0o777).toBe(0o600);
    expect(fs.readdirSync(dir)).toEqual(['account-refresh-settings.json']);
  });

  it('defaults on malformed or oversized state and never reads a symlink target', () => {
    const dir = directory();
    const file = path.join(dir, 'account-refresh-settings.json');
    fs.writeFileSync(file, '{"refreshIntervalSeconds":5}');
    expect(readAccountRefreshSettings(dir).refreshIntervalSeconds).toBe(60);
    fs.writeFileSync(file, ' '.repeat(4097));
    expect(readAccountRefreshSettings(dir).refreshIntervalSeconds).toBe(60);
    fs.unlinkSync(file);
    const target = path.join(dir, 'external.json');
    fs.writeFileSync(target, '{"refreshIntervalSeconds":30}');
    fs.symlinkSync(target, file);
    expect(readAccountRefreshSettings(dir).refreshIntervalSeconds).toBe(60);
    writeAccountRefreshSettings({ refreshIntervalSeconds: 120 }, dir);
    expect(JSON.parse(fs.readFileSync(target, 'utf8')).refreshIntervalSeconds).toBe(30);
    expect(readAccountRefreshSettings(dir).refreshIntervalSeconds).toBe(120);
  });

  it('requires a session and same origin; rejects unsafe bounds and returns only persisted values', async () => {
    let saved = { refreshIntervalSeconds: 60 };
    let writes = 0;
    const app = express();
    app.use(express.json());
    app.use((req, _res, next) => {
      if (req.headers['x-test-session'] === 'true')
        Object.assign(req, { session: { authenticated: true } });
      next();
    });
    app.use(
      '/api/accounts',
      createAccountRefreshSettingsRouter({
        read: () => saved,
        write: (value) => {
          writes++;
          saved = value;
          return saved;
        },
      })
    );
    app.get('/api/accounts/analytics', (req, res) => res.json({ range: req.query.range }));
    server = await new Promise<Server>((resolve) => {
      const listener = app.listen(0, '127.0.0.1', () => resolve(listener));
    });
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('Missing port');
    const base = `http://127.0.0.1:${address.port}`;
    const url = `${base}/api/accounts/settings`;
    const analytics = await fetch(`${base}/api/accounts/analytics?range=7d`, {
      headers: { 'x-test-session': 'true', origin: base },
    });
    expect(analytics.status).toBe(200);
    expect(await analytics.json()).toEqual({ range: '7d' });
    const request = (body: unknown, origin = base, authenticated = true) =>
      fetch(url, {
        method: 'PUT',
        headers: {
          'content-type': 'application/json',
          origin,
          ...(authenticated ? { 'x-test-session': 'true' } : {}),
        },
        body: JSON.stringify(body),
      });
    expect((await fetch(url)).status).toBe(401);
    expect((await request({ refreshIntervalSeconds: 120 }, base, false)).status).toBe(401);
    expect(
      (await request({ refreshIntervalSeconds: 120 }, 'https://untrusted.example')).status
    ).toBe(403);
    for (const body of [
      null,
      [],
      {},
      { refreshIntervalSeconds: 29 },
      { refreshIntervalSeconds: 3601 },
      { refreshIntervalSeconds: 60.5 },
      { refreshIntervalSeconds: '60' },
      { refreshIntervalSeconds: 60, command: 'untrusted' },
    ]) {
      expect((await request(body)).status).toBe(400);
    }
    expect(writes).toBe(0);
    const response = await request({ refreshIntervalSeconds: 120 });
    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('no-store');
    expect(await response.json()).toEqual({ refreshIntervalSeconds: 120 });
    const read = await fetch(url, { headers: { 'x-test-session': 'true' } });
    expect(await read.json()).toEqual({ refreshIntervalSeconds: 120 });
    expect(writes).toBe(1);
  });
});

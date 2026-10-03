import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import express from 'express';
import type { Server } from 'http';
import {
  defaultDashboardPreferences,
  isDashboardPreferences,
  readDashboardPreferences,
  writeDashboardPreferences,
} from '../../../src/web-server/services/dashboard-preferences';
import { createAccountPreferencesRouter } from '../../../src/web-server/routes/account-preferences-routes';

const temporaryDirs: string[] = [];
const originalCcsHome = process.env.CCS_HOME;
let server: Server | undefined;
beforeEach(() => {
  process.env.CCS_HOME = directory();
});
afterEach(async () => {
  if (server) await new Promise<void>((resolve) => server!.close(() => resolve()));
  server = undefined;
  if (originalCcsHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = originalCcsHome;
  for (const dir of temporaryDirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

function directory(): string {
  const result = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-dashboard-preferences-'));
  temporaryDirs.push(result);
  return result;
}

describe('dashboard preferences', () => {
  it('defaults to America/New_York with automatic snapshot cleanup and no extra sources', () => {
    expect(defaultDashboardPreferences()).toEqual({
      timeZone: 'America/New_York',
      snapshotCleanup: { auto: true },
      usageLogSources: [],
    });
    expect(readDashboardPreferences(directory())).toEqual(defaultDashboardPreferences());
  });

  it('persists atomically and survives a fresh read', () => {
    const dir = directory();
    const saved = writeDashboardPreferences(
      {
        timeZone: 'Europe/Paris',
        snapshotCleanup: { auto: false },
        usageLogSources: [
          { id: 'omp-mac', tool: 'omp', host: 'mac', path: '/Users/u/.omp/sessions' },
        ],
      },
      dir
    );
    expect(saved.timeZone).toBe('Europe/Paris');
    expect(readDashboardPreferences(dir)).toEqual(saved);
    expect(fs.statSync(path.join(dir, 'dashboard-preferences.json')).mode & 0o777).toBe(0o600);
    expect(fs.readdirSync(dir)).toEqual(['dashboard-preferences.json']);
  });

  it('refuses zones that are not IANA names', () => {
    const base = defaultDashboardPreferences();
    expect(isDashboardPreferences({ ...base, timeZone: 'Mars/Olympus' })).toBe(false);
    expect(isDashboardPreferences({ ...base, timeZone: 'UTC+2' })).toBe(false);
    expect(isDashboardPreferences({ ...base, timeZone: '../x' })).toBe(false);
    expect(isDashboardPreferences({ ...base, timeZone: 5 })).toBe(false);
    expect(isDashboardPreferences({ ...base, timeZone: 'UTC' })).toBe(true);
    expect(isDashboardPreferences({ ...base, timeZone: 'Asia/Tokyo' })).toBe(true);
    expect(isDashboardPreferences({ ...base, snapshotCleanup: { auto: 'yes' } })).toBe(false);
  });

  it('refuses usage-log sources with bad ids, tools, hosts or paths', () => {
    const base = defaultDashboardPreferences();
    const withSource = (source: unknown) => ({ ...base, usageLogSources: [source] });
    const good = { id: 'zcode-ubuntu', tool: 'zcode', host: 'ubuntu', path: '/home/u/.zcode/db' };
    expect(isDashboardPreferences(withSource(good))).toBe(true);
    expect(isDashboardPreferences(withSource({ ...good, id: 'Bad id!' }))).toBe(false);
    expect(isDashboardPreferences(withSource({ ...good, tool: 'cursor' }))).toBe(false);
    expect(isDashboardPreferences(withSource({ ...good, host: 'mars' }))).toBe(false);
    expect(isDashboardPreferences(withSource({ ...good, path: 'relative/path' }))).toBe(false);
    expect(isDashboardPreferences(withSource({ ...good, path: '/x/../y' }))).toBe(false);
    expect(isDashboardPreferences(withSource({ ...good, path: '/x\0y' }))).toBe(false);
    // Windows paths follow Windows syntax, but only on the Windows host.
    expect(
      isDashboardPreferences(withSource({ ...good, host: 'windows', path: 'C:\\logs\\omp' }))
    ).toBe(true);
    expect(
      isDashboardPreferences(withSource({ ...good, host: 'windows', path: 'C:/logs/omp' }))
    ).toBe(true);
    expect(isDashboardPreferences(withSource({ ...good, path: 'C:\\logs\\omp' }))).toBe(false);
    expect(
      isDashboardPreferences(withSource({ ...good, host: 'windows', path: 'C:\\x\\..\\y' }))
    ).toBe(false);
    expect(
      isDashboardPreferences({ ...base, usageLogSources: [good, { ...good, path: '/other' }] })
    ).toBe(false);
    expect(isDashboardPreferences({ ...base, usageLogSources: new Array(65).fill(good) })).toBe(
      false
    );
  });

  it('requires a field mapping on generic JSONL sources, with timestamp and model', () => {
    const base = defaultDashboardPreferences();
    const jsonl = { id: 'harness', tool: 'jsonl', host: 'ubuntu', path: '/var/log/harness' };
    expect(isDashboardPreferences({ ...base, usageLogSources: [jsonl] })).toBe(false);
    expect(
      isDashboardPreferences({
        ...base,
        usageLogSources: [{ ...jsonl, fieldMapping: { timestamp: 'ts', model: 'model' } }],
      })
    ).toBe(true);
    expect(
      isDashboardPreferences({
        ...base,
        usageLogSources: [{ ...jsonl, fieldMapping: { timestamp: 'ts' } }],
      })
    ).toBe(false);
    expect(
      isDashboardPreferences({
        ...base,
        usageLogSources: [{ ...jsonl, fieldMapping: { timestamp: 'ts', model: 'm', bogus: 'x' } }],
      })
    ).toBe(false);
  });

  it('defaults on corrupt state and never reads a symlink target', () => {
    const dir = directory();
    const file = path.join(dir, 'dashboard-preferences.json');
    fs.writeFileSync(file, '{"timeZone":"Mars/Olympus"}');
    expect(readDashboardPreferences(dir)).toEqual(defaultDashboardPreferences());
    fs.writeFileSync(file, ' '.repeat(65537));
    expect(readDashboardPreferences(dir)).toEqual(defaultDashboardPreferences());
    fs.unlinkSync(file);
    const target = path.join(dir, 'target.json');
    fs.writeFileSync(target, JSON.stringify({ ...defaultDashboardPreferences(), timeZone: 'UTC' }));
    fs.symlinkSync(target, file);
    expect(readDashboardPreferences(dir)).toEqual(defaultDashboardPreferences());
  });

  it('serves GET and PUT to a signed-in browser session only', async () => {
    const dir = directory();
    const app = express();
    app.use(express.json());
    app.use((req, _res, next) => {
      if (req.headers['x-test-session'] === 'true')
        Object.assign(req, { session: { authenticated: true } });
      next();
    });
    app.use(
      '/api/accounts',
      createAccountPreferencesRouter({
        read: () => readDashboardPreferences(dir),
        write: (value) => writeDashboardPreferences(value, dir),
      })
    );
    server = await new Promise<Server>((resolve) => {
      const listener = app.listen(0, '127.0.0.1', () => resolve(listener));
    });
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('Missing port');
    const base = `http://127.0.0.1:${address.port}`;
    const url = `${base}/api/accounts/preferences`;
    expect((await fetch(url)).status).toBe(401);
    const read = await fetch(url, { headers: { 'x-test-session': 'true' } });
    expect(read.status).toBe(200);
    expect(await read.json()).toEqual(defaultDashboardPreferences());
    const put = (body: unknown, headers: Record<string, string> = {}) =>
      fetch(url, {
        method: 'PUT',
        headers: { 'content-type': 'application/json', origin: base, ...headers },
        body: JSON.stringify(body),
      });
    expect((await put(defaultDashboardPreferences())).status).toBe(401);
    expect(
      (
        await put(defaultDashboardPreferences(), {
          origin: 'http://evil.test',
          'x-test-session': 'true',
        })
      ).status
    ).toBe(403);
    expect(
      (
        await put(
          { ...defaultDashboardPreferences(), timeZone: 'Mars/Olympus' },
          { 'x-test-session': 'true' }
        )
      ).status
    ).toBe(400);
    const saved = await put(
      { ...defaultDashboardPreferences(), timeZone: 'Europe/Paris' },
      { 'x-test-session': 'true' }
    );
    expect(saved.status).toBe(200);
    expect(((await saved.json()) as { timeZone: string }).timeZone).toBe('Europe/Paris');
    expect(readDashboardPreferences(dir).timeZone).toBe('Europe/Paris');
  });
});

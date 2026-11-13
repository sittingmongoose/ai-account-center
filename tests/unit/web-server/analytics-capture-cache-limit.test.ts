/**
 * Stage 1 review fixes (2026-11-13):
 * - M2: turning cwd capture off clears the working folders already saved, not only new ones, and is a
 *   real setting (an authenticated route), not a file to edit by hand;
 * - M3: one size limit for saving and loading a computer's cache; a cache over it is trimmed to fit,
 *   and one the load discards says why.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import * as fs from 'fs';
import type { Server } from 'http';
import * as os from 'os';
import * as path from 'path';
import { createAccountAnalyticsRouter } from '../../../src/web-server/routes/account-analytics-routes';
import {
  readAnalyticsCaptureSettings,
  writeAnalyticsCaptureSettings,
} from '../../../src/web-server/services/analytics-capture-settings';
import {
  MAX_REMOTE_CACHE_BYTES,
  fitRemoteCache,
  remoteCacheBytes,
} from '../../../src/web-server/services/analytics-remote-cache-limit';
import {
  clearAnalyticsRemoteCwds,
  loadAnalyticsRemoteCachedSources,
  loadAnalyticsRemoteSources,
} from '../../../src/web-server/services/analytics-remote-sources';
import {
  AnalyticsRemoteTransportError,
  parseAnalyticsRemoteResponse,
  type AnalyticsRemoteKind,
  type AnalyticsRemoteRow,
  type AnalyticsRemoteSessionRow,
} from '../../../src/web-server/services/analytics-remote-transport';

const MIN_DATE = Date.parse('2026-09-01T00:00:00Z');
const NOW = Date.parse('2026-10-02T00:00:00Z');
const KINDS: AnalyticsRemoteKind[] = [
  'claude',
  'codex',
  'omp',
  'muse',
  'zcode',
  'antigravity',
  'cursor',
  'devin',
];
const FILE_1 = 'a1'.repeat(32);
const SESSION = 'c3'.repeat(8);
const FOLDER = '~/code/private-client-project';

let dir: string;
let cache: string;
let savedCcs: string | undefined;
let server: Server | undefined;
beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-capture-limit-'));
  cache = path.join(dir, 'cache');
  savedCcs = process.env.CCS_HOME;
  process.env.CCS_HOME = path.join(dir, 'ccs');
});
afterEach(async () => {
  if (server) await new Promise<void>((resolve) => server!.close(() => resolve()));
  server = undefined;
  if (savedCcs === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = savedCcs;
  fs.rmSync(dir, { recursive: true, force: true });
});

/** A complete answer: one Codex file with one session, its evidence naming a working folder. */
function answer(withFolder = true) {
  return parseAnalyticsRemoteResponse(
    JSON.stringify({
      version: 2,
      truncated: false,
      kinds: Object.fromEntries(
        KINDS.map((kind) => [
          kind,
          kind === 'codex'
            ? {
                state: 'ok',
                fingerprints: {
                  [FILE_1]: { size: 10, mtimeMs: 20, head: 'a'.repeat(64), tail: 'a'.repeat(64) },
                },
              }
            : { state: 'ok', fingerprints: {} },
        ])
      ),
      rows: [
        {
          k: 'codex',
          f: FILE_1,
          m: 'm',
          h: '2026-10-01 15:00',
          i: 1,
          o: 2,
          cr: 0,
          cw: 0,
          c: 0,
          n: 3,
        },
      ],
      srows: [
        {
          k: 'codex',
          f: FILE_1,
          s: SESSION,
          m: 'm',
          a: Date.parse('2026-10-01T15:00:00Z'),
          z: Date.parse('2026-10-01T15:30:00Z'),
          i: 1,
          o: 2,
          cr: 0,
          cw: 0,
          c: 0,
          n: 3,
        },
      ],
      sessions: {
        [SESSION]: {
          hash: `${SESSION}${'d'.repeat(48)}`,
          cwd: withFolder
            ? {
                key: 'e'.repeat(16),
                path: FOLDER,
                identitySource: 'inferred_cwd',
                confidence: 'inferred',
              }
            : null,
        },
      },
    })
  );
}

const hosts = async () => ({ mac: null, windows: null, nas1: 'nas1-agent', alexis: null });
const cacheText = () =>
  fs.readFileSync(path.join(cache, 'analytics-remote-v1', 'nas1.json'), 'utf8');
const scan = (withFolder = true) =>
  loadAnalyticsRemoteSources(MIN_DATE, {
    cacheDir: cache,
    hosts,
    now: () => NOW,
    force: true,
    runHelper: async () => answer(withFolder),
  });

async function listen(): Promise<string> {
  const app = express();
  app.use(express.json());
  app.use((req, _res, next) => {
    if (req.headers['x-test-session'] === 'true')
      Object.assign(req, { session: { authenticated: true } });
    next();
  });
  app.use(
    '/api/accounts',
    createAccountAnalyticsRouter({ clearCwds: () => clearAnalyticsRemoteCwds(cache) })
  );
  server = await new Promise<Server>((resolve) => {
    const listener = app.listen(0, '127.0.0.1', () => resolve(listener));
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Missing port');
  return `http://127.0.0.1:${address.port}`;
}

describe('turning cwd capture off', () => {
  it('clears the folders already saved, keeps the usage, and saves none afterwards', async () => {
    await scan();
    expect(cacheText()).toContain(FOLDER);
    const base = await listen();
    const url = `${base}/api/accounts/analytics/capture`;
    const put = (body: unknown, origin = base, session = true) =>
      fetch(url, {
        method: 'PUT',
        headers: {
          'content-type': 'application/json',
          origin,
          ...(session ? { 'x-test-session': 'true' } : {}),
        },
        body: JSON.stringify(body),
      });
    // The guards: a session, the dashboard origin, and exactly {cwd: boolean}.
    expect((await fetch(url)).status).toBe(401);
    expect((await put({ cwd: false }, base, false)).status).toBe(401);
    expect((await put({ cwd: false }, 'https://untrusted.example')).status).toBe(403);
    for (const body of [null, [], {}, { cwd: 'false' }, { cwd: false, extra: 1 }])
      expect((await put(body)).status).toBe(400);
    expect(readAnalyticsCaptureSettings().cwd).toBe(true);
    expect(cacheText()).toContain(FOLDER);

    const off = await put({ cwd: false });
    expect(off.status).toBe(200);
    expect(off.headers.get('cache-control')).toBe('no-store');
    expect(await off.json()).toEqual({ cwd: false, cleared: { folders: 1, caches: 0 } });
    expect(readAnalyticsCaptureSettings().cwd).toBe(false);
    const cleared = JSON.parse(cacheText());
    expect(cacheText()).not.toContain(FOLDER);
    expect(cleared.sessions[SESSION]).toEqual({ hash: `${SESSION}${'d'.repeat(48)}`, cwd: null });
    expect(cleared.rows).toHaveLength(1);
    expect(cleared.srows).toHaveLength(1);

    // An answer that still names a folder (a scan started before the change) saves none.
    await scan(true);
    expect(cacheText()).not.toContain(FOLDER);
    expect(JSON.parse(cacheText()).sessions[SESSION].cwd).toBeNull();
    const read = await fetch(url, { headers: { 'x-test-session': 'true' } });
    expect(await read.json()).toEqual({ cwd: false });

    // On again: new answers keep their folder; nothing is cleared.
    const on = await put({ cwd: true });
    expect(await on.json()).toEqual({ cwd: true });
    await scan(true);
    expect(cacheText()).toContain(FOLDER);
  });

  it('removes a cache it cannot read, since it may hold folders too', () => {
    fs.mkdirSync(path.join(cache, 'analytics-remote-v1'), { recursive: true });
    fs.writeFileSync(path.join(cache, 'analytics-remote-v1', 'mac.json'), `{"sessions":${FOLDER}`);
    writeAnalyticsCaptureSettings({ cwd: false });
    expect(clearAnalyticsRemoteCwds(cache)).toEqual({ folders: 0, caches: 1 });
    expect(fs.existsSync(path.join(cache, 'analytics-remote-v1', 'mac.json'))).toBe(false);
  });
});

describe('one cache size limit for save and load', () => {
  const codexCell = (states: Array<{ host: string; tool: string }>) =>
    states.find((entry) => entry.host === 'nas1' && entry.tool === 'codex') as
      { state: string; detail: string | null; rowCount: number } | undefined;

  it('loads a cache of exactly the limit, and names a cache one byte over it instead of dropping it silently', async () => {
    await scan();
    const file = path.join(cache, 'analytics-remote-v1', 'nas1.json');
    const body = cacheText();
    // JSON allows trailing white space: the same cache, padded to the limit.
    fs.writeFileSync(file, body + ' '.repeat(MAX_REMOTE_CACHE_BYTES - Buffer.byteLength(body)));
    expect(fs.statSync(file).size).toBe(MAX_REMOTE_CACHE_BYTES);
    const at = codexCell(loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache }).states);
    expect(at).toMatchObject({ state: 'cached', rowCount: 3 });
    expect(at?.detail).not.toContain('cache_oversize');

    fs.appendFileSync(file, ' ');
    const over = codexCell(loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache }).states);
    expect(over?.state).toBe('unavailable');
    expect(over?.detail).toContain('discarded');
    expect(over?.detail).toContain('cache_oversize');

    // The next scan reads again from the start, says so, and saves under the limit.
    const { states } = await scan();
    expect(codexCell(states)?.detail).toContain('cache_oversize');
    expect(fs.statSync(file).size).toBeLessThan(MAX_REMOTE_CACHE_BYTES);
    // Read completely: the discard has been named and is not carried on.
    expect(JSON.parse(cacheText()).discarded).toBeUndefined();
  });

  it('never replaces a cache it could not load with a blank while scans fail', async () => {
    const file = path.join(cache, 'analytics-remote-v1', 'nas1.json');
    fs.mkdirSync(path.dirname(file), { recursive: true });
    // A newer format written before a rollback: this version cannot read it, nor may it erase it.
    const newer = JSON.stringify({ version: 99, fingerprints: {}, rows: [], srows: [] });
    fs.writeFileSync(file, newer);
    const { states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: cache,
      hosts,
      now: () => NOW,
      force: true,
      runHelper: async () => {
        throw new AnalyticsRemoteTransportError(false, 'ssh_unreachable', 255);
      },
    });
    expect(fs.readFileSync(file, 'utf8')).toBe(newer);
    expect(codexCell(states)?.detail).toContain('cache_format');
  });

  it('names an unreadable cache', () => {
    fs.mkdirSync(path.join(cache, 'analytics-remote-v1'), { recursive: true });
    fs.writeFileSync(path.join(cache, 'analytics-remote-v1', 'nas1.json'), '{"version": 6,');
    const cell = codexCell(loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache }).states);
    expect(cell?.detail).toContain('cache_unreadable');
  });

  it('trims a cache over the limit to fit: evidence first, then the oldest hours, every print kept', () => {
    const row = (hour: number): AnalyticsRemoteRow => ({
      k: 'codex',
      f: FILE_1,
      m: 'm',
      h: `2026-10-01 ${String(hour).padStart(2, '0')}:00`,
      i: 1,
      o: 1,
      cr: 0,
      cw: 0,
      c: 0,
      n: 1,
    });
    const srow = (hour: number): AnalyticsRemoteSessionRow => ({
      k: 'codex',
      f: FILE_1,
      s: SESSION,
      m: 'm',
      a: Date.parse(`2026-10-01T${String(hour).padStart(2, '0')}:00:00Z`),
      z: Date.parse(`2026-10-01T${String(hour).padStart(2, '0')}:10:00Z`),
      i: 1,
      o: 1,
      cr: 0,
      cw: 0,
      c: 0,
      n: 1,
    });
    const value = {
      fingerprints: { codex: { [FILE_1]: { size: 1, mtimeMs: 1 } } },
      rows: Array.from({ length: 200 }, (_, index) => row(index % 20)),
      srows: Array.from({ length: 20 }, (_, index) => srow(index)),
      sessions: { [SESSION]: { hash: `${SESSION}${'d'.repeat(48)}`, cwd: null } },
    };
    const whole = remoteCacheBytes(value);
    const limit = Math.floor(whole * 0.6);
    const fitted = fitRemoteCache(value, '2026-10-02T00:00:00.000Z', limit);
    expect(fitted).not.toBeNull();
    expect(remoteCacheBytes(fitted)).toBeLessThanOrEqual(limit);
    expect(fitted?.sessions).toEqual({});
    expect(fitted?.fingerprints).toEqual(value.fingerprints);
    const kept = new Set(fitted?.rows.map((entry) => entry.h));
    const first = [...kept].sort()[0];
    // Only the oldest hours go, and the cut is named at the first hour kept.
    expect(kept.has('2026-10-01 19:00')).toBe(true);
    expect(kept.has('2026-10-01 00:00')).toBe(false);
    expect(fitted?.trimmed).toEqual({
      cause: 'cache_oversize',
      before: `${first.replace(' ', 'T')}:00.000Z`,
      at: '2026-10-02T00:00:00.000Z',
    });
    expect(fitted?.srows.every((entry) => entry.z >= Date.parse(fitted!.trimmed!.before))).toBe(
      true
    );
    // Under the limit already: only the evidence would go, nothing else.
    expect(fitRemoteCache(value, 'x', whole * 2)?.rows).toHaveLength(200);
    // Prints alone over the limit: nothing fits, the caller keeps its last save.
    expect(fitRemoteCache(value, 'x', 10)).toBeNull();
  });

  it('names a trim on the cells while the cut is in range, also after a restart', async () => {
    await scan();
    const file = path.join(cache, 'analytics-remote-v1', 'nas1.json');
    const saved = JSON.parse(cacheText());
    saved.trimmed = {
      cause: 'cache_oversize',
      before: '2026-09-20T00:00:00.000Z',
      at: '2026-10-01T00:00:00.000Z',
    };
    fs.writeFileSync(file, JSON.stringify(saved));
    const cell = codexCell(loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache }).states);
    expect(cell?.detail).toContain('usage before 2026-09-20T00:00:00.000Z is not kept');
    // Carried by the next save while in range; gone once the window starts after it.
    await scan();
    expect(JSON.parse(cacheText()).trimmed?.before).toBe('2026-09-20T00:00:00.000Z');
    const later = codexCell(
      loadAnalyticsRemoteCachedSources(Date.parse('2026-09-25T00:00:00Z'), { cacheDir: cache })
        .states
    );
    expect(later?.detail ?? '').not.toContain('is not kept');
  });
});

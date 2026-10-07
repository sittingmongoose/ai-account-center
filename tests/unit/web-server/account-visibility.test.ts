import { afterEach, describe, expect, it } from 'bun:test';
import express from 'express';
import fs from 'fs';
import http, { type Server } from 'http';
import type { AddressInfo } from 'net';
import os from 'os';
import path from 'path';
import { WebSocket, WebSocketServer } from 'ws';
import {
  ACCOUNT_VISIBILITY_FILE,
  parseVisibilityBody,
  readAccountVisibility,
  updateAccountVisibility,
  writeAccountVisibility,
} from '../../../src/web-server/services/account-visibility';
import { createAccountVisibilityRouter } from '../../../src/web-server/routes/account-visibility-routes';
import {
  attachDashboardEventServer,
  broadcastDashboardEvent,
} from '../../../src/web-server/dashboard-events';

const temporaryDirs: string[] = [];
const closers: Array<() => Promise<void>> = [];
afterEach(async () => {
  for (const close of closers.splice(0).reverse()) await close();
  for (const dir of temporaryDirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

function ccsDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-visibility-'));
  temporaryDirs.push(dir);
  return dir;
}

const ids = (count: number) =>
  Array.from({ length: count }, (_, index) => `codex:profile-${index}`);

describe('account visibility store', () => {
  it('round-trips a private file and treats absence as nothing hidden', async () => {
    const dir = ccsDir();
    expect(await readAccountVisibility(dir)).toEqual({
      state: 'ok',
      visibility: {
        hiddenProviders: [],
        hiddenAccountIds: [],
        trayHiddenProviders: [],
        trayHiddenAccountIds: [],
      },
    });
    const saved = await writeAccountVisibility(dir, {
      hiddenProviders: ['kimi-code'],
      hiddenAccountIds: ['codex:lime', 'plan-opencode-go-console-mac-0123456789ab'],
      trayHiddenProviders: ['qwen'],
      trayHiddenAccountIds: ['codex:party'],
    });
    const file = path.join(dir, ACCOUNT_VISIBILITY_FILE);
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    expect(JSON.parse(fs.readFileSync(file, 'utf8'))).toEqual({ version: 1, ...saved });
    expect(await readAccountVisibility(dir)).toEqual({ state: 'ok', visibility: saved });
    expect(fs.readdirSync(dir)).toEqual([ACCOUNT_VISIBILITY_FILE]);
  });

  it('reports an unsafe or malformed file as unavailable, never as empty', async () => {
    const dir = ccsDir();
    const file = path.join(dir, ACCOUNT_VISIBILITY_FILE);
    const valid = JSON.stringify({ version: 1, hiddenProviders: ['zai'], hiddenAccountIds: [] });
    for (const [contents, mode] of [
      [valid, 0o644],
      [valid, 0o620],
      ['{"version":1,', 0o600],
      [JSON.stringify({ version: 2, hiddenProviders: [], hiddenAccountIds: [] }), 0o600],
      [JSON.stringify({ version: 1, hiddenProviders: [], hiddenAccountIds: ['not an id'] }), 0o600],
      [
        JSON.stringify({ version: 1, hiddenProviders: [], hiddenAccountIds: [], extra: true }),
        0o600,
      ],
      [JSON.stringify({ version: 1, hiddenProviders: [], hiddenAccountIds: ids(129) }), 0o600],
      [' '.repeat(32 * 1024 + 1), 0o600],
      // The tray list is validated exactly like the dashboard provider list.
      [
        JSON.stringify({
          version: 1,
          hiddenProviders: [],
          hiddenAccountIds: [],
          trayHiddenProviders: 'qwen',
        }),
        0o600,
      ],
      [
        JSON.stringify({
          version: 1,
          hiddenProviders: [],
          hiddenAccountIds: [],
          trayHiddenProviders: [42],
        }),
        0o600,
      ],
      [
        JSON.stringify({
          version: 1,
          hiddenProviders: [],
          hiddenAccountIds: [],
          trayHiddenProviders: Array.from({ length: 33 }, () => 'qwen'),
        }),
        0o600,
      ],
      // The tray key does not replace a required key.
      [JSON.stringify({ version: 1, hiddenProviders: [], trayHiddenProviders: [] }), 0o600],
      // The tray id list is validated exactly like the dashboard id list.
      [
        JSON.stringify({
          version: 1,
          hiddenProviders: [],
          hiddenAccountIds: [],
          trayHiddenAccountIds: 'codex:party',
        }),
        0o600,
      ],
      [
        JSON.stringify({
          version: 1,
          hiddenProviders: [],
          hiddenAccountIds: [],
          trayHiddenAccountIds: ['not an id'],
        }),
        0o600,
      ],
      [
        JSON.stringify({
          version: 1,
          hiddenProviders: [],
          hiddenAccountIds: [],
          trayHiddenAccountIds: ids(129),
        }),
        0o600,
      ],
      [
        JSON.stringify({
          version: 1,
          hiddenProviders: [],
          trayHiddenProviders: [],
          trayHiddenAccountIds: [],
        }),
        0o600,
      ],
    ] as const) {
      fs.writeFileSync(file, contents);
      fs.chmodSync(file, mode);
      expect(await readAccountVisibility(dir)).toEqual({ state: 'unavailable' });
    }
    fs.rmSync(file);
    const target = path.join(dir, 'target.json');
    fs.writeFileSync(target, valid, { mode: 0o600 });
    fs.symlinkSync(target, file);
    expect(await readAccountVisibility(dir)).toEqual({ state: 'unavailable' });
    // A full replacement repairs it without following the link.
    await writeAccountVisibility(dir, {
      hiddenProviders: [],
      hiddenAccountIds: [],
      trayHiddenProviders: [],
      trayHiddenAccountIds: [],
    });
    expect(fs.lstatSync(file).isSymbolicLink()).toBe(false);
    expect(fs.readFileSync(target, 'utf8')).toBe(valid);
  });

  it('drops a provider that has left the table but keeps the rest of the file', async () => {
    const dir = ccsDir();
    const file = path.join(dir, ACCOUNT_VISIBILITY_FILE);
    fs.writeFileSync(
      file,
      JSON.stringify({
        version: 1,
        hiddenProviders: ['retired-provider', 'qwen'],
        hiddenAccountIds: ['zai:usage'],
        trayHiddenProviders: ['retired-provider', 'zai'],
      }),
      { mode: 0o600 }
    );
    expect(await readAccountVisibility(dir)).toEqual({
      state: 'ok',
      visibility: {
        hiddenProviders: ['qwen'],
        hiddenAccountIds: ['zai:usage'],
        trayHiddenProviders: ['zai'],
        trayHiddenAccountIds: [],
      },
    });
    // A file written before the tray lists existed keeps reading, with nothing tray-hidden.
    fs.writeFileSync(
      file,
      JSON.stringify({ version: 1, hiddenProviders: ['qwen'], hiddenAccountIds: [] }),
      { mode: 0o600 }
    );
    expect(await readAccountVisibility(dir)).toEqual({
      state: 'ok',
      visibility: {
        hiddenProviders: ['qwen'],
        hiddenAccountIds: [],
        trayHiddenProviders: [],
        trayHiddenAccountIds: [],
      },
    });
    // A file from the provider-only tray build (three lists) keeps reading too.
    fs.writeFileSync(
      file,
      JSON.stringify({
        version: 1,
        hiddenProviders: [],
        hiddenAccountIds: [],
        trayHiddenProviders: ['qwen'],
      }),
      { mode: 0o600 }
    );
    expect(await readAccountVisibility(dir)).toEqual({
      state: 'ok',
      visibility: {
        hiddenProviders: [],
        hiddenAccountIds: [],
        trayHiddenProviders: ['qwen'],
        trayHiddenAccountIds: [],
      },
    });
  });

  it('validates the body strictly', () => {
    expect(
      parseVisibilityBody({
        hiddenProviders: ['zai', 'zai'],
        hiddenAccountIds: ['zai:acct:9f2c41d0'],
      })
    ).toEqual({
      hiddenProviders: ['zai'],
      hiddenAccountIds: ['zai:acct:9f2c41d0'],
    });
    // Each list may be updated alone; the tray list deduplicates the same way.
    expect(parseVisibilityBody({ hiddenProviders: [] })).toEqual({ hiddenProviders: [] });
    expect(parseVisibilityBody({ hiddenAccountIds: [] })).toEqual({ hiddenAccountIds: [] });
    expect(parseVisibilityBody({ trayHiddenProviders: ['qwen', 'qwen'] })).toEqual({
      trayHiddenProviders: ['qwen'],
    });
    expect(parseVisibilityBody({ trayHiddenAccountIds: ['codex:gmail', 'codex:gmail'] })).toEqual({
      trayHiddenAccountIds: ['codex:gmail'],
    });
    expect(
      parseVisibilityBody({ trayHiddenAccountIds: ids(128) })?.trayHiddenAccountIds
    ).toHaveLength(128);
    expect(
      parseVisibilityBody({ hiddenProviders: [], hiddenAccountIds: ids(128) })?.hiddenAccountIds
    ).toHaveLength(128);
    expect(
      parseVisibilityBody({ trayHiddenProviders: Array.from({ length: 32 }, () => 'qwen') })
        ?.trayHiddenProviders
    ).toHaveLength(1);
    for (const body of [
      null,
      [],
      {},
      { hiddenProviders: ['unknown'], hiddenAccountIds: [] },
      { hiddenProviders: [], hiddenAccountIds: ids(129) },
      { hiddenProviders: [], hiddenAccountIds: ['codex'] },
      { hiddenProviders: [], hiddenAccountIds: ['Codex:party'] },
      { hiddenProviders: [], hiddenAccountIds: ['codex:party/../x'] },
      { hiddenProviders: [], hiddenAccountIds: ['a:b:c:d'] },
      { hiddenProviders: [], hiddenAccountIds: [42] },
      { hiddenProviders: 'zai', hiddenAccountIds: [] },
      { hiddenProviders: [], hiddenAccountIds: [], extra: [] },
      { hiddenProviders: Array.from({ length: 33 }, () => 'zai') },
      { trayHiddenProviders: ['unknown'] },
      { trayHiddenProviders: 'qwen' },
      { trayHiddenProviders: [42] },
      { trayHiddenProviders: Array.from({ length: 33 }, () => 'qwen') },
      { trayHiddenProviders: [], nope: [] },
      { trayHiddenAccountIds: ids(129) },
      { trayHiddenAccountIds: ['codex'] },
      { trayHiddenAccountIds: ['Codex:party'] },
      { trayHiddenAccountIds: ['codex:party/../x'] },
      { trayHiddenAccountIds: [42] },
      { trayHiddenAccountIds: 'codex:party' },
    ]) {
      expect(parseVisibilityBody(body)).toBeNull();
    }
    for (const id of [
      'claude:me@example.com',
      'antigravity:profile:party',
      'zai:acct:9f2c41d0',
      'codex:gmail',
    ]) {
      expect(parseVisibilityBody({ hiddenProviders: [], hiddenAccountIds: [id] })).not.toBeNull();
    }
  });
});

async function listen(
  app: express.Express | http.Server
): Promise<{ server: Server; base: string }> {
  const server = app instanceof http.Server ? app : http.createServer(app);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', () => resolve()));
  closers.push(() => new Promise<void>((resolve) => server.close(() => resolve())));
  return { server, base: `http://127.0.0.1:${(server.address() as AddressInfo).port}` };
}

describe('GET and PUT /api/accounts/visibility', () => {
  async function fixture(onChanged?: () => void) {
    const dir = ccsDir();
    const audits: unknown[] = [];
    let changes = 0;
    const app = express();
    app.use(express.json());
    app.use((req, _res, next) => {
      if (req.headers['x-test-session'] === 'true')
        Object.assign(req, { session: { authenticated: true } });
      next();
    });
    app.use(
      '/api/accounts',
      createAccountVisibilityRouter({
        read: () => readAccountVisibility(dir),
        write: (update) => updateAccountVisibility(dir, update),
        onChanged: onChanged ?? (() => (changes += 1)),
        audit: (counts) => audits.push(counts),
      })
    );
    const { base } = await listen(app);
    const url = `${base}/api/accounts/visibility`;
    const put = (body: unknown, headers: Record<string, string> = {}, target = url) =>
      fetch(target, {
        method: 'PUT',
        headers: {
          'content-type': 'application/json',
          origin: base,
          'x-test-session': 'true',
          ...headers,
        },
        body: typeof body === 'string' ? body : JSON.stringify(body),
      });
    return { dir, base, url, put, audits, changes: () => changes };
  }

  it('saves a full replacement and returns it on the next GET', async () => {
    const { dir, url, put, audits, changes } = await fixture();
    const body = {
      hiddenProviders: ['kimi-code'],
      hiddenAccountIds: ['codex:lime'],
      trayHiddenProviders: ['qwen'],
      trayHiddenAccountIds: ['codex:party'],
    };
    const response = await put(body);
    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('no-store');
    expect(await response.json()).toEqual(body);
    const read = await fetch(url, { headers: { 'x-test-session': 'true' } });
    expect(read.status).toBe(200);
    expect(read.headers.get('cache-control')).toBe('no-store');
    expect(await read.json()).toEqual(body);
    expect(fs.statSync(path.join(dir, ACCOUNT_VISIBILITY_FILE)).mode & 0o777).toBe(0o600);
    expect(changes()).toBe(1);
    expect(audits).toEqual([
      { hiddenProviders: 1, hiddenAccountIds: 1, trayHiddenProviders: 1, trayHiddenAccountIds: 1 },
    ]);
    expect(JSON.stringify(audits)).not.toContain('lime');
    expect(JSON.stringify(audits)).not.toContain('party');
  });

  it('updates one list without touching the others', async () => {
    const { url, put } = await fixture();
    const get = async () => {
      const read = await fetch(url, { headers: { 'x-test-session': 'true' } });
      expect(read.status).toBe(200);
      return read.json();
    };
    expect((await put({ trayHiddenProviders: ['qwen', 'zai'] })).status).toBe(200);
    expect(await get()).toEqual({
      hiddenProviders: [],
      hiddenAccountIds: [],
      trayHiddenProviders: ['qwen', 'zai'],
      trayHiddenAccountIds: [],
    });
    expect((await put({ hiddenProviders: ['kimi-code'] })).status).toBe(200);
    expect(await get()).toEqual({
      hiddenProviders: ['kimi-code'],
      hiddenAccountIds: [],
      trayHiddenProviders: ['qwen', 'zai'],
      trayHiddenAccountIds: [],
    });
    expect((await put({ hiddenAccountIds: ['codex:lime'] })).status).toBe(200);
    expect(await get()).toEqual({
      hiddenProviders: ['kimi-code'],
      hiddenAccountIds: ['codex:lime'],
      trayHiddenProviders: ['qwen', 'zai'],
      trayHiddenAccountIds: [],
    });
    expect((await put({ trayHiddenAccountIds: ['codex:party'] })).status).toBe(200);
    expect(await get()).toEqual({
      hiddenProviders: ['kimi-code'],
      hiddenAccountIds: ['codex:lime'],
      trayHiddenProviders: ['qwen', 'zai'],
      trayHiddenAccountIds: ['codex:party'],
    });
    // An empty tray list clears only that tray list.
    expect((await put({ trayHiddenProviders: [] })).status).toBe(200);
    expect(await get()).toEqual({
      hiddenProviders: ['kimi-code'],
      hiddenAccountIds: ['codex:lime'],
      trayHiddenProviders: [],
      trayHiddenAccountIds: ['codex:party'],
    });
  });

  it('refuses without a session, with a query, a foreign origin, a non-JSON type or a bad body', async () => {
    const { dir, base, url, put, changes } = await fixture();
    const valid = { hiddenProviders: [], hiddenAccountIds: [] };
    const expectError = async (response: Response, status: number, code: string) => {
      expect(response.status).toBe(status);
      expect(response.headers.get('cache-control')).toBe('no-store');
      const body = await response.json();
      expect(body.code).toBe(code);
      expect(typeof body.error).toBe('string');
      expect(JSON.stringify(body)).not.toContain('PRIVATE');
    };
    await expectError(await fetch(url), 401, 'auth_required');
    await expectError(await put(valid, { 'x-test-session': 'false' }), 401, 'auth_required');
    await expectError(
      await fetch(`${url}?x=1`, { headers: { 'x-test-session': 'true' } }),
      400,
      'unexpected_query'
    );
    await expectError(await put(valid, {}, `${url}?PRIVATE=1`), 400, 'unexpected_query');
    await expectError(
      await put(valid, { origin: 'https://PRIVATE.example' }),
      403,
      'origin_required'
    );
    const noOrigin = await fetch(url, {
      method: 'PUT',
      headers: { 'content-type': 'application/json', 'x-test-session': 'true' },
      body: JSON.stringify(valid),
    });
    await expectError(noOrigin, 403, 'origin_required');
    await expectError(
      await put(JSON.stringify(valid), { 'content-type': 'text/plain' }),
      415,
      'json_required'
    );
    for (const body of [
      { hiddenProviders: ['PRIVATE'], hiddenAccountIds: [] },
      { hiddenProviders: [], hiddenAccountIds: ['PRIVATE id'] },
      { hiddenProviders: [], hiddenAccountIds: ids(129) },
      { hiddenProviders: [], hiddenAccountIds: [], PRIVATE: true },
      { hiddenProviders: [], hiddenAccountIds: ['codex:x'], pad: 'x'.repeat(9000) },
      { trayHiddenProviders: ['PRIVATE'] },
      { trayHiddenProviders: 'qwen' },
      { trayHiddenProviders: Array.from({ length: 33 }, () => 'qwen') },
      { trayHiddenAccountIds: ['PRIVATE id'] },
      { trayHiddenAccountIds: ids(129) },
      { trayHiddenAccountIds: 'codex:party' },
      {},
      [],
    ]) {
      await expectError(await put(body), 400, 'invalid_body');
    }
    // Valid ids, but a body over 8 KB.
    const large = {
      hiddenProviders: [],
      hiddenAccountIds: Array.from(
        { length: 100 },
        (_, index) => `codex:${String(index).padStart(120, 'x')}`
      ),
    };
    expect(parseVisibilityBody(large)).not.toBeNull();
    await expectError(await put(large), 400, 'invalid_body');
    expect(fs.existsSync(path.join(dir, ACCOUNT_VISIBILITY_FILE))).toBe(false);
    expect(changes()).toBe(0);
    expect(
      (
        await fetch(`${base}/api/accounts/visibility`, {
          method: 'DELETE',
          headers: { 'x-test-session': 'true' },
        })
      ).status
    ).toBe(404);
  });

  it('answers 500 when the file cannot be read safely, and a full PUT repairs it', async () => {
    const { dir, url, put } = await fixture();
    const file = path.join(dir, ACCOUNT_VISIBILITY_FILE);
    const unsafe = '{"version":1,"hiddenProviders":[],"hiddenAccountIds":[]}';
    fs.writeFileSync(file, unsafe, { mode: 0o644 });
    fs.chmodSync(file, 0o644);
    const read = await fetch(url, { headers: { 'x-test-session': 'true' } });
    expect(read.status).toBe(500);
    expect(await read.json()).toEqual({
      error: 'Account visibility could not be read safely.',
      code: 'visibility_unavailable',
    });
    // A partial PUT cannot merge into an unreadable file: 500, and nothing is written.
    const partial = await put({ trayHiddenProviders: ['qwen'] });
    expect(partial.status).toBe(500);
    expect(await partial.json()).toEqual({
      error: 'Account visibility could not be read safely.',
      code: 'visibility_unavailable',
    });
    expect(fs.readFileSync(file, 'utf8')).toBe(unsafe);
    expect(fs.statSync(file).mode & 0o777).toBe(0o644);
    // A partial PUT naming the old three lists is still partial now: nothing is written.
    const threeLists = await put({
      hiddenProviders: ['zai'],
      hiddenAccountIds: [],
      trayHiddenProviders: ['qwen'],
    });
    expect(threeLists.status).toBe(500);
    expect(fs.readFileSync(file, 'utf8')).toBe(unsafe);
    // A full PUT (all four lists) replaces and repairs the file, exactly as before.
    const full = await put({
      hiddenProviders: ['zai'],
      hiddenAccountIds: [],
      trayHiddenProviders: ['qwen'],
      trayHiddenAccountIds: [],
    });
    expect(full.status).toBe(200);
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    const after = await fetch(url, { headers: { 'x-test-session': 'true' } });
    expect(after.status).toBe(200);
    expect(await after.json()).toEqual({
      hiddenProviders: ['zai'],
      hiddenAccountIds: [],
      trayHiddenProviders: ['qwen'],
      trayHiddenAccountIds: [],
    });
  });

  it('keeps the per-account dashboard and tray switches independent in all four combinations', async () => {
    const { url, put } = await fixture();
    const get = async () => {
      const read = await fetch(url, { headers: { 'x-test-session': 'true' } });
      expect(read.status).toBe(200);
      return read.json();
    };
    const id = 'codex:party';
    // [dashboard hidden, tray hidden] for one account, reached one switch at a time.
    const steps: Array<[string, Record<string, string[]>, boolean, boolean]> = [
      ['shown in both', { hiddenAccountIds: [] }, false, false],
      ['hidden only from the dashboard', { hiddenAccountIds: [id] }, true, false],
      ['hidden from both', { trayHiddenAccountIds: [id] }, true, true],
      ['hidden only from the tray', { hiddenAccountIds: [] }, false, true],
      ['shown in both again', { trayHiddenAccountIds: [] }, false, false],
      ['hidden only from the tray, directly', { trayHiddenAccountIds: [id] }, false, true],
      ['hidden from both, from the tray state', { hiddenAccountIds: [id] }, true, true],
      ['hidden only from the dashboard, from both', { trayHiddenAccountIds: [] }, true, false],
    ];
    for (const [name, body, dashboard, tray] of steps) {
      const response = await put(body);
      expect([name, response.status]).toEqual([name, 200]);
      const saved = await get();
      expect([
        name,
        saved.hiddenAccountIds.includes(id),
        saved.trayHiddenAccountIds.includes(id),
      ]).toEqual([name, dashboard, tray]);
      // The provider lists are never touched by an account switch.
      expect(saved.hiddenProviders).toEqual([]);
      expect(saved.trayHiddenProviders).toEqual([]);
    }
  });

  it('pushes accounts-changed to every open /ws client after a saved change', async () => {
    const server = http.createServer();
    const wss = new WebSocketServer({ server, path: '/ws' });
    const detach = attachDashboardEventServer(wss);
    closers.push(async () => {
      detach();
      for (const client of wss.clients) client.terminate();
      await new Promise<void>((resolve) => wss.close(() => resolve()));
    });
    const { base } = await listen(server);
    const connect = async () => {
      const socket = new WebSocket(`${base.replace('http', 'ws')}/ws`);
      await new Promise<void>((resolve, reject) => {
        socket.once('open', () => resolve());
        socket.once('error', reject);
      });
      closers.push(async () => socket.close());
      return socket;
    };
    const sockets = await Promise.all([connect(), connect()]);
    const received = sockets.map(
      (socket) =>
        new Promise<string>((resolve) => socket.once('message', (data) => resolve(String(data))))
    );
    const { put } = await fixture(() => broadcastDashboardEvent({ type: 'accounts-changed' }));
    expect((await put({ hiddenProviders: ['kimi-code'], hiddenAccountIds: [] })).status).toBe(200);
    expect(await Promise.all(received)).toEqual([
      '{"type":"accounts-changed"}',
      '{"type":"accounts-changed"}',
    ]);
    detach();
    expect(broadcastDashboardEvent({ type: 'accounts-changed' })).toBe(0);
  });
});

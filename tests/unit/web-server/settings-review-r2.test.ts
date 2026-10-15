import { afterEach, beforeEach, expect, it, mock, spyOn } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import express from 'express';
import type { Server } from 'http';
import { createAccountPreferencesRouter } from '../../../src/web-server/routes/account-preferences-routes';
import { createAccountVisibilityRouter } from '../../../src/web-server/routes/account-visibility-routes';
import claudeRoutes from '../../../src/web-server/routes/claude-desktop-routes';
import {
  defaultDashboardPreferences,
  readDashboardPreferences,
} from '../../../src/web-server/services/dashboard-preferences';
import {
  readAccountVisibility,
  ACCOUNT_VISIBILITY_FILE,
} from '../../../src/web-server/services/account-visibility';
import * as history from '../../../src/web-server/services/claude-history-sync-service';

const originalHome = process.env.CCS_HOME;
let temporary: string;
let ccsDir: string;
let server: Server;
let base: string;
beforeEach(async () => {
  temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-settings-r2-'));
  process.env.CCS_HOME = temporary;
  ccsDir = path.join(temporary, '.ccs');
  const app = express();
  app.use(express.json());
  app.use((req, _res, next) => {
    if (req.headers['x-session'] === 'true')
      Object.assign(req, { session: { authenticated: true } });
    next();
  });
  app.use('/api/accounts', createAccountPreferencesRouter());
  app.use('/api/accounts', createAccountVisibilityRouter({ onChanged: () => {}, audit: () => {} }));
  app.use('/api/claude', claudeRoutes);
  server = await new Promise<Server>((resolve) => {
    const listener = app.listen(0, '127.0.0.1', () => resolve(listener));
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Missing fixture port');
  base = `http://127.0.0.1:${address.port}`;
});
afterEach(async () => {
  server.closeAllConnections();
  await new Promise<void>((resolve) => server.close(() => resolve()));
  mock.restore();
  if (originalHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = originalHome;
  fs.rmSync(temporary, { recursive: true, force: true });
});
function send(
  route: string,
  body: unknown,
  headers: Record<string, string> = {},
  method = 'PATCH'
) {
  return fetch(`${base}/api/${route}`, {
    method,
    headers: { 'content-type': 'application/json', origin: base, 'x-session': 'true', ...headers },
    body: JSON.stringify(body),
  });
}
it('merges concurrent stale-browser visibility operations without replacing unrelated items', async () => {
  const route = 'accounts/visibility';
  const responses = await Promise.all([
    send(route, { hiddenProviders: { hide: ['codex'], show: [] } }),
    send(route, { hiddenProviders: { hide: ['claude'], show: [] } }),
    send(route, { hiddenAccountIds: { hide: ['codex:work'], show: [] } }),
    send(route, { trayHiddenAccountIds: { hide: ['codex:work'], show: [] } }),
  ]);
  expect(responses.map((r) => r.status)).toEqual([200, 200, 200, 200]);
  await send(route, { hiddenProviders: { show: ['codex'] } });
  expect(await readAccountVisibility(ccsDir)).toEqual({
    state: 'ok',
    visibility: {
      hiddenProviders: ['claude'],
      hiddenAccountIds: ['codex:work'],
      trayHiddenProviders: [],
      trayHiddenAccountIds: ['codex:work'],
    },
  });
  // The full-list API remains available to existing clients.
  expect((await send(route, { hiddenProviders: ['qwen'] }, {}, 'PUT')).status).toBe(200);
  expect((await readAccountVisibility(ccsDir)).state).toBe('ok');
});
it('visibility deltas keep the existing session, Origin, JSON and validation guards', async () => {
  const route = 'accounts/visibility';
  const body = { hiddenProviders: { hide: ['codex'] } };
  expect((await send(route, body, { 'x-session': 'false' })).status).toBe(401);
  expect((await send(route, body, { origin: 'https://foreign.test' })).status).toBe(403);
  expect((await send(route, body, { 'content-type': 'text/plain' })).status).toBe(415);
  for (const invalid of [
    { nope: {} },
    { hiddenProviders: { hide: null } },
    { hiddenProviders: {} },
    { hiddenProviders: { hide: ['unknown'] } },
    { hiddenProviders: { hide: ['codex'], show: ['codex'] } },
    { hiddenAccountIds: { hide: ['../escape'] } },
  ])
    expect((await send(route, invalid)).status).toBe(400);
  fs.mkdirSync(ccsDir, { recursive: true });
  fs.writeFileSync(path.join(ccsDir, ACCOUNT_VISIBILITY_FILE), 'broken');
  expect((await send(route, body)).status).toBe(500);
  expect(fs.readFileSync(path.join(ccsDir, ACCOUNT_VISIBILITY_FILE), 'utf8')).toBe('broken');
});
it('preference field and source-id operations merge atomically and keep PUT compatible', async () => {
  const route = 'accounts/preferences';
  expect((await send(route, defaultDashboardPreferences(), {}, 'PUT')).status).toBe(200);
  const source = {
    id: 'generic',
    tool: 'jsonl',
    host: 'ubuntu',
    path: '/fixture/logs',
    fieldMapping: {
      timestamp: 'created_at',
      model: 'model_name',
      inputTokens: 'usage.input_tokens',
    },
  };
  const second = { id: 'codex-extra', tool: 'codex', host: 'ubuntu', path: '/fixture/codex' };
  const responses = await Promise.all([
    send(route, { timeZone: 'Asia/Tokyo' }),
    send(route, { snapshotCleanup: { auto: false } }),
    send(route, { addUsageLogSource: source }),
    send(route, { addUsageLogSource: second }),
  ]);
  expect(responses.map((r) => r.status)).toEqual([200, 200, 200, 200]);
  expect((await send(route, { removeUsageLogSource: 'generic' })).status).toBe(200);
  expect(readDashboardPreferences(ccsDir)).toEqual({
    timeZone: 'Asia/Tokyo',
    snapshotCleanup: { auto: false },
    usageLogSources: [second],
  });
});
it('preference PATCH refuses invalid fields, sessions and transport before saving', async () => {
  const route = 'accounts/preferences';
  const body = { timeZone: 'UTC' };
  expect((await send(route, body, { 'x-session': 'false' })).status).toBe(401);
  expect((await send(route, body, { origin: 'https://foreign.test' })).status).toBe(403);
  expect((await send(route, body, { 'content-type': 'text/plain' })).status).toBe(415);
  for (const invalid of [
    {},
    { timeZone: 'bad' },
    { usageLogSources: [] },
    { snapshotCleanup: { auto: 'false' } },
    { removeUsageLogSource: '../x' },
  ])
    expect((await send(route, invalid)).status).toBe(400);
});
it('snapshot cleanup checks Origin and JSON before running, and reports a listing failure', async () => {
  const cleanup = spyOn(history, 'cleanupAllHistorySnapshots').mockRejectedValue(
    new Error('listing failed')
  );
  const route = 'claude/history-snapshots/cleanup';
  expect((await send(route, {}, { origin: 'https://foreign.test' }, 'POST')).status).toBe(403);
  expect((await send(route, {}, { 'content-type': 'text/plain' }, 'POST')).status).toBe(415);
  expect(cleanup).not.toHaveBeenCalled();
  expect((await send(route, {}, {}, 'POST')).status).toBe(500);
  expect(cleanup).toHaveBeenCalledTimes(1);
});
it('a failed snapshot profile listing rejects rather than reporting zero work', async () => {
  await expect(
    history.cleanupAllHistorySnapshots(async () => {
      throw new Error('fixture listing failed');
    })
  ).rejects.toThrow('fixture listing failed');
});

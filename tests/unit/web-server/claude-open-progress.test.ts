/**
 * Claude Open progress (CONTRACT-serving-misc section 4.4): with a managed
 * history policy, POST .../open answers 202 when the client sends
 * `Prefer: respond-async`, and GET /api/claude/desktop-profiles shows
 * `openOperation` until the Open ends. A client that does not send it (the
 * shipped web UI and both trays) keeps today's 200 or refusal. Everything is
 * synthetic: a temporary CCS_DIR, the offline history fixtures and mocked
 * transports.
 */
import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import express from 'express';
import fs from 'fs';
import type http from 'http';
import os from 'os';
import path from 'path';
import { authMiddleware } from '../../../src/web-server/middleware/auth-middleware';
import claudeDesktopRoutes, {
  prefersRespondAsync,
} from '../../../src/web-server/routes/claude-desktop-routes';
import * as transport from '../../../src/web-server/services/claude-desktop-transport';
import { ClaudeDesktopTransportError } from '../../../src/web-server/services/claude-desktop-transport';
import {
  ClaudeOpenOperations,
  type ClaudeOpenOperation,
} from '../../../src/web-server/services/claude-open-operations';
import { ClaudeHistoryOpenHeldError } from '../../../src/web-server/services/claude-desktop-open-service';
import { ProfileError } from '../../../src/errors/error-types';
import { getRecentLogEntries } from '../../../src/services/logging';

const fx = require('../claude-history/synthetic-history-fixtures.cjs');
const core = require('../../../scripts/claude-history/history-index-sync.cjs');
const seed = fx.bindings('Q');
const UNCONFIRMED =
  'Claude history copy is unconfirmed. Verify it has stopped before opening this profile.';

interface Gate {
  promise: Promise<void>;
  release: () => void;
}
function gate(): Gate {
  let release = () => {};
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}

let directory = '';
let previousCcsDir: string | undefined;
let previousAuth: string | undefined;
let server: http.Server;
let port = 0;
let appendCalls = 0;
let opened = 0;
let appendOutcome: 'success' | 'lost' = 'success';
let appendGate: Gate | null = null;
let openGate: Gate | null = null;
let openFailure: Error | null = null;

function snapshot(platform: string) {
  const records = platform === 'mac' ? [fx.envelope(fx.record(seed))] : [];
  return {
    profileId: 'platyr',
    platform,
    identity: {
      accountSha256: seed.profiles.platyr.accountSha256,
      orgSha256: seed.profiles.platyr.orgSha256,
    },
    records: records.map((row: { name: string; sha256: string; bytes: Buffer }) => ({
      name: row.name,
      sha256: row.sha256,
      base64: row.bytes.toString('base64'),
    })),
    revision: core.snapshotRevision(records),
    snapshotStable: true,
    endpoint: seed.endpoint,
    nativeGuard: {
      ...core.NATIVE[platform],
      warmGuardVerified: true,
      autoResumeGuardVerified: true,
    },
    noPendingInput: true,
    noScheduledWork: true,
    protectedSnapshotStable: true,
  };
}

const policy = {
  version: 1,
  enabled: true,
  sourcePlatform: 'mac',
  identity: {
    accountUuid: seed.profiles.platyr.accountUuid,
    organizationUuid: seed.profiles.platyr.organizationUuid,
  },
  project: { cwd: seed.project, originCwd: seed.project, transcriptRoot: seed.transcriptRoot },
  ssh: Object.fromEntries(
    ['mac', 'windows'].map((platform) => [
      platform,
      { alias: seed.aliases[platform], ...seed.plainEndpoint },
    ])
  ),
};

function writeManifest(withPolicy = true, withWindowsLauncher = true): void {
  const row: Record<string, unknown> = {
    id: 'platyr',
    email: 'synthetic@example.com',
    mac: {
      launcherName: 'Synthetic.app',
      launcherPath: '/Users/synthetic/Synthetic.app',
      profilePath: '/Users/synthetic/Claude',
      sshHost: 'synthetic-mac',
    },
    windows: {
      launcherName: 'Synthetic.lnk',
      launcherPath: 'C:\\Synthetic.lnk',
      profilePath: 'C:\\Claude',
      sshHost: 'synthetic-windows',
    },
  };
  if (withPolicy) row.historySync = policy;
  if (!withWindowsLauncher) delete row.windows;
  fs.writeFileSync(
    path.join(directory, 'claude-desktop-profiles.json'),
    JSON.stringify({ version: 1, profiles: [row] }),
    { mode: 0o600 }
  );
}

interface Answer {
  status: number;
  body: Record<string, unknown>;
  preferenceApplied?: string | null;
}

async function request(
  method: 'GET' | 'POST',
  suffix = '',
  headers: Record<string, string> = {}
): Promise<Answer> {
  const response = await fetch(`http://127.0.0.1:${port}/api/claude/desktop-profiles${suffix}`, {
    method,
    headers: { 'Content-Type': 'application/json', ...headers },
    ...(method === 'POST' ? { body: JSON.stringify({ platform: 'windows' }) } : {}),
  });
  return { status: response.status, body: (await response.json()) as Record<string, unknown> };
}

const ASYNC = { Prefer: 'respond-async' };
/** The redesigned clients' Open: asks for the 202 progress form. */
const open = () => request('POST', '/platyr/open', ASYNC);
/** The shipped clients' Open: no Prefer header, so it waits for the outcome. */
const openAndWait = () => request('POST', '/platyr/open');

async function currentOperation(): Promise<ClaudeOpenOperation | null> {
  const { body } = await request('GET');
  const profiles = body.profiles as Array<{ openOperation: ClaudeOpenOperation | null }>;
  return profiles[0]?.openOperation ?? null;
}

async function waitForState(state: string): Promise<ClaudeOpenOperation> {
  const deadline = Date.now() + 5000;
  for (;;) {
    const operation = await currentOperation();
    if (operation?.state === state) return operation;
    if (Date.now() > deadline)
      throw new Error(`Open never reached ${state}; last ${JSON.stringify(operation)}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

beforeEach(async () => {
  directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-open-progress-'));
  fs.chmodSync(directory, 0o700);
  previousCcsDir = process.env.CCS_DIR;
  previousAuth = process.env.CCS_DASHBOARD_AUTH_ENABLED;
  process.env.CCS_DIR = directory;
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
  appendCalls = opened = 0;
  appendOutcome = 'success';
  appendGate = openGate = null;
  openFailure = null;
  writeManifest();
  spyOn(transport, 'runClaudeHistoryHelper').mockImplementation(
    async (_launcher: unknown, platform: string, _id: string, request: { mode: string }) => {
      let out: unknown;
      if (request.mode === 'closed-check') out = { closed: true };
      else if (request.mode === 'collect') out = snapshot(platform);
      else if (request.mode === 'verify-transcripts') out = { verified: true };
      else if (request.mode === 'protected-check') out = { unchanged: true };
      else if (request.mode === 'append') {
        appendCalls++;
        await appendGate?.promise;
        if (appendOutcome === 'lost') throw new Error('SYNTHETIC_PRIVATE_LOST_RECEIPT');
        out = {
          status: 'created_metadata',
          createdCount: 1,
          protectedBytesUnchanged: true,
          writerQuiescent: true,
        };
      } else throw new Error('unexpected helper mode');
      return Buffer.from(JSON.stringify(out));
    }
  );
  const launch = async () => {
    await openGate?.promise;
    if (openFailure) throw openFailure;
    opened++;
  };
  spyOn(transport, 'openClaudeMacLauncher').mockImplementation(launch);
  spyOn(transport, 'openClaudeWindowsLauncher').mockImplementation(launch);

  const app = express();
  app.use(express.json());
  app.use((req, _res, next) => {
    req.session = { authenticated: true } as express.Request['session'];
    next();
  });
  app.use(authMiddleware);
  app.use('/api/claude', claudeDesktopRoutes);
  server = await new Promise<http.Server>((resolve) => {
    const instance = app.listen(0, '127.0.0.1', () => resolve(instance));
  });
  port = (server.address() as { port: number }).port;
});

afterEach(async () => {
  appendGate?.release();
  openGate?.release();
  await new Promise<void>((resolve) => server.close(() => resolve()));
  mock.restore();
  if (previousCcsDir === undefined) delete process.env.CCS_DIR;
  else process.env.CCS_DIR = previousCcsDir;
  if (previousAuth === undefined) delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
  else process.env.CCS_DASHBOARD_AUTH_ENABLED = previousAuth;
  fs.rmSync(directory, { recursive: true, force: true });
});

describe('Claude Open with a managed history copy', () => {
  it('answers 202 and moves openOperation through copying, opening and opened', async () => {
    appendGate = gate();
    openGate = gate();
    const started = await open();
    expect(started.status).toBe(202);
    expect(started.body).toEqual({
      id: 'platyr',
      platform: 'windows',
      state: 'checking',
      operationId: expect.stringMatching(/^op_[a-f0-9]{24}$/),
    });
    const operationId = started.body.operationId;

    expect(await waitForState('copying')).toEqual({
      id: operationId as string,
      platform: 'windows',
      state: 'copying',
      confirmedCount: 0,
      totalCount: 1,
      message: null,
    });
    // A second click while it runs joins the same operation; nothing runs twice.
    const again = await open();
    expect(again.status).toBe(202);
    expect(again.body.operationId).toBe(operationId);

    appendGate.release();
    expect(await waitForState('opening')).toMatchObject({
      id: operationId,
      confirmedCount: 1,
      totalCount: 1,
    });
    openGate.release();
    expect(await waitForState('opened')).toEqual({
      id: operationId as string,
      platform: 'windows',
      state: 'opened',
      confirmedCount: 1,
      totalCount: 1,
      message: null,
    });
    expect(appendCalls).toBe(1);
    expect(opened).toBe(1);
  });

  it('ends blocked_uncertain on a lost append and never replays it', async () => {
    appendOutcome = 'lost';
    const started = await open();
    expect(started.status).toBe(202);
    const blocked = await waitForState('blocked_uncertain');
    expect(blocked).toMatchObject({ state: 'blocked_uncertain', message: UNCONFIRMED });
    expect(opened).toBe(0);
    // The durable marker holds Open: a new click is refused before any helper call.
    const retry = await open();
    expect(retry).toEqual({
      status: 409,
      body: { error: UNCONFIRMED, code: 'history_unconfirmed' },
    });
    expect(appendCalls).toBe(1);
    expect(opened).toBe(0);
  });

  it('ends failed with a fixed sentence when the launcher fails', async () => {
    openFailure = new ClaudeDesktopTransportError();
    expect((await open()).status).toBe(202);
    expect(await waitForState('failed')).toMatchObject({
      state: 'failed',
      message: 'Claude desktop request failed.',
    });
  });

  it('keeps the ordinary 200 Open when no managed policy applies', async () => {
    writeManifest(false);
    const response = await open();
    expect(response).toEqual({
      status: 200,
      body: { opened: true, id: 'platyr', platform: 'windows' },
    });
    expect(opened).toBe(1);
    expect(appendCalls).toBe(0);
    expect(await currentOperation()).toBeNull();
  });

  it('keeps the old refusals for an unknown profile and a configured launcher', async () => {
    for (const headers of [ASYNC, {}]) {
      expect((await request('POST', '/missing/open', headers)).status).toBe(400);
      expect((await request('POST', '/gmail/open', headers)).status).toBe(404);
    }
  });

  it('answers 409 for a policy profile whose launcher is not configured, with or without Prefer', async () => {
    writeManifest(true, false);
    for (const headers of [ASYNC, {}]) {
      expect(await request('POST', '/platyr/open', headers)).toEqual({
        status: 409,
        body: { error: 'Claude desktop launcher is not configured for this platform.' },
      });
    }
    expect(appendCalls).toBe(0);
    expect(opened).toBe(0);
    expect(await currentOperation()).toBeNull();
  });

  it('gives two concurrent first POSTs the same operation', async () => {
    appendGate = gate();
    const [first, second] = await Promise.all([open(), open()]);
    expect([first.status, second.status]).toEqual([202, 202]);
    expect(first.body.operationId).toMatch(/^op_[a-f0-9]{24}$/);
    expect(second.body.operationId).toBe(first.body.operationId);
    appendGate.release();
    expect((await waitForState('opened')).id).toBe(first.body.operationId as string);
    expect(appendCalls).toBe(1);
    expect(opened).toBe(1);
  });

  it('marks the 202 with Preference-Applied', async () => {
    const response = await fetch(
      `http://127.0.0.1:${port}/api/claude/desktop-profiles/platyr/open`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Prefer: 'wait=5, Respond-Async' },
        body: JSON.stringify({ platform: 'windows' }),
      }
    );
    expect(response.status).toBe(202);
    expect(response.headers.get('preference-applied')).toBe('respond-async');
    await response.json();
    await waitForState('opened');
  });

  it('never exposes identities, titles, paths or ssh details in the progress', async () => {
    appendGate = gate();
    const started = await open();
    const copying = await waitForState('copying');
    appendGate.release();
    const finished = await waitForState('opened');
    for (const value of [started.body, copying, finished]) {
      const text = JSON.stringify(value);
      for (const secret of [
        seed.profiles.platyr.accountUuid,
        seed.profiles.platyr.organizationUuid,
        seed.aliases.mac,
        seed.aliases.windows,
        seed.plainEndpoint.hostname,
        seed.plainEndpoint.username,
        seed.transcriptRoot,
        seed.project,
        'SYNTHETIC_PRIVATE',
        'synthetic-windows',
        'C:\\\\',
      ])
        expect(text).not.toContain(secret);
      expect(text).not.toMatch(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i);
    }
    expect(Object.keys(finished).sort()).toEqual([
      'confirmedCount',
      'id',
      'message',
      'platform',
      'state',
      'totalCount',
    ]);
  });
});

describe('Claude Open without Prefer: respond-async (the shipped clients)', () => {
  it('waits for the managed copy and Open, then answers the old 200', async () => {
    appendGate = gate();
    const pending = openAndWait();
    // The progress is still visible to a polling client while the old client waits.
    expect(await waitForState('copying')).toMatchObject({ confirmedCount: 0, totalCount: 1 });
    appendGate.release();
    expect(await pending).toEqual({
      status: 200,
      body: { opened: true, id: 'platyr', platform: 'windows' },
    });
    expect(await currentOperation()).toMatchObject({ state: 'opened', confirmedCount: 1 });
    expect(appendCalls).toBe(1);
    expect(opened).toBe(1);
  });

  it('answers 409 when the copy ends unconfirmed, as before', async () => {
    appendOutcome = 'lost';
    expect(await openAndWait()).toEqual({
      status: 409,
      body: { error: UNCONFIRMED, code: 'history_unconfirmed' },
    });
    expect((await currentOperation())?.state).toBe('blocked_uncertain');
    expect(opened).toBe(0);
    // The durable marker still holds the next click; nothing is replayed.
    expect((await openAndWait()).status).toBe(409);
    expect(appendCalls).toBe(1);
  });

  it.each([
    [new ClaudeDesktopTransportError(), 502],
    [new ClaudeDesktopTransportError(true), 504],
  ])('answers the launcher failure with its old status', async (failure, status) => {
    openFailure = failure;
    const response = await openAndWait();
    expect(response.status).toBe(status);
    expect(response.body.error).toBe(failure.message);
    expect((await currentOperation())?.state).toBe('failed');
  });

  it('joins a running 202 Open and answers with its outcome', async () => {
    appendGate = gate();
    const started = await open();
    expect(started.status).toBe(202);
    await waitForState('copying');
    const waits = spyOn(ClaudeOpenOperations.prototype, 'settled');
    const joined = openAndWait();
    // Release the copy only once the second POST is waiting on the running Open.
    for (const deadline = Date.now() + 5000; waits.mock.calls.length === 0; ) {
      if (Date.now() > deadline) throw new Error('The second POST never joined the Open.');
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    appendGate.release();
    expect(await joined).toEqual({
      status: 200,
      body: { opened: true, id: 'platyr', platform: 'windows' },
    });
    expect((await currentOperation())?.id).toBe(started.body.operationId as string);
    expect(appendCalls).toBe(1);
    expect(opened).toBe(1);
  });

  it('logs how each Open ended with its state and error class only', async () => {
    openFailure = new ClaudeDesktopTransportError(true);
    await openAndWait();
    const finished = getRecentLogEntries().filter(
      (entry) => entry.event === 'claude.open.finished'
    );
    const last = finished[finished.length - 1];
    expect(last?.level).toBe('warn');
    expect(last?.context).toEqual({
      state: 'failed',
      kind: 'transport_timeout',
      platform: 'windows',
    });
    expect(JSON.stringify(last)).not.toMatch(/platyr|synthetic|Synthetic/);
  });
});

describe('Prefer header parsing', () => {
  it('reads respond-async as one preference among others, in any letter case', () => {
    expect(prefersRespondAsync('respond-async')).toBe(true);
    expect(prefersRespondAsync('wait=10, respond-async')).toBe(true);
    expect(prefersRespondAsync(' Respond-Async ; x=1')).toBe(true);
    expect(prefersRespondAsync(['return=minimal', 'respond-async'])).toBe(true);
    expect(prefersRespondAsync('respond-asynchronously')).toBe(false);
    expect(prefersRespondAsync('return=respond-async')).toBe(false);
    expect(prefersRespondAsync('')).toBe(false);
    expect(prefersRespondAsync(undefined)).toBe(false);
    expect(prefersRespondAsync(`${'x'.repeat(1100)}, respond-async`)).toBe(false);
  });
});

describe('ClaudeOpenOperations store', () => {
  it('starts empty after a restart and keeps a running Open single', async () => {
    const first = new ClaudeOpenOperations();
    let runs = 0;
    const never = () => {
      runs++;
      return new Promise<void>(() => {});
    };
    const running = first.start('/scope', 'platyr', 'mac', never);
    expect(first.start('/scope', 'platyr', 'mac', never).id).toBe(running.id);
    expect(runs).toBe(1);
    expect(first.forProfile('/scope', 'platyr')?.state).toBe('checking');
    expect(first.forProfile('/other-scope', 'platyr')).toBeNull();
    // A restarted server has a new store: nothing resumes.
    expect(new ClaudeOpenOperations().forProfile('/scope', 'platyr')).toBeNull();
  });

  it('maps failures to fixed states and forgets finished operations after the retention', async () => {
    let now = 1_000;
    const store = new ClaudeOpenOperations({ now: () => now, retainMs: 60_000 });
    const cases: Array<[unknown, string, string]> = [
      [new ClaudeHistoryOpenHeldError(), 'blocked_uncertain', UNCONFIRMED],
      [
        new ProfileError('Claude desktop profile was not found.'),
        'failed',
        'Claude desktop profile was not found.',
      ],
      [new ClaudeDesktopTransportError(true), 'failed', 'Claude desktop request timed out.'],
      [
        new Error('SYNTHETIC_PRIVATE_DETAIL /home/x'),
        'failed',
        'Claude account could not be opened safely.',
      ],
    ];
    for (const [index, [error, state, message]] of cases.entries()) {
      store.start('/scope', `p${index}`, 'mac', () => Promise.reject(error));
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(store.forProfile('/scope', `p${index}`)).toMatchObject({ state, message });
    }
    store.start('/scope', 'sync-throw', 'windows', () => {
      throw new Error('synchronous');
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(store.forProfile('/scope', 'sync-throw')?.state).toBe('failed');
    now += 60_000;
    expect(store.forProfile('/scope', 'p0')).toBeNull();
  });

  it('prefers a running operation over a finished one for the same profile', async () => {
    const store = new ClaudeOpenOperations();
    store.start('/scope', 'platyr', 'mac', () => Promise.resolve());
    await new Promise((resolve) => setTimeout(resolve, 0));
    const running = store.start('/scope', 'platyr', 'windows', () => new Promise<void>(() => {}));
    expect(store.forProfile('/scope', 'platyr')).toMatchObject({
      id: running.id,
      platform: 'windows',
      state: 'checking',
    });
  });
});

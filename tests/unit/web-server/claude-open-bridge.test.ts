/**
 * G13 TS bridge: the Open and cleanup routes delegate to the bound Rust Open owner after
 * their guards, keep the TypeScript flow on the unbound handoff, and answer 500 without
 * replaying a click when the reply is lost. The profiles accept passes the owner's
 * `openOperation` progress through when it is bound. A scripted RustService subclass drives
 * the real route handlers over a local express server; no real host, app or provider.
 */
import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import express from 'express';
import fs from 'fs';
import type http from 'http';
import os from 'os';
import path from 'path';
import { authMiddleware } from '../../../src/web-server/middleware/auth-middleware';
import claudeDesktopRoutes from '../../../src/web-server/routes/claude-desktop-routes';
import * as transport from '../../../src/web-server/services/claude-desktop-transport';
import { RustService } from '../../../src/web-server/rust-service';
import {
  acceptRustDesktopProfiles,
  acceptRustDesktopProfilesWithOperations,
} from '../../../src/web-server/rust-profile-input';

interface Reply {
  status: number;
  body: unknown;
}

/** A bound-owner stand-in: the route only reads the public gate and the two bridge methods. */
class FakeOwner extends RustService {
  openReply: Reply | null = null;
  openError: Error | null = null;
  cleanupReply: Reply | null = null;
  openCalls: Array<{ id: string; platform: string; respondAsync: boolean }> = [];
  cleanupCalls = 0;
  enabled = true;

  constructor() {
    super('off');
  }
  override get claudeOpenEnabled(): boolean {
    return this.enabled;
  }
  override async claudeOpen(
    id: string,
    platform: 'mac' | 'windows',
    respondAsync: boolean
  ): Promise<Reply | null> {
    this.openCalls.push({ id, platform, respondAsync });
    if (this.openError) throw this.openError;
    return this.openReply;
  }
  override async claudeHistoryCleanup(): Promise<Reply | null> {
    this.cleanupCalls++;
    return this.cleanupReply;
  }
}

let directory = '';
let previousCcsDir: string | undefined;
let previousAuth: string | undefined;
let server: http.Server;
let port = 0;
let owner: FakeOwner;
let launched = 0;

function writeManifest(profiles: unknown[]): void {
  fs.writeFileSync(
    path.join(directory, 'claude-desktop-profiles.json'),
    JSON.stringify({ version: 1, profiles }),
    { mode: 0o600 }
  );
}

const manifestRow = {
  id: 'gmail',
  email: 'synthetic@example.test',
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

async function request(
  method: 'GET' | 'POST',
  suffix = '',
  headers: Record<string, string> = {},
  body?: unknown
): Promise<{ status: number; body: Record<string, unknown>; preferenceApplied?: string | null }> {
  const response = await fetch(`http://127.0.0.1:${port}/api/claude${suffix}`, {
    method,
    headers: { 'Content-Type': 'application/json', ...headers },
    ...(method === 'POST' ? { body: JSON.stringify(body ?? { platform: 'windows' }) } : {}),
  });
  const applied = response.headers.get('preference-applied');
  return {
    status: response.status,
    body: (await response.json()) as Record<string, unknown>,
    ...(applied === null ? {} : { preferenceApplied: applied }),
  };
}

beforeEach(async () => {
  directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-open-bridge-'));
  fs.chmodSync(directory, 0o700);
  previousCcsDir = process.env.CCS_DIR;
  previousAuth = process.env.CCS_DASHBOARD_AUTH_ENABLED;
  process.env.CCS_DIR = directory;
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
  launched = 0;
  owner = new FakeOwner();
  writeManifest([manifestRow]);
  spyOn(transport, 'openClaudeMacLauncher').mockImplementation(async () => {
    launched++;
  });
  spyOn(transport, 'openClaudeWindowsLauncher').mockImplementation(async () => {
    launched++;
  });
  const app = express();
  app.use(express.json());
  app.use((req, _res, next) => {
    req.session = { authenticated: true } as express.Request['session'];
    next();
  });
  app.use(authMiddleware);
  app.use((_req, res, next) => {
    res.locals.rustService = owner;
    next();
  });
  app.use('/api/claude', claudeDesktopRoutes);
  server = await new Promise<http.Server>((resolve) => {
    const instance = app.listen(0, '127.0.0.1', () => resolve(instance));
  });
  port = (server.address() as { port: number }).port;
});

afterEach(async () => {
  await new Promise<void>((resolve) => server.close(() => resolve()));
  mock.restore();
  if (previousCcsDir === undefined) delete process.env.CCS_DIR;
  else process.env.CCS_DIR = previousCcsDir;
  if (previousAuth === undefined) delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
  else process.env.CCS_DASHBOARD_AUTH_ENABLED = previousAuth;
  fs.rmSync(directory, { recursive: true, force: true });
});

describe('Open route delegation', () => {
  it('sends the owner reply unchanged and never runs the TS launcher', async () => {
    owner.openReply = { status: 200, body: { opened: true, id: 'gmail', platform: 'windows' } };
    const answer = await request('POST', '/desktop-profiles/gmail/open');
    expect(answer.status).toBe(200);
    expect(answer.body).toEqual({ opened: true, id: 'gmail', platform: 'windows' });
    expect(owner.openCalls).toEqual([{ id: 'gmail', platform: 'windows', respondAsync: false }]);
    expect(launched).toBe(0);
  });

  it('marks the 202 with Preference-Applied and forwards the async preference', async () => {
    owner.openReply = {
      status: 202,
      body: { id: 'gmail', platform: 'windows', state: 'checking', operationId: 'op_0'.repeat(1) + '0'.repeat(24) },
    };
    const answer = await request('POST', '/desktop-profiles/gmail/open', {
      Prefer: 'respond-async',
    });
    expect(answer.status).toBe(202);
    expect(answer.preferenceApplied).toBe('respond-async');
    expect(owner.openCalls[0]?.respondAsync).toBe(true);
  });

  it('passes the hold refusal body through', async () => {
    owner.openReply = {
      status: 409,
      body: {
        error: 'Claude history copy is unconfirmed. Verify it has stopped before opening this profile.',
        code: 'history_unconfirmed',
      },
    };
    const answer = await request('POST', '/desktop-profiles/gmail/open');
    expect(answer.status).toBe(409);
    expect(answer.body.code).toBe('history_unconfirmed');
    expect(launched).toBe(0);
  });

  it('keeps the TypeScript flow on the unbound handoff', async () => {
    owner.openReply = null;
    // An unknown profile: the TS flow answers 404 before any transport call.
    const answer = await request('POST', '/desktop-profiles/unknown/open');
    expect(answer.status).toBe(404);
    expect(answer.body).toEqual({ error: 'Claude desktop profile was not found.' });
    expect(owner.openCalls.length).toBe(1);
    expect(launched).toBe(0);
  });

  it('answers 500 once when the reply is lost, never replaying the click', async () => {
    owner.openError = new Error('synthetic transport loss');
    const answer = await request('POST', '/desktop-profiles/gmail/open');
    expect(answer.status).toBe(500);
    expect(answer.body).toEqual({ error: 'Claude account could not be opened safely.' });
    expect(owner.openCalls.length).toBe(1);
    expect(launched).toBe(0);
  });

  it('still applies the route guards before delegating', async () => {
    owner.openReply = { status: 200, body: { opened: true } };
    const badBody = await request('POST', '/desktop-profiles/gmail/open', {}, { platform: 'linux' });
    expect(badBody.status).toBe(400);
    const badId = await request('POST', '/desktop-profiles/bad..id/open');
    expect(badId.status).toBe(400);
    expect(owner.openCalls.length).toBe(0);
  });
});

describe('cleanup route delegation', () => {
  it('sends the owner totals unchanged', async () => {
    owner.cleanupReply = {
      status: 200,
      body: { profiles: 2, targets: 3, kept: 3, deleted: 1, skipped: 1, failed: 1 },
    };
    const answer = await request('POST', '/history-snapshots/cleanup', {}, {});
    expect(answer.status).toBe(200);
    expect(answer.body).toEqual({ profiles: 2, targets: 3, kept: 3, deleted: 1, skipped: 1, failed: 1 });
    expect(owner.cleanupCalls).toBe(1);
  });

  it('keeps the TypeScript cleanup on the unbound handoff', async () => {
    owner.cleanupReply = null;
    writeManifest([]);
    const answer = await request('POST', '/history-snapshots/cleanup', {}, {});
    expect(answer.status).toBe(200);
    expect(answer.body).toEqual({ profiles: 0, targets: 0, kept: 0, deleted: 0, skipped: 0, failed: 0 });
  });
});

describe('the bound-owner gate', () => {
  it('is off in mode off and on Windows even with the group on', () => {
    const previousGroup = process.env.AAC_RUST_GROUP_CLAUDE_OPEN;
    process.env.AAC_RUST_GROUP_CLAUDE_OPEN = 'on';
    try {
      expect(new RustService('off').claudeOpenEnabled).toBe(false);
      // The dashboard host is Linux; a Windows front door keeps the TS flow.
      expect(new RustService('on').claudeOpenEnabled).toBe(process.platform !== 'win32');
    } finally {
      if (previousGroup === undefined) delete process.env.AAC_RUST_GROUP_CLAUDE_OPEN;
      else process.env.AAC_RUST_GROUP_CLAUDE_OPEN = previousGroup;
    }
  });

  it('hands off before dispatch without a lease on the canonical root', async () => {
    const previousGroup = process.env.AAC_RUST_GROUP_CLAUDE_OPEN;
    const previousPlatform = Object.getOwnPropertyDescriptor(process, 'platform');
    process.env.AAC_RUST_GROUP_CLAUDE_OPEN = 'on';
    Object.defineProperty(process, 'platform', { value: 'linux', configurable: true });
    try {
      const service = new RustService('on');
      expect(service.claudeOpenEnabled).toBe(true);
      // No lease: the pre-dispatch handoff, never a throw.
      expect(await service.claudeOpen('gmail', 'mac', false)).toBeNull();
      expect(await service.claudeHistoryCleanup()).toBeNull();
    } finally {
      if (previousPlatform) Object.defineProperty(process, 'platform', previousPlatform);
      if (previousGroup === undefined) delete process.env.AAC_RUST_GROUP_CLAUDE_OPEN;
      else process.env.AAC_RUST_GROUP_CLAUDE_OPEN = previousGroup;
    }
  });
});

describe('the profiles accept with the owner progress', () => {
  const operation = {
    id: 'op_0123456789abcdef01234567',
    platform: 'mac',
    state: 'copying',
    confirmedCount: 1,
    totalCount: 4,
    message: null,
  };

  it('accepts rows carrying null or a shaped operation', () => {
    const body = {
      profiles: [
        { id: 'gmail', email: 'g@example.test', openOperation: operation },
        { id: 'store', email: 's@example.test', openOperation: null },
        { email: 'noid@example.test' },
      ],
    };
    expect(acceptRustDesktopProfilesWithOperations(body)).toEqual({ value: body });
  });

  it('refuses a malformed operation so the TS reader answers', () => {
    for (const bad of [
      { ...operation, state: 'unknown' },
      { ...operation, confirmedCount: -1 },
      { ...operation, totalCount: 'four' },
      { ...operation, extra: 1 },
    ]) {
      expect(
        acceptRustDesktopProfilesWithOperations({
          profiles: [{ id: 'gmail', openOperation: bad }],
        })
      ).toBeNull();
    }
    expect(acceptRustDesktopProfilesWithOperations({ profiles: [] })).toEqual({
      value: { profiles: [] },
    });
    expect(acceptRustDesktopProfilesWithOperations({ extra: 1 })).toBeNull();
  });

  it('the unbound accept still adds this process’s progress', () => {
    const accept = acceptRustDesktopProfiles((profiles) =>
      profiles.map((profile) =>
        typeof profile.id === 'string' && profile.id
          ? { ...profile, openOperation: null }
          : profile
      )
    );
    const body = { profiles: [{ id: 'gmail', email: 'g@example.test' }] };
    expect(accept(body)).toEqual({
      value: { profiles: [{ id: 'gmail', email: 'g@example.test', openOperation: null }] },
    });
  });
});

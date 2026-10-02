import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  activateCodexProfile,
  type CodexActivationRuntime,
} from '../../../src/codex-auth/activate-codex-profile';
import { acquireCodexActivationLock } from '../../../src/codex-auth/codex-activation-lock';
import { CodexProfileRegistry } from '../../../src/codex-auth/codex-profile-registry';
import { resolveCodexProfileDir } from '../../../src/codex-auth/codex-profile-paths';
import { invalidateCodexAuthProfilesCache } from '../../../src/codex-auth/codex-auth-dashboard-service';

const originalCcsHome = process.env.CCS_HOME;
let temporary: string;
let codexHome: string;
let registry: CodexProfileRegistry;

interface SyntheticIdentity {
  accountId?: unknown;
  storedAccountId?: unknown;
  userId?: unknown;
  subject?: unknown;
  issuer?: unknown;
  email?: string;
  rotation?: number;
}

function syntheticAuth(identity: SyntheticIdentity): Buffer {
  const rotation = identity.rotation ?? 0;
  const header = Buffer.from('{"alg":"none"}').toString('base64url');
  const payload = Buffer.from(
    JSON.stringify({
      email: identity.email ?? 'shared@example.test',
      sub: identity.subject,
      iss: identity.issuer,
      exp: 1000 + rotation,
      'https://api.openai.com/auth': {
        chatgpt_account_id: identity.accountId,
        chatgpt_user_id: identity.userId,
        chatgpt_plan_type: rotation ? 'business' : 'plus',
      },
    })
  ).toString('base64url');
  return Buffer.from(
    JSON.stringify({
      tokens: {
        id_token: `${header}.${payload}.fakesig`,
        access_token: `synthetic-access-${rotation}`,
        refresh_token: `synthetic-refresh-${rotation}`,
        ...(identity.storedAccountId !== undefined ? { account_id: identity.storedAccountId } : {}),
      },
    })
  );
}

function profileAuth(name: string): string {
  return path.join(resolveCodexProfileDir(name), 'auth.json');
}

function writeProfile(name: string, content: Buffer): void {
  fs.mkdirSync(resolveCodexProfileDir(name), { recursive: true });
  fs.writeFileSync(profileAuth(name), content, { mode: 0o600 });
  registry.createProfile(name);
}

function stubRuntime(events: string[]): CodexActivationRuntime {
  return {
    async stop() {
      events.push('stop');
    },
    async start() {
      events.push('start');
    },
  };
}

const personal = { accountId: 'personal', storedAccountId: 'personal', userId: 'same-user' };
const workspace = { accountId: 'workspace', storedAccountId: 'workspace', userId: 'same-user' };
const liveAuth = (): string => path.join(codexHome, 'auth.json');

beforeEach(() => {
  temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-identity-'));
  process.env.CCS_HOME = temporary;
  codexHome = path.join(temporary, 'synthetic-codex');
  fs.mkdirSync(codexHome);
  registry = new CodexProfileRegistry();
  writeProfile('personal', syntheticAuth(personal));
  writeProfile('workspace', syntheticAuth(workspace));
  fs.writeFileSync(liveAuth(), syntheticAuth(personal), { mode: 0o600 });
  invalidateCodexAuthProfilesCache();
});

afterEach(() => {
  if (originalCcsHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = originalCcsHome;
  invalidateCodexAuthProfilesCache();
  fs.rmSync(temporary, { recursive: true, force: true });
});

describe('workspace and principal bound activation', () => {
  it('saves final rotated personal auth to personal rather than a same-email workspace target', async () => {
    const refreshed = syntheticAuth({ ...personal, rotation: 1 });
    const target = fs.readFileSync(profileAuth('workspace'));
    const events: string[] = [];
    const runtime = stubRuntime(events);
    runtime.stop = async () => {
      events.push('stop');
      fs.writeFileSync(liveAuth(), refreshed);
    };
    await activateCodexProfile('workspace', { registry, codexHome, runtime });
    expect(events).toEqual(['stop', 'start']);
    expect(fs.readFileSync(profileAuth('personal'))).toEqual(refreshed);
    expect(fs.readFileSync(profileAuth('workspace'))).toEqual(target);
    expect(fs.readFileSync(liveAuth())).toEqual(target);
  });

  it('separates different users with the same email and workspace', async () => {
    const otherUser = { ...personal, userId: 'other-user' };
    fs.writeFileSync(profileAuth('workspace'), syntheticAuth(otherUser));
    const target = fs.readFileSync(profileAuth('workspace'));
    const refreshed = syntheticAuth({ ...personal, rotation: 2 });
    fs.writeFileSync(liveAuth(), refreshed);
    await activateCodexProfile('workspace', {
      registry,
      codexHome,
      runtime: stubRuntime([]),
    });
    expect(fs.readFileSync(profileAuth('personal'))).toEqual(refreshed);
    expect(fs.readFileSync(profileAuth('workspace'))).toEqual(target);
    expect(fs.readFileSync(liveAuth())).toEqual(target);
  });

  it('refuses a same-email workspace change while waiting for the lock before stopping', async () => {
    const release = await acquireCodexActivationLock(codexHome);
    const events: string[] = [];
    const original = fs.readFileSync(liveAuth());
    const pending = activateCodexProfile('workspace', {
      registry,
      codexHome,
      runtime: stubRuntime(events),
    });
    fs.writeFileSync(
      profileAuth('workspace'),
      syntheticAuth({ ...workspace, accountId: 'changed', storedAccountId: 'changed' })
    );
    await release();
    await expect(pending).rejects.toThrow('changed account before activation');
    expect(events).toEqual([]);
    expect(fs.readFileSync(liveAuth())).toEqual(original);
  });

  it('refuses a same-email workspace change during stop and restarts the original', async () => {
    const original = fs.readFileSync(liveAuth());
    const changed = syntheticAuth({
      ...workspace,
      accountId: 'changed',
      storedAccountId: 'changed',
    });
    const events: string[] = [];
    const runtime = stubRuntime(events);
    runtime.stop = async () => {
      events.push('stop');
      fs.writeFileSync(profileAuth('workspace'), changed);
    };
    await expect(
      activateCodexProfile('workspace', { registry, codexHome, runtime })
    ).rejects.toThrow('changed account during activation');
    expect(events).toEqual(['stop', 'start']);
    expect(fs.readFileSync(liveAuth())).toEqual(original);
    expect(fs.readFileSync(profileAuth('workspace'))).toEqual(changed);
  });

  it('rolls back exact original auth if a restarted writer switches to another same-email workspace', async () => {
    const original = fs.readFileSync(liveAuth());
    const events: string[] = [];
    let starts = 0;
    const runtime = stubRuntime(events);
    runtime.start = async () => {
      events.push('start');
      if (++starts === 1) fs.writeFileSync(liveAuth(), syntheticAuth(personal));
    };
    await expect(
      activateCodexProfile('workspace', { registry, codexHome, runtime })
    ).rejects.toThrow('did not keep the requested account');
    expect(events).toEqual(['stop', 'start', 'stop', 'start']);
    expect(fs.readFileSync(liveAuth())).toEqual(original);
    expect(fs.existsSync(path.join(codexHome, '.ccs-activation.lock'))).toBe(false);
  });

  it('accepts rotated ID/access/refresh tokens and plan when strong identity stays the same', async () => {
    const refreshed = syntheticAuth({ ...workspace, rotation: 3 });
    const events: string[] = [];
    const runtime = stubRuntime(events);
    runtime.stop = async () => {
      events.push('stop');
      fs.writeFileSync(profileAuth('workspace'), refreshed);
    };
    runtime.start = async () => {
      events.push('start');
      fs.writeFileSync(liveAuth(), syntheticAuth({ ...workspace, rotation: 4 }));
    };
    const result = await activateCodexProfile('workspace', { registry, codexHome, runtime });
    expect(events).toEqual(['stop', 'start']);
    expect(fs.readFileSync(profileAuth('workspace'))).toEqual(refreshed);
    expect(fs.readFileSync(liveAuth())).toEqual(syntheticAuth({ ...workspace, rotation: 4 }));
    expect(result.plan).toBe('business');
  });

  it.each([
    ['missing JWT workspace', { ...workspace, accountId: undefined }],
    ['nonstring JWT workspace', { ...workspace, accountId: 42 }],
    ['conflicting workspace', { ...workspace, storedAccountId: 'different' }],
    ['empty stored workspace', { ...workspace, storedAccountId: '' }],
  ])('refuses %s before stopping or writing any auth', async (_label, identity) => {
    const original = fs.readFileSync(liveAuth());
    const invalid = syntheticAuth(identity);
    fs.writeFileSync(profileAuth('workspace'), invalid);
    const events: string[] = [];
    await expect(
      activateCodexProfile('workspace', { registry, codexHome, runtime: stubRuntime(events) })
    ).rejects.toMatchObject({ code: 'invalid_profile' });
    expect(events).toEqual([]);
    expect(fs.readFileSync(liveAuth())).toEqual(original);
    expect(fs.readFileSync(profileAuth('workspace'))).toEqual(invalid);
  });

  it('preserves JWT-only legacy IDs and prefers the requested true alias for refreshed live tokens', async () => {
    const legacy = { accountId: 'personal' };
    const original = syntheticAuth(legacy);
    const refreshed = syntheticAuth({ ...legacy, rotation: 5 });
    fs.writeFileSync(profileAuth('personal'), original);
    writeProfile('personal-alias', original);
    fs.writeFileSync(liveAuth(), refreshed);
    await activateCodexProfile('personal-alias', {
      registry,
      codexHome,
      runtime: stubRuntime([]),
    });
    expect(fs.readFileSync(profileAuth('personal-alias'))).toEqual(refreshed);
    expect(fs.readFileSync(profileAuth('personal'))).toEqual(original);
    expect(fs.readFileSync(liveAuth())).toEqual(refreshed);
  });

  it('retains a user binding learned under the lock through the after-stop reread', async () => {
    const legacy = syntheticAuth({ accountId: 'workspace' });
    fs.writeFileSync(profileAuth('workspace'), legacy);
    const release = await acquireCodexActivationLock(codexHome);
    const events: string[] = [];
    const runtime = stubRuntime(events);
    runtime.stop = async () => {
      events.push('stop');
      fs.writeFileSync(profileAuth('workspace'), legacy);
    };
    const pending = activateCodexProfile('workspace', { registry, codexHome, runtime });
    fs.writeFileSync(profileAuth('workspace'), syntheticAuth(workspace));
    await release();
    await expect(pending).rejects.toThrow('changed account during activation');
    expect(events).toEqual(['stop', 'start']);
    expect(fs.readFileSync(liveAuth())).toEqual(syntheticAuth(personal));
  });

  it('does not save an unknown live workspace or leak credential canaries', async () => {
    const original = syntheticAuth({ userId: 'same-user' });
    fs.writeFileSync(liveAuth(), original);
    const target = fs.readFileSync(profileAuth('workspace'));
    const savedPersonal = fs.readFileSync(profileAuth('personal'));
    const events: string[] = [];
    let failure: unknown;
    try {
      await activateCodexProfile('workspace', {
        registry,
        codexHome,
        runtime: stubRuntime(events),
      });
    } catch (error) {
      failure = error;
    }
    expect(failure).toMatchObject({ code: 'invalid_profile' });
    expect(String(failure)).not.toContain('synthetic-access');
    expect(String(failure)).not.toContain('synthetic-refresh');
    expect(events).toEqual(['stop', 'start']);
    expect(fs.readFileSync(liveAuth())).toEqual(original);
    expect(fs.readFileSync(profileAuth('personal'))).toEqual(savedPersonal);
    expect(fs.readFileSync(profileAuth('workspace'))).toEqual(target);
  });
});

/**
 * M3 regression: Home's Antigravity inventory comes from its native owner, so a saved registry
 * reaches Home exactly as it reaches GET /api/antigravity/profiles.
 *
 * The failing composition this guards: the TS Home adapter is still the answering path (the
 * shipping `AAC_RUST_HOME_VIEW` switch unset, the native Home group off so the fed answer
 * declines), and the saved registry is not empty. Before the fix the retained adapter answered an
 * unconditional empty Ubuntu inventory and the frame replaced its Antigravity state with it.
 *
 * Linux only (the Antigravity runtime exists there); elsewhere it prints a skip and exits 0.
 *
 *   bun tests/spec/home-antigravity-owner.ts --binary <aac-http>
 *
 * Fixture data only, in a temporary CCS home; read-only probes against the compiled service.
 */
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import express from 'express';
import { RustService } from '../../src/web-server/rust-service';
import {
  createHomeViewRouter,
  readAntigravityInventory,
  readClaudeDesktopProfiles,
  runtimeInventory,
} from '../../src/web-server/routes/home-view-routes';

const here = path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
const repo = path.resolve(here, '../..');
const args = process.argv.slice(2);
const option = (name: string, fallback: string) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 && args[i + 1] ? args[i + 1] : fallback;
};

if (process.platform !== 'linux') {
  console.log('[SKIP] home-antigravity-owner: the Antigravity runtime exists on Linux only.');
  process.exit(0);
}

const binary = path.resolve(option('binary', path.join(repo, 'target/debug/aac-http')));
if (!fs.existsSync(binary)) throw new Error(`needs the built aac-http at ${binary}`);

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-home-owner-'));
const home = path.join(root, 'home');
fs.mkdirSync(home, { recursive: true });
fs.chmodSync(root, 0o700);
fs.chmodSync(home, 0o700);

// One ordinary saved profile: the registry the native owner reads.
const identity = {
  email: 'gmail@example.com',
  subject: 'fixture-authoritative-subject-gmail',
  plan: 'Google AI Pro',
  verifiedAt: '2026-10-01T16:40:00.000Z',
  source: 'provider-userinfo',
};
const credential = Buffer.from(
  JSON.stringify({
    fixtureAccount: 'gmail',
    version: 1,
    private: 'FIXTURE_ONLY_PRIVATE_NATIVE_BYTES',
  })
);
const fingerprint = createHash('sha256').update(credential).digest('hex');
const identityKey = createHash('sha256')
  .update(JSON.stringify(['antigravity', identity.email.trim().toLowerCase(), identity.subject]))
  .digest('hex');
fs.mkdirSync(path.join(root, 'antigravity-profiles'), { recursive: true });
fs.writeFileSync(
  path.join(root, 'antigravity-profiles/registry-000000000001.json'),
  JSON.stringify({
    version: 1,
    revision: 1,
    profiles: [
      {
        id: 'gmail',
        email: identity.email,
        identityKey,
        plan: identity.plan,
        credential: {
          fingerprint,
          format: 'fake-native-json',
          identity,
          savedAt: identity.verifiedAt,
        },
      },
    ],
    active: { profileId: 'gmail', verifiedAt: identity.verifiedAt },
    transaction: null,
  }),
  { mode: 0o600 }
);
// One saved Claude desktop profile, so the profiles read has a row whose Open field the owner stamps.
fs.writeFileSync(
  path.join(root, 'claude-desktop-profiles.json'),
  JSON.stringify({
    version: 1,
    profiles: [
      {
        id: 'fixture-a',
        email: 'user0@example.invalid',
        mac: {
          launcherName: 'Claude 0',
          launcherPath: '/Applications/Claude 0.app',
          profilePath: '/Users/fixture/Library/Application Support/Claude-0',
          sshHost: 'mac',
        },
      },
    ],
  }),
  { mode: 0o600 }
);
const instance = path.join(root, 'antigravity-instances/gmail/ubuntu');
fs.mkdirSync(instance, { recursive: true });
fs.writeFileSync(path.join(instance, `credential-${fingerprint}.bin`), credential, { mode: 0o600 });

const ccs = process.env.CCS_DIR;
process.env.CCS_DIR = root;
process.env.HOME = home;
// The packaged scripts, or the repo's (the binary may live in a target directory).
process.env.AAC_ANTIGRAVITY_SCRIPTS ??= path.join(repo, 'scripts');
// The retained TS adapter answers: the fed group is off, and the route is offered through the
// view switch (the same retained branch the shipping default-ready group reaches after a fed decline).
process.env.AAC_RUST_HOME_VIEW = 'on';
process.env.AAC_RUST_GROUP_HOME = 'off';
// The public Open owner is bound: its progress rides the profiles read into Home.
process.env.AAC_RUST_GROUP_CLAUDE_OPEN = 'on';

const service = new RustService('on');
const dashboard = {
  schemaVersion: 1,
  updatedAt: new Date().toISOString(),
  accounts: [],
  settings: { refreshIntervalSeconds: 60 },
  server: { version: '0.0.0-fixture', commit: null },
};

let server: http.Server | undefined;
const failures: string[] = [];
try {
  await service.start(binary);
  if (!service.lease) throw new Error('the Rust service did not start');

  const app = express();
  app.use((req: express.Request, res: express.Response, next: express.NextFunction) => {
    Object.assign(req, { session: { authenticated: true }, sessionID: 'fixture-owner-session' });
    res.locals.rustService = service;
    next();
  });
  app.use(
    '/api/views',
    createHomeViewRouter({
      getDashboard: async () => structuredClone(dashboard) as never,
      readProfiles: async () => [],
      scope: () => root,
    })
  );
  // The profiles route (as the cut composes it) and the owner's raw read, both over real requests.
  app.get('/api/antigravity/profiles', async (req: express.Request, res: express.Response) => {
    res.json(await readAntigravityInventory(req, res));
  });
  app.get('/api/antigravity/owner', async (req: express.Request, res: express.Response) => {
    res.json(await runtimeInventory(req, res).catch(() => null));
  });
  app.get('/api/claude/desktop-profiles', async (req: express.Request, res: express.Response) => {
    res.json(await readClaudeDesktopProfiles(req, res).catch(() => null));
  });
  server = await new Promise<http.Server>((resolve) => {
    const started = app.listen(0, '127.0.0.1', () => resolve(started));
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('no test port');
  const base = `http://127.0.0.1:${address.port}`;
  // The owner's own read (the profiles route in the cut): one saved profile.
  const owner = (await (await fetch(`${base}/api/antigravity/owner`)).json()) as {
    profiles?: { id?: string }[];
  };
  if (owner.profiles?.length !== 1) failures.push(`owner read: ${owner.profiles?.length} profiles`);
  else if (owner.profiles[0].id !== 'gmail') failures.push(`owner read: ${owner.profiles[0].id}`);

  const reply = await fetch(`${base}/api/views/home?platform=mac`);
  if (reply.status !== 200) failures.push(`Home answered ${reply.status}`);
  const body = (await reply.json()) as { antigravity?: { profiles?: { id?: string }[] } };
  const homeProfiles = body.antigravity?.profiles ?? [];
  if (homeProfiles.length !== 1) failures.push(`Home: ${homeProfiles.length} Antigravity profiles`);
  else if (homeProfiles[0].id !== 'gmail') failures.push(`Home: ${homeProfiles[0].id}`);

  // The Open owner answered: each id-bearing profile row carries its field (none running here).
  const profiles = (await (await fetch(`${base}/api/claude/desktop-profiles`)).json()) as {
    id?: string;
    openOperation?: unknown;
  }[];
  if (profiles.length !== 1) failures.push(`profiles: ${profiles.length} rows`);
  else if (!('openOperation' in profiles[0])) failures.push('profiles: the Open field is missing');
  else if (profiles[0].openOperation !== null)
    failures.push(`profiles: ${JSON.stringify(profiles[0].openOperation)}`);

  // The composed copy the profiles route answers must agree with Home's.
  const copy = await (await fetch(`${base}/api/antigravity/profiles`)).json();
  if (JSON.stringify(copy) !== JSON.stringify(body.antigravity))
    failures.push('Home and the profiles route disagree');

  // A service that cannot answer is unavailable (null), never the empty inventory.
  await service.stop();
  const gone = await fetch(`${base}/api/views/home?platform=mac`);
  const goneBody = (await gone.json()) as { antigravity?: unknown };
  if (goneBody.antigravity !== null)
    failures.push(`unavailable Home: ${JSON.stringify(goneBody.antigravity)}`);
} finally {
  if (server) await new Promise<void>((resolve) => server?.close(() => resolve()));
  process.env.CCS_DIR = ccs;
  fs.rmSync(root, { recursive: true, force: true });
  process.env.AAC_RUST_GROUP_HOME = undefined;
}
if (failures.length) {
  console.error('[FAIL] home-antigravity-owner: ' + failures.join('; '));
  process.exit(1);
}
console.log(
  '[OK] home-antigravity-owner: the saved profile reaches Home and the profiles route; unavailable stays null.'
);

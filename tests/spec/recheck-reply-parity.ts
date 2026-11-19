/**
 * One per-account Re-check's TS reply (`recheck` in account-lifecycle-extras.ts): the
 * exact `account` object, the row that is stored before the ready lifecycle is added,
 * and the two route outcomes the native sequencer must mirror (an unknown account
 * answers 404 and publishes nothing). The 10 s route limit is TS route admission; it
 * is recorded here but stays with the route, not the in-process sequencer.
 *
 * The Rust side is `lifecycle_recheck::account` (`cargo test -p aac-http` on Nas1).
 *
 *   bun tests/spec/recheck-reply-parity.ts               (verify against the golden)
 *   bun tests/spec/recheck-reply-parity.ts --regenerate  (rewrite it; review the diff)
 */
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { recheck } from '../../src/web-server/services/account-lifecycle-extras';
import type { LifecycleEnv } from '../../src/web-server/services/account-lifecycle-env';

const repo = fileURLToPath(new URL('../..', import.meta.url));
const GOLDEN = path.join(repo, 'crates/aac-http/tests/fixtures/recheck-replies.json');
const NOW = Date.parse('2026-10-02T12:00:00Z');

const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-recheck-')));
const ccsDir = path.join(root, '.ccs');
fs.mkdirSync(ccsDir, { recursive: true, mode: 0o700 });
const registry = path.join(ccsDir, 'account-usage-accounts.json');
fs.writeFileSync(
  registry,
  JSON.stringify({
    version: 2,
    accounts: [
      {
        id: 'zai:usage',
        provider: 'zai',
        platform: 'ubuntu',
        sshHost: null,
        label: null,
        credential: { kind: 'discover' },
        createdAt: null,
        createdBy: 'migration',
      },
      {
        id: 'cursor:usage',
        provider: 'cursor',
        platform: 'ubuntu',
        sshHost: null,
        label: null,
        credential: { kind: 'discover' },
        createdAt: null,
        createdBy: 'migration',
      },
      {
        id: 'kimi-code:usage',
        provider: 'kimi-code',
        platform: 'ubuntu',
        sshHost: null,
        label: null,
        credential: { kind: 'discover' },
        createdAt: null,
        createdBy: 'migration',
      },
    ],
  })
);
fs.chmodSync(registry, 0o600);

const row = (id: string, provider: string, status: string) => ({
  id,
  provider,
  providerLabel: provider === 'zai' ? 'Z.ai coding plan' : 'Cursor',
  label: provider === 'zai' ? 'Z.ai' : 'Cursor',
  email: null,
  plan: null,
  platform: 'ubuntu',
  source: 'fixture',
  status,
  ...(status === 'cached' ? { cacheReason: 'read_failed' } : {}),
  message: status === 'cached' ? 'The last successful reading is shown.' : null,
  fetchedAt: '2026-10-02T11:55:00.000Z',
  sampledAt: '2026-10-02T11:55:00.000Z',
  isActive: false,
  windows: [],
  capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
});

async function envFor(rows: Record<string, unknown>) {
  const replaced: unknown[] = [];
  const env = {
    ccsDir: () => ccsDir,
    now: () => NOW,
    refreshAdditional: async (id: string) =>
      id in rows ? structuredClone(rows[id]) : null,
    replaceRow: (account: unknown) => replaced.push(account),
  } as unknown as LifecycleEnv;
  return { env, replaced };
}

const cases: Array<Record<string, unknown>> = [];
for (const [name, id, status] of [
  ['zai-cached-row', 'zai:usage', 'cached'],
  ['cursor-ok-row', 'cursor:usage', 'ok'],
] as const) {
  const account = row(id, id.split(':')[0], status);
  const { env, replaced } = await envFor({ [id]: account });
  const result = await recheck(env, id, {});
  cases.push({
    name,
    id,
    reply: result.body,
    stored: replaced[0] ?? null,
  });
}

// An account the current registry does not name: the route answers 404 and stores nothing.
{
  const { env, replaced } = await envFor({});
  try {
    await recheck(env, 'zai:acct:00000000', {});
    throw new Error('an unknown account answered');
  } catch (error) {
    const failure = error as { status: number; code: string };
    cases.push({
      name: 'unknown-account',
      id: 'zai:acct:00000000',
      outcome: { status: failure.status, code: failure.code },
      stored: replaced[0] ?? null,
    });
  }
}

// The route's own 10 s admission: one account, then a 429 with the retry in seconds.
{
  const account = row('kimi-code:usage', 'kimi-code', 'ok');
  const { env } = await envFor({ 'kimi-code:usage': account });
  await recheck(env, 'kimi-code:usage', {});
  try {
    await recheck(env, 'kimi-code:usage', {});
    throw new Error('the route limit did not apply');
  } catch (error) {
    const failure = error as { status: number; code: string; extra: Record<string, unknown> };
    cases.push({
      name: 'route-limit',
      id: 'kimi-code:usage',
      outcome: {
        status: failure.status,
        code: failure.code,
        retryAfterSeconds: failure.extra.retryAfterSeconds,
      },
    });
  }
}

const fixture = { version: 1, now: NOW, cases };
const text = `${JSON.stringify(fixture, null, 2)}\n`;
if (process.argv.includes('--regenerate')) {
  fs.mkdirSync(path.dirname(GOLDEN), { recursive: true });
  fs.writeFileSync(GOLDEN, text);
  console.log(`wrote ${path.relative(repo, GOLDEN)} (${cases.length} cases)`);
} else {
  const current = fs.existsSync(GOLDEN) ? fs.readFileSync(GOLDEN, 'utf8') : '';
  if (current !== text) {
    console.error(
      `${path.relative(repo, GOLDEN)} differs from the TS reply; rerun with --regenerate and review the diff.`
    );
    process.exit(1);
  }
  console.log(`TS Re-check replies match the golden (${cases.length} cases)`);
}

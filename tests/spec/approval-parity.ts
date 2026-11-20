/** Shared lifecycle approval contract with the real TypeScript store and private Rust service. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const tsOnly = process.argv.includes('--ts-only');
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-approval-parity-'));
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { mode: 0o700 });
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: home,
  CCS_DIR: ccs,
  AAC_RUST_GROUP_WRITES: 'on',
  AAC_STORE: 'off',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_NO_LOCAL_RUST_BUILD: '1',
});

const { RustService } = await import('../../src/web-server/rust-service');
const { AccountConfirmationStore, sessionKey, stateFingerprint, CONFIRMATION_TTL_MS } =
  await import('../../src/web-server/services/account-confirmations');
import type {
  ConfirmationBinding,
  IssuedConfirmation,
} from '../../src/web-server/services/account-confirmations';

interface ApprovalStore {
  issue(binding: ConfirmationBinding): IssuedConfirmation | Promise<IssuedConfirmation>;
  consume(token: string, binding: ConfirmationBinding): boolean | Promise<boolean>;
}

async function check(store: ApprovalStore) {
  const base = {
    action: 'remove',
    subject: 'codex:fixture@example.com',
    sessionKey: sessionKey('fixture-session-a'),
    stateFingerprint: stateFingerprint({ state: 'reviewed' }),
  } satisfies ConfirmationBinding;
  const start = Date.now();
  const { token, expiresAt } = await store.issue(base);
  assert.match(token, /^[A-Za-z0-9_-]{43}$/);
  const issued = Date.parse(expiresAt) - CONFIRMATION_TTL_MS;
  assert.ok(issued >= start && issued <= Date.now());
  assert.equal(await store.consume(token, base), true);
  assert.equal(await store.consume(token, base), false);

  for (const altered of [
    { ...base, action: 'trash-restore' as const },
    { ...base, subject: 'codex:another-fixture@example.com' },
    { ...base, sessionKey: sessionKey('fixture-session-b') },
    { ...base, stateFingerprint: stateFingerprint({ state: 'changed' }) },
  ]) {
    const denied = await store.issue(base);
    assert.equal(await store.consume(denied.token, altered), false);
    assert.equal(await store.consume(denied.token, base), false);
  }

  const other = { ...base, sessionKey: sessionKey('fixture-session-b') };
  const kept = await store.issue(other);
  const first = await store.issue(base);
  for (let index = 0; index < 8; index++) await store.issue(base);
  assert.equal(await store.consume(first.token, base), false);
  assert.equal(await store.consume(kept.token, other), true);

  const oldest = await store.issue(other);
  for (let index = 0; index < 32; index++)
    await store.issue({ ...base, sessionKey: sessionKey(`fixture-${index}`) });
  assert.equal(await store.consume(oldest.token, other), false);
  assert.equal(await store.consume('invalid', base), false);
}

const service = new RustService('on');
try {
  await check(new AccountConfirmationStore());
  if (!tsOnly) {
    await service.start(
      process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http')
    );
    assert.equal(service.stats.fallbacks, 0);
    const store = service.accountConfirmations();
    assert.ok(store, 'Rust ownership must be active');
    await check(store);
    assert.ok(service.stats.served > 0, 'the fixture must reach the real Rust issuer');
    assert.equal(service.stats.fallbacks, 0);
  }
  console.log(
    `account approval parity: one-shot, four bindings, expiry and both caps (${tsOnly ? 'TS' : 'TS/Rust'})`
  );
} finally {
  service.stop();
  fs.rmSync(home, { recursive: true, force: true });
}

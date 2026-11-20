// G6 quota oracle: run the real TypeScript Antigravity runtime (composition + usage service +
// cooldown) on the committed fixtures with a scripted quota worker, and print each step's accounts
// and the public quotas answer. Read-only: a temporary CCS root, invented credential bytes, a fake
// driver, and a collector that only returns the fixture's snapshots.
//
// Run: bun tests/parity/g6/quota-oracle.mjs > tests/parity/g6/quota-expected.json
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { AntigravityProfileRegistry, identityKey } from '../../../src/antigravity/registry';
import { createAntigravityRuntime } from '../../../src/antigravity/runtime-composition';
import { publicQuotas } from '../../../src/antigravity/public-reads';

const here = path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
const spec = JSON.parse(readFileSync(path.join(here, 'quota-cases.json'), 'utf8'));
const registryFixture = spec.registry;

function materialize(item) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'aac-g6-quota-'));
  const profilesDirectory = path.join(root, 'antigravity-profiles');
  mkdirSync(profilesDirectory, { recursive: true });
  const records = item.profiles.map((profile) => {
    const bytes = Buffer.from(profile.credential.bytes, 'base64');
    const fingerprint = createHash('sha256').update(bytes).digest('hex');
    const directory = path.join(root, 'antigravity-instances', profile.id, 'ubuntu');
    mkdirSync(directory, { recursive: true });
    writeFileSync(path.join(directory, `credential-${fingerprint}.bin`), bytes);
    return {
      id: profile.id,
      email: profile.email,
      identityKey: identityKey(profile.identity),
      plan: profile.plan,
      credential: {
        fingerprint,
        format: profile.credential.format,
        identity: profile.identity,
        savedAt: profile.identity.verifiedAt,
      },
    };
  });
  writeFileSync(
    path.join(profilesDirectory, 'registry-000000000001.json'),
    JSON.stringify({
      version: 1,
      revision: 1,
      profiles: records,
      active: item.active,
      transaction: null,
    })
  );
  return root;
}

function snapshotFor(profile, sample, at) {
  const bytes = Buffer.from(profile.credential.bytes, 'base64');
  return {
    profileId: profile.id,
    identityKey: sample.identityKey === 'other' ? 'f'.repeat(64) : profile.identityKey,
    credentialRevision: createHash('sha256').update(bytes).digest('hex'),
    email: profile.email,
    plan: sample.plan ?? null,
    status: sample.status,
    fetchedAt: sample.sampledAt ?? at,
    sampledAt: sample.sampledAt ?? at,
    windows: sample.windows ?? [],
    ...(sample.retryAfterSeconds !== undefined
      ? { retryAfterSeconds: sample.retryAfterSeconds }
      : {}),
    ...(sample.reportedPlan ? { reportedPlan: sample.reportedPlan } : {}),
    source: sample.source,
    identityVerified: sample.identityVerified,
    identityValidation: sample.identityValidation,
  };
}

async function run() {
  const answers = [];
  for (const testCase of spec.cases) {
    const ccs = materialize(registryFixture);
    const registry = new AntigravityProfileRegistry(ccs, { readOnly: true });
  // The same identities the Rust fixture uses, resolved through the registry.
  const profiles = registry.listProfiles().map((profile) => {
    const saved = registry.readCredential(profile.id, 'ubuntu');
    return {
      ...profile,
      credential: { bytes: saved.credential.bytes, format: saved.credential.format },
    };
  });
  const live = registryFixture.live;
  let step = null;
  let at = Date.parse(registryFixture.now);
  const forbidden = async (name) => {
    throw new Error(`Unexpected native action in the quota oracle: ${name}`);
  };
  const collectQuota = async (request) => {
    const sample = step.samples[request.profileId];
    if (!sample) throw new Error('The fixture worker did not answer this profile.');
    const profile = profiles.find((entry) => entry.id === request.profileId);
    return snapshotFor(profile, sample, step.at);
  };
  const driver = {
    hostId: 'ubuntu',
    canProveRuntimeIdentity: async () => false,
    readInventoryIdentity: async () => live,
    readCurrentCredential: () => forbidden('readCurrentCredential'),
    validateCredential: () => forbidden('validateCredential'),
    inspectProcesses: () => forbidden('inspectProcesses'),
    stopProcesses: () => forbidden('stopProcesses'),
    installCredential: () => forbidden('installCredential'),
    readStoredIdentity: () => forbidden('readStoredIdentity'),
    restartProcesses: () => forbidden('restartProcesses'),
    proveRuntimeIdentity: () => forbidden('proveRuntimeIdentity'),
    rollbackCredential: () => forbidden('rollbackCredential'),
    stopOwnedRestarts: () => forbidden('stopOwnedRestarts'),
  };
  const runtime = createAntigravityRuntime({
    registry,
    ccsDirectory: ccs,
    driver,
    collectQuota,
    now: () => at,
  });
    const steps = [];
    for (const current of testCase.steps) {
      step = current;
      at = Date.parse(current.at);
      const accounts = await runtime.getAccounts({ refresh: current.refresh });
      steps.push({ at: current.at, accounts, quotas: publicQuotas(accounts, at) });
    }
    answers.push({ name: testCase.name, steps });
  }
  // `publicQuotas` runs `antigravityPlanDisplay` with no sampled time through Date.now(); record it
  // so the parity test uses the same clock the answers were computed at.
  process.stdout.write(JSON.stringify({ clock: Date.now(), cases: answers }, null, 2));
}

await run();

// G6 parity oracle: run the TypeScript Antigravity runtime on the committed fixture cases and print
// what it answers, so the Rust port can assert the same bytes. Read-only: a temporary CCS root,
// invented credential bytes, a fake driver that performs no native action, and no provider call.
//
// Run (bun or node with --experimental-strip-types):  bun tests/parity/g6/oracle.mjs > expected.json
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { readFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {
  AntigravityProfileRegistry,
  identityKey,
} from '../../../src/antigravity/registry';
import { AntigravitySwitchService } from '../../../src/antigravity/switch-service';
import { createAntigravityRuntime } from '../../../src/antigravity/runtime-composition';

const here = path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
const cases = JSON.parse(readFileSync(path.join(here, 'cases.json'), 'utf8'));
const now = Date.parse(cases.now);

function materialize(item) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'aac-g6-oracle-'));
  const profilesDirectory = path.join(root, 'antigravity-profiles');
  mkdirSync(profilesDirectory, { recursive: true });
  const records = item.profiles.map((profile) => {
    const bytes = Buffer.from(profile.credential.bytes, 'base64');
    const fingerprint = createHash('sha256').update(bytes).digest('hex');
    if (!item.omitCredentials) {
      const directory = path.join(root, 'antigravity-instances', profile.id, 'ubuntu');
      mkdirSync(directory, { recursive: true });
      writeFileSync(path.join(directory, `credential-${fingerprint}.bin`), bytes);
    }
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

async function report(item) {
  const root = materialize(item);
  const registry = new AntigravityProfileRegistry(root, { readOnly: true });
  const forbidden = async (name) => {
    throw new Error(`Unexpected native action in the fixture oracle: ${name}`);
  };
  const driver = {
    hostId: 'ubuntu',
    canProveRuntimeIdentity: async () => false,
    readInventoryIdentity: item.live ? async () => item.live : undefined,
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
  const switcher = new AntigravitySwitchService({ registry, driver, now: () => now });
  const runtime = createAntigravityRuntime({
    registry,
    driver,
    now: () => now,
    collectQuota: () => forbidden('collectQuota'),
    readNativeUpdatePaused: async () => item.paused ?? null,
    readRuntimeServiceProblem: async () => item.problem ?? null,
  });
  return {
    name: item.name,
    profiles: await switcher.readInventory(),
    inventory: await runtime.getInventory(),
    selectedProfileId: await runtime.readSelectedProfileId(),
  };
}

const answers = [];
for (const item of cases.cases) answers.push(await report(item));
process.stdout.write(JSON.stringify(answers, null, 2));

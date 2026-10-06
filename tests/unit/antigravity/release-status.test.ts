/**
 * The read-only switching readiness report over temporary folders: every
 * gate is read from fixtures; nothing is installed, started or written.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import net from 'net';
import os from 'os';
import path from 'path';
import {
  formatAntigravityReleaseStatus,
  readAntigravityReleaseStatus,
  type ReleaseStatusDeps,
} from '../../../src/antigravity/release-status';
import { AntigravityProfileRegistry } from '../../../src/antigravity/registry';
import { AntigravityAutoSwitchFileStore } from '../../../src/antigravity/auto-switch/store';
import type { AntigravityRuntimeServiceProblem } from '../../../src/antigravity/usage-contract';

const PIN = 'a759ce7c7a235d9b6c281a25ead97cbbf2e92314a3ffd224e2f9144f3fae7a86';
let root: string;
let ccsDir: string;
let home: string;
let releaseFile: string;

beforeEach(() => {
  root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-release-')));
  ccsDir = path.join(root, '.ccs');
  home = path.join(root, 'home');
  fs.mkdirSync(ccsDir, { mode: 0o700 });
  fs.mkdirSync(path.join(home, '.local', 'bin'), { recursive: true });
  releaseFile = path.join(root, 'release.json');
  writeRelease(false);
});

afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

function writeRelease(open: boolean): void {
  fs.writeFileSync(
    releaseFile,
    JSON.stringify({
      schemaVersion: 1,
      nativeActivationReleased: open,
      nativeVersion: '1.2.16',
      nativeSha256: PIN,
      nativeProofReceiptSha256: open ? 'a'.repeat(64) : null,
      reviewedNatives: [
        {
          nativeVersion: '1.2.14',
          nativeSha256: '0d0d3eba22daf29504dd290151c7ed9a4d33b0c6aa0acfc5da27bc3b01d2f029',
        },
        { nativeVersion: '1.2.16', nativeSha256: PIN },
      ],
    })
  );
}

function installAgy(): void {
  const binary = path.join(home, '.local', 'bin', 'agy');
  fs.writeFileSync(binary, '#!/bin/sh\nexit 0\n');
  fs.chmodSync(binary, 0o755);
}

async function save(id: string): Promise<void> {
  const registry = new AntigravityProfileRegistry(ccsDir);
  const bytes = Buffer.from(JSON.stringify({ auth_method: 'consumer', id }));
  await registry.withLock(async () =>
    registry.saveCredential(
      id,
      'ubuntu',
      { format: 'antigravity-consumer-json', bytes },
      {
        email: `${id}@example.com`,
        subject: `subject-${id}`,
        plan: null,
        verifiedAt: new Date().toISOString(),
        source: 'provider-userinfo',
      },
      Date.now()
    )
  );
}

function read(extra: Partial<ReleaseStatusDeps> = {}) {
  return readAntigravityReleaseStatus({
    ccsDir,
    home,
    releaseFile,
    dashboardGate: false,
    pinMatches: () => true,
    // Hermetic: no interpreter or systemctl is ever spawned by these fixtures.
    diagnoseService: () => null,
    ...extra,
  });
}

const MISSING_PYTE: AntigravityRuntimeServiceProblem = {
  reason: 'missing-python-module',
  module: 'pyte',
  python: '3.14',
  builtFor: '3.13',
  exitStatus: null,
};

async function adoptedWithoutService(): Promise<string> {
  installAgy();
  await save('gmail');
  await save('party');
  writeRelease(true);
  const state = installRuntime();
  fs.writeFileSync(path.join(state, 'runtime-adoption.json'), '{"phase":"adopted"}');
  return state;
}

function installRuntime(): string {
  const bundle = path.join(
    home,
    '.local/share/ai-account-center/antigravity-runtime/bundles',
    'b'.repeat(64)
  );
  fs.mkdirSync(bundle, { recursive: true, mode: 0o700 });
  const state = path.join(ccsDir, 'antigravity-switching');
  fs.mkdirSync(state, { mode: 0o700 });
  fs.chmodSync(state, 0o700);
  const descriptor = path.join(state, 'runtime-installation.json');
  fs.writeFileSync(
    descriptor,
    JSON.stringify({
      schemaVersion: 1,
      bundleDirectory: bundle,
      nativeBinary: path.join(home, '.local/bin/agy'),
      nativeSha256: PIN,
      socketPath: path.join(ccsDir, 'antigravity-runtime/control.sock'),
    }),
    { mode: 0o600 }
  );
  fs.chmodSync(descriptor, 0o600);
  return state;
}

describe('Antigravity switching readiness', () => {
  it('walks the gates in order and names the next step each time', async () => {
    expect(read()).toMatchObject({ nativeCli: 'missing', profiles: [] });
    expect(read().nextStep).toContain('Install the Antigravity CLI');
    installAgy();
    await save('gmail');
    expect(read().nextStep).toContain('Save a second profile');
    await save('party');
    expect(read({ pinMatches: () => false })).toMatchObject({ nativeCli: 'changed' });
    expect(read({ pinMatches: () => false }).nextStep).toContain(
      'not the reviewed build; switching is paused until the runtime is reviewed'
    );
    const closed = read();
    expect(closed).toMatchObject({
      nativeCli: 'pinned',
      dashboardGateOpen: false,
      runtimeGateOpen: false,
      runtimeInstalled: false,
      adoption: 'none',
      serviceSocket: false,
      profiles: [
        { id: 'gmail', email: 'gmail@example.com', runtimeVerifiedActive: false },
        { id: 'party', email: 'party@example.com', runtimeVerifiedActive: false },
      ],
      automaticSwitching: { enabled: false, thresholdUsedPercent: 95, requestedPoolId: null },
    });
    expect(closed.nextStep).toContain('Open both release gates');
    writeRelease(true);
    expect(read().nextStep).toContain('Open both release gates');
    expect(read({ dashboardGate: true }).nextStep).toContain('install_runtime.py --apply');
    const state = installRuntime();
    expect(read({ dashboardGate: true })).toMatchObject({ runtimeInstalled: true });
    expect(read({ dashboardGate: true }).nextStep).toContain('adopt_runtime.py (step 4)');
    fs.writeFileSync(path.join(state, 'runtime-adoption.json'), '{"phase":"recovery-required"}');
    expect(read({ dashboardGate: true }).nextStep).toContain('did not finish');
    fs.writeFileSync(path.join(state, 'runtime-adoption.json'), '{"phase":"adopted"}');
    expect(read({ dashboardGate: true }).nextStep).toContain('Start ai-account-center');
    const store = new AntigravityAutoSwitchFileStore(state);
    store.write({ ...store.read(), settings: { ...store.read().settings, enabled: true } });
    expect(read({ dashboardGate: true }).automaticSwitching?.enabled).toBe(true);
  });

  it('names a runtime service that cannot load its parser and never advises starting it', async () => {
    await adoptedWithoutService();
    const seen: string[] = [];
    const status = read({
      dashboardGate: true,
      diagnoseService: (bundle) => {
        seen.push(bundle);
        return MISSING_PYTE;
      },
    });
    expect(seen).toEqual([
      path.join(home, '.local/share/ai-account-center/antigravity-runtime/bundles', 'b'.repeat(64)),
    ]);
    expect(status.serviceSocket).toBe(false);
    expect(status.serviceProblem).toEqual(MISSING_PYTE);
    expect(status.nextStep).toStartWith('Runtime service failed: missing Python module pyte');
    expect(status.nextStep).toContain('rebuild_bundle.py --plan, then --apply');
    expect(status.nextStep).toContain('Starting the service again will fail');
    expect(status.nextStep).toContain('without a restart');
    expect(status.nextStep).not.toContain('restart ccs-dashboard');
    expect(status.nextStep).not.toContain('Start ai-account-center-antigravity.service');
    const text = formatAntigravityReleaseStatus(status).join('\n');
    expect(text).toContain(
      'Runtime service:     failed: missing Python module pyte (system Python is 3.14; the runtime bundle was built for 3.13)'
    );
    expect(text).not.toContain('not running');
  });

  it('names a failed unit without a parser cause and points at its journal', async () => {
    await adoptedWithoutService();
    const status = read({
      dashboardGate: true,
      diagnoseService: () => ({
        reason: 'service-failed',
        module: null,
        python: null,
        builtFor: null,
        exitStatus: 1,
      }),
    });
    expect(status.nextStep).toStartWith('Runtime service failed: exit status 1.');
    expect(status.nextStep).toContain('journalctl --user -u ai-account-center-antigravity.service');
    expect(status.nextStep).not.toContain('Start ai-account-center-antigravity.service');
    expect(formatAntigravityReleaseStatus(status).join('\n')).toContain(
      'Runtime service:     failed: exit status 1'
    );
  });

  it('keeps the start advice when nothing is known and checks nothing while the socket exists', async () => {
    const state = await adoptedWithoutService();
    const unknown = read({ dashboardGate: true });
    expect(unknown.serviceProblem).toBeNull();
    expect(unknown.nextStep).toContain('Start ai-account-center-antigravity.service');
    expect(formatAntigravityReleaseStatus(unknown).join('\n')).toContain(
      'Runtime service:     not running'
    );
    const throwing = read({
      dashboardGate: true,
      diagnoseService: () => {
        throw new Error('synthetic probe failure');
      },
    });
    expect(throwing.serviceProblem).toBeNull();
    const runtime = path.join(ccsDir, 'antigravity-runtime');
    fs.mkdirSync(runtime, { mode: 0o700 });
    const server = net.createServer();
    await new Promise<void>((resolve) =>
      server.listen(path.join(runtime, 'control.sock'), resolve)
    );
    try {
      let probes = 0;
      const running = read({
        dashboardGate: true,
        diagnoseService: () => {
          probes++;
          return MISSING_PYTE;
        },
      });
      expect(probes).toBe(0);
      expect(running.serviceSocket).toBe(true);
      expect(running.serviceProblem).toBeNull();
      expect(running.nextStep).toContain('Manual switching can be tested');
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
    expect(fs.existsSync(state)).toBe(true);
  });

  it('pins any reviewed build and names the reviewed set', () => {
    installAgy();
    const either = (match: string) => ({
      pinMatches: (binary: string, sha: string) => sha === match,
    });
    expect(read(either(PIN))).toMatchObject({
      nativeCli: 'pinned',
      reviewedNativeVersions: ['1.2.14', '1.2.16'],
    });
    expect(
      read(either('0d0d3eba22daf29504dd290151c7ed9a4d33b0c6aa0acfc5da27bc3b01d2f029'))
    ).toMatchObject({ nativeCli: 'pinned' });
    expect(read(either('e'.repeat(64)))).toMatchObject({ nativeCli: 'changed' });
    expect(formatAntigravityReleaseStatus(read(either(PIN))).join('\n')).toContain(
      'reviewed 1.2.14 / 1.2.16 build'
    );
  });

  it('only reads: an empty profiles folder gains no credential folder', () => {
    fs.mkdirSync(path.join(ccsDir, 'antigravity-profiles'), { mode: 0o700 });
    expect(read().profiles).toEqual([]);
    expect(fs.readdirSync(ccsDir)).toEqual(['antigravity-profiles']);
  });

  it('formats one line per gate without secrets', async () => {
    installAgy();
    await save('gmail');
    await save('party');
    const lines = formatAntigravityReleaseStatus(read());
    expect(lines[0]).toBe('Antigravity switching on Ubuntu');
    expect(lines.join('\n')).toContain(
      'Saved profiles:      gmail (gmail@example.com), party (party@example.com)'
    );
    expect(lines.join('\n')).toContain('Dashboard gate:      closed');
    expect(lines.join('\n')).not.toContain('subject-');
    expect(lines.join('\n')).not.toContain('consumer');
  });
});

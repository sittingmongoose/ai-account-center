import fs from 'fs';
import path from 'path';
import { AntigravityAutoSwitchFileStore } from './auto-switch/store';
import {
  ANTIGRAVITY_NATIVE_RELEASED,
  readInstalledAntigravityRuntime,
  verifyOwnedAntigravityNativePin,
} from './production-runtime';
import { AntigravityProfileRegistry } from './registry';
import { nativeBinaryProblem } from './signin-sandbox';

/**
 * A read-only report of what stands between this computer and Antigravity
 * account switching (docs/antigravity-runtime.md, "Releasing account
 * switching"): the native CLI pin, both release gates, the runtime
 * installation and adoption, the resident service socket, the saved profiles
 * and the automatic switching setting. It reads metadata only (no credential,
 * no network) and changes nothing.
 */
export interface AntigravityReleaseStatus {
  nativeCli: 'missing' | 'pinned' | 'changed';
  dashboardGateOpen: boolean;
  runtimeGateOpen: boolean;
  runtimeInstalled: boolean;
  adoption: 'none' | 'adopted' | 'unfinished';
  serviceSocket: boolean;
  profiles: Array<{ id: string; email: string; runtimeVerifiedActive: boolean }>;
  automaticSwitching: {
    enabled: boolean;
    thresholdUsedPercent: number;
    requestedPoolId: string | null;
  } | null;
  nextStep: string;
}

export interface ReleaseStatusDeps {
  ccsDir: string;
  home: string;
  /** The packaged runtime release file. */
  releaseFile?: string;
  dashboardGate?: boolean;
  pinMatches?: (binary: string, sha256: string) => boolean;
}

function readRelease(file: string): { open: boolean; nativeSha256: string | null } {
  try {
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as Record<string, unknown>;
    return {
      open:
        value.nativeActivationReleased === true &&
        typeof value.nativeProofReceiptSha256 === 'string' &&
        /^[a-f0-9]{64}$/.test(value.nativeProofReceiptSha256),
      nativeSha256:
        typeof value.nativeSha256 === 'string' && /^[a-f0-9]{64}$/.test(value.nativeSha256)
          ? value.nativeSha256
          : null,
    };
  } catch {
    return { open: false, nativeSha256: null };
  }
}

function readAdoption(ccsDir: string): AntigravityReleaseStatus['adoption'] {
  const file = path.join(ccsDir, 'antigravity-switching', 'runtime-adoption.json');
  try {
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.size > 4 * 1024 * 1024) return 'unfinished';
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as { phase?: unknown };
    return value.phase === 'adopted' ? 'adopted' : 'unfinished';
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'ENOENT' ? 'none' : 'unfinished';
  }
}

function nextStep(status: Omit<AntigravityReleaseStatus, 'nextStep'>): string {
  if (status.nativeCli === 'missing') return 'Install the Antigravity CLI at ~/.local/bin/agy.';
  if (status.profiles.length < 2)
    return 'Save a second profile: ai-account-center antigravity signin <profile>.';
  if (status.nativeCli === 'changed')
    return 'The Antigravity CLI is not the reviewed 1.2.14 build; switching stays off until the runtime is reviewed for it.';
  if (!status.runtimeGateOpen || !status.dashboardGateOpen)
    return 'Open both release gates and deploy that build (releasing steps 1 and 2).';
  if (!status.runtimeInstalled)
    return 'Install the runtime: python3 -I scripts/antigravity/install_runtime.py --apply (step 3).';
  if (status.adoption !== 'adopted')
    return status.adoption === 'none'
      ? 'Adopt the runtime: python3 -I scripts/antigravity/adopt_runtime.py (step 4).'
      : 'Runtime adoption did not finish; review it or roll it back with adopt_runtime.py --rollback.';
  if (!status.serviceSocket)
    return 'Start ai-account-center-antigravity.service, then restart ccs-dashboard (steps 4 and 5).';
  if (!status.automaticSwitching?.enabled)
    return 'Manual switching can be tested (step 7). Automatic switching is off (step 8).';
  if (!status.automaticSwitching.requestedPoolId)
    return 'Automatic switching is on but has no quota pool; choose one both accounts report (step 8).';
  return 'Manual and automatic switching are open.';
}

export function readAntigravityReleaseStatus(deps: ReleaseStatusDeps): AntigravityReleaseStatus {
  const ccsDir = path.resolve(deps.ccsDir);
  const home = path.resolve(deps.home);
  const release = readRelease(
    deps.releaseFile ?? path.resolve(__dirname, '../../scripts/antigravity/runtime/release.json')
  );
  const binary = path.join(home, '.local', 'bin', 'agy');
  const nativeCli: AntigravityReleaseStatus['nativeCli'] = nativeBinaryProblem(
    home,
    process.getuid?.() ?? null
  )
    ? 'missing'
    : release.nativeSha256 &&
        (deps.pinMatches ?? verifyOwnedAntigravityNativePin)(binary, release.nativeSha256)
      ? 'pinned'
      : 'changed';
  const installed = readInstalledAntigravityRuntime(ccsDir, home);
  let serviceSocket = false;
  if (installed) {
    try {
      serviceSocket = fs.lstatSync(installed.socketPath).isSocket();
    } catch {
      serviceSocket = false;
    }
  }
  let profiles: AntigravityReleaseStatus['profiles'] = [];
  try {
    // Read-only: the report never creates a folder.
    profiles = (AntigravityProfileRegistry.openExisting(ccsDir)?.listProfiles() ?? []).map(
      (profile) => ({
        id: profile.id,
        email: profile.email,
        runtimeVerifiedActive: profile.hosts.some((host) => host.active),
      })
    );
  } catch {
    profiles = [];
  }
  let automaticSwitching: AntigravityReleaseStatus['automaticSwitching'] = null;
  try {
    const { settings } = new AntigravityAutoSwitchFileStore(
      path.join(ccsDir, 'antigravity-switching')
    ).read();
    automaticSwitching = {
      enabled: settings.enabled,
      thresholdUsedPercent: settings.thresholdUsedPercent,
      requestedPoolId: settings.requestedPoolId,
    };
  } catch {
    automaticSwitching = null;
  }
  const status = {
    nativeCli,
    dashboardGateOpen: deps.dashboardGate ?? ANTIGRAVITY_NATIVE_RELEASED,
    runtimeGateOpen: release.open,
    runtimeInstalled: installed !== null,
    adoption: readAdoption(ccsDir),
    serviceSocket,
    profiles,
    automaticSwitching,
  };
  return { ...status, nextStep: nextStep(status) };
}

export function formatAntigravityReleaseStatus(status: AntigravityReleaseStatus): string[] {
  const yes = (value: boolean) => (value ? 'yes' : 'no');
  const gate = (open: boolean) => (open ? 'open' : 'closed');
  return [
    'Antigravity switching on Ubuntu',
    `  Native CLI:          ${
      status.nativeCli === 'pinned'
        ? 'installed, reviewed 1.2.14 build'
        : status.nativeCli === 'changed'
          ? 'installed, not the reviewed build'
          : 'missing'
    }`,
    `  Dashboard gate:      ${gate(status.dashboardGateOpen)}`,
    `  Runtime gate:        ${gate(status.runtimeGateOpen)}`,
    `  Runtime installed:   ${yes(status.runtimeInstalled)}`,
    `  Runtime adopted:     ${status.adoption === 'none' ? 'no' : status.adoption === 'adopted' ? 'yes' : 'unfinished'}`,
    `  Runtime service:     ${status.serviceSocket ? 'socket present' : 'not running'}`,
    `  Saved profiles:      ${
      status.profiles.length
        ? status.profiles
            .map(
              (profile) =>
                `${profile.id} (${profile.email})${profile.runtimeVerifiedActive ? ' active' : ''}`
            )
            .join(', ')
        : 'none'
    }`,
    `  Automatic switching: ${
      status.automaticSwitching
        ? `${status.automaticSwitching.enabled ? 'on' : 'off'}, at ${status.automaticSwitching.thresholdUsedPercent}% used, pool ${status.automaticSwitching.requestedPoolId ?? 'not chosen'}`
        : 'settings unreadable'
    }`,
    `  Next step:           ${status.nextStep}`,
  ];
}

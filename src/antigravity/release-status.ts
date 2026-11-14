import fs from 'fs';
import path from 'path';
import {
  ANTIGRAVITY_NATIVE_RELEASED,
  readInstalledAntigravityRuntime,
  verifyOwnedAntigravityNativePin,
} from './production-runtime';
import { defaultNativeReleaseFile, readNativeRelease, type ReviewedNative } from './native-version';
import { AntigravityProfileRegistry } from './registry';
import {
  describeRuntimeServiceProblem,
  diagnoseRuntimeService,
  isRuntimeParserProblem,
  runtimeServiceProblemDetail,
} from './runtime-health';
import { nativeBinaryProblem } from './signin-sandbox';
import type { AntigravityRuntimeServiceProblem } from './usage-contract';

/**
 * A read-only report of the Antigravity runtime on this computer
 * (docs/antigravity-runtime.md): the native CLI pin, both release gates, the
 * runtime installation and adoption, the resident service socket that managed
 * updates use, and the saved profiles. Account switching was removed
 * (2026-11-14). It reads metadata only (no credential, no network) and changes
 * nothing. When the socket is missing it also runs the
 * read-only runtime health check (runtime-health.ts), so a service that cannot
 * start is named instead of reported as merely "not running".
 */
export interface AntigravityReleaseStatus {
  nativeCli: 'missing' | 'pinned' | 'changed';
  /** Reviewed `agy --version` stamps from the packaged release, oldest first. */
  reviewedNativeVersions: string[];
  dashboardGateOpen: boolean;
  runtimeGateOpen: boolean;
  runtimeInstalled: boolean;
  adoption: 'none' | 'adopted' | 'unfinished';
  serviceSocket: boolean;
  /** Why the service is not running, when the read-only check found a cause. */
  serviceProblem: AntigravityRuntimeServiceProblem | null;
  profiles: Array<{ id: string; email: string; runtimeVerifiedActive: boolean }>;
  nextStep: string;
}

export interface ReleaseStatusDeps {
  ccsDir: string;
  home: string;
  /** The packaged runtime release file. */
  releaseFile?: string;
  dashboardGate?: boolean;
  pinMatches?: (binary: string, sha256: string) => boolean;
  /** Read-only runtime health check; production runs runtime-health.ts. */
  diagnoseService?: (bundleDirectory: string) => AntigravityRuntimeServiceProblem | null;
}

function readRelease(file: string): { open: boolean; reviewed: ReviewedNative[] } {
  const release = readNativeRelease(file);
  return { open: release.gateOpen, reviewed: release.reviewed };
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
  if (status.profiles.length < 1)
    return 'Save a profile: ai-account-center antigravity signin <profile>.';
  if (status.nativeCli === 'changed')
    return 'The Antigravity CLI is not the reviewed build; managed updates are paused until the runtime is reviewed for it.';
  if (!status.runtimeGateOpen || !status.dashboardGateOpen)
    return 'Open both release gates and deploy that build (releasing steps 1 and 2).';
  if (!status.runtimeInstalled)
    return 'Install the runtime: python3 -I scripts/antigravity/install_runtime.py --apply (step 3).';
  if (status.adoption !== 'adopted')
    return status.adoption === 'none'
      ? 'Adopt the runtime: python3 -I scripts/antigravity/adopt_runtime.py (step 4).'
      : 'Runtime adoption did not finish; review it or roll it back with adopt_runtime.py --rollback.';
  if (
    !status.serviceSocket &&
    status.serviceProblem &&
    isRuntimeParserProblem(status.serviceProblem)
  )
    // Starting the same bundle again would fail the same way.
    return `${describeRuntimeServiceProblem(status.serviceProblem)}. Starting the service again will fail; rebuild the runtime bundle: python3 -I scripts/antigravity/rebuild_bundle.py --plan, then --apply. It restarts only the runtime service; the dashboard picks it up without a restart.`;
  if (!status.serviceSocket && status.serviceProblem)
    return `${describeRuntimeServiceProblem(status.serviceProblem)}. Read journalctl --user -u ai-account-center-antigravity.service and fix the cause before starting it again.`;
  if (!status.serviceSocket)
    return 'Start ai-account-center-antigravity.service, then restart ccs-dashboard (steps 4 and 5).';
  return 'The Antigravity runtime is ready.';
}

export function readAntigravityReleaseStatus(deps: ReleaseStatusDeps): AntigravityReleaseStatus {
  const ccsDir = path.resolve(deps.ccsDir);
  const home = path.resolve(deps.home);
  const release = readRelease(deps.releaseFile ?? defaultNativeReleaseFile());
  const binary = path.join(home, '.local', 'bin', 'agy');
  const pinMatches = deps.pinMatches ?? verifyOwnedAntigravityNativePin;
  const nativeCli: AntigravityReleaseStatus['nativeCli'] = nativeBinaryProblem(
    home,
    process.getuid?.() ?? null
  )
    ? 'missing'
    : release.reviewed.some((entry) => {
          try {
            return pinMatches(binary, entry.sha256);
          } catch {
            return false;
          }
        })
      ? 'pinned'
      : 'changed';
  const installed = readInstalledAntigravityRuntime(ccsDir, home);
  let serviceSocket = false;
  let serviceProblem: AntigravityRuntimeServiceProblem | null = null;
  if (installed) {
    try {
      serviceSocket = fs.lstatSync(installed.socketPath).isSocket();
    } catch {
      serviceSocket = false;
    }
    if (!serviceSocket) {
      try {
        serviceProblem = (deps.diagnoseService ?? diagnoseRuntimeService)(
          installed.bundleDirectory
        );
      } catch {
        serviceProblem = null;
      }
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
  const status = {
    nativeCli,
    reviewedNativeVersions: release.reviewed.map((entry) => entry.version),
    dashboardGateOpen: deps.dashboardGate ?? ANTIGRAVITY_NATIVE_RELEASED,
    runtimeGateOpen: release.open,
    runtimeInstalled: installed !== null,
    adoption: readAdoption(ccsDir),
    serviceSocket,
    serviceProblem,
    profiles,
  };
  return { ...status, nextStep: nextStep(status) };
}

export function formatAntigravityReleaseStatus(status: AntigravityReleaseStatus): string[] {
  const yes = (value: boolean) => (value ? 'yes' : 'no');
  const gate = (open: boolean) => (open ? 'open' : 'closed');
  return [
    'Antigravity runtime on Ubuntu',
    `  Native CLI:          ${
      status.nativeCli === 'pinned'
        ? `installed, reviewed ${status.reviewedNativeVersions.join(' / ')} build`
        : status.nativeCli === 'changed'
          ? 'installed, not the reviewed build'
          : 'missing'
    }`,
    `  Dashboard gate:      ${gate(status.dashboardGateOpen)}`,
    `  Runtime gate:        ${gate(status.runtimeGateOpen)}`,
    `  Runtime installed:   ${yes(status.runtimeInstalled)}`,
    `  Runtime adopted:     ${status.adoption === 'none' ? 'no' : status.adoption === 'adopted' ? 'yes' : 'unfinished'}`,
    `  Runtime service:     ${
      status.serviceSocket
        ? 'socket present'
        : status.serviceProblem
          ? `failed: ${runtimeServiceProblemDetail(status.serviceProblem)}`
          : 'not running'
    }`,
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
    `  Next step:           ${status.nextStep}`,
  ];
}

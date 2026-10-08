/**
 * Read-only preview of saved-login renewal against a real install: no network,
 * no writes, no lock. It reports what each guard would decide and which other
 * login files (by path and match label) share or may share a profile's family.
 * Token text and digests never appear in the result.
 *
 *   node -e "require('<package>/dist/codex-auth/codex-renewal-dry-run')
 *     .dryRunCodexProfileRenewal().then((r) => console.log(JSON.stringify(r, null, 2)))"
 */
import * as path from 'path';
import { getCcsDir } from '../utils/config-manager';
import { getCodexInstancesDir } from './codex-profile-paths';
import { sameAccountOrUnknown, type CodexLoginSourceKind } from './codex-login-family';
import {
  collectCodexRenewalSnapshot,
  planCodexProfileRenewal,
  type CodexRenewalDecision,
  type CodexRenewalEnvironment,
  type CodexRenewalGuards,
} from './codex-renewal-planner';
import {
  codexRenewalState,
  isCodexProfileRenewalEnabled,
  type CodexRenewalReason,
  type CodexRenewalState,
} from './codex-renewal-types';

export interface CodexRenewalDryRunProfile {
  name: string;
  decision: CodexRenewalDecision;
  state: CodexRenewalState;
  reason: CodexRenewalReason;
  message: string;
  accessExpiresAt: string | null;
  dueAt: string | null;
  lastRenewedAt: string | null;
  lastOutcome: CodexRenewalReason | null;
  nextAttemptAt: string | null;
  /** Whether the saved login has session ids / sign-in times to compare. */
  hasSessionId: boolean;
  hasAuthTime: boolean;
  guards: CodexRenewalGuards & { sharesFamily: boolean };
  /** Other saved profiles of the same account (separate logins unless listed in sharedWith). */
  sameAccountProfiles: string[];
}

export interface CodexRenewalDryRun {
  generatedAt: string;
  enabledByEnvironment: boolean;
  ccsDir: string;
  instancesDir: string;
  codexHome: string;
  homeDir: string;
  processScan: 'ok' | 'unavailable' | 'failed';
  processHomes: string[];
  sources: { path: string; kind: CodexLoginSourceKind; state: 'ok' | 'none' | 'unreadable' }[];
  profiles: CodexRenewalDryRunProfile[];
}

export async function dryRunCodexProfileRenewal(
  options: CodexRenewalEnvironment & { now?: () => number; env?: NodeJS.ProcessEnv } = {}
): Promise<CodexRenewalDryRun> {
  const now = (options.now ?? Date.now)();
  const snapshot = collectCodexRenewalSnapshot(options);
  return {
    generatedAt: new Date(now).toISOString(),
    enabledByEnvironment: isCodexProfileRenewalEnabled(options.env),
    ccsDir: path.resolve(getCcsDir()),
    instancesDir: path.resolve(getCodexInstancesDir()),
    codexHome: snapshot.codexHome,
    homeDir: snapshot.homeDir,
    processScan: snapshot.processScan.state,
    processHomes: snapshot.processScan.homes.map(({ home }) => home),
    sources: snapshot.sources.map(({ path: file, kind, state }) => ({ path: file, kind, state })),
    profiles: snapshot.profiles.map((profile) => {
      const { entry, guards } = planCodexProfileRenewal(snapshot, profile, now);
      const fingerprint = profile.fingerprint;
      return {
        name: entry.name,
        decision: entry.decision,
        state: codexRenewalState(entry.reason),
        reason: entry.reason,
        message: entry.message,
        accessExpiresAt: entry.accessExpiresAt,
        dueAt: entry.dueAt,
        lastRenewedAt: entry.lastRenewedAt,
        lastOutcome: entry.lastOutcome,
        nextAttemptAt: entry.nextAttemptAt,
        hasSessionId: (fingerprint?.sessionIds.size ?? 0) > 0,
        hasAuthTime: (fingerprint?.authTimes.size ?? 0) > 0,
        guards: { ...guards, sharesFamily: guards.sharedWith.length > 0 },
        sameAccountProfiles: snapshot.profiles
          .filter(
            (other) =>
              other.name !== profile.name &&
              fingerprint?.binding &&
              other.fingerprint?.binding &&
              sameAccountOrUnknown(fingerprint, other.fingerprint)
          )
          .map((other) => other.name),
      };
    }),
  };
}

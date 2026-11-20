/**
 * The public copies the read-only Antigravity routes answer with (moved verbatim from
 * antigravity-routes.ts so the M3 parity harness can count them). The M3 Rust service ports
 * both: crates/aac-views/src/antigravity.rs.
 */
import { AntigravityError } from './errors';
import { publicRuntimeServiceProblem } from './runtime-health';
import type { AntigravityDashboardAccount, AntigravityInventory } from './usage-contract';
import {
  displayText,
  email,
  safeId,
  timestamp,
  publicDashboardAccount as publicAccount,
} from './usage-normalization';
import { markPassedResets } from '../web-server/services/account-window-reset';

function runtimeServiceProblemField(value: unknown): Partial<AntigravityInventory> {
  const problem = publicRuntimeServiceProblem(value);
  return problem ? { runtimeServiceProblem: problem } : {};
}

/** All public returns are rebuilt so adapter-private fields cannot leak accidentally. */
export function publicInventory(value: AntigravityInventory): AntigravityInventory {
  if (value.hostId !== 'ubuntu' || !Array.isArray(value.profiles) || value.profiles.length > 16)
    throw new AntigravityError('Invalid Antigravity inventory.');
  const ids = new Set<string>();
  return {
    schemaVersion: 1,
    hostId: 'ubuntu',
    ...runtimeServiceProblemField(value.runtimeServiceProblem),
    profiles: value.profiles.map((profile) => {
      if (
        !safeId(profile.id) ||
        ids.has(profile.id) ||
        !email(profile.email) ||
        profile.hostId !== 'ubuntu'
      )
        throw new AntigravityError('Invalid Antigravity inventory.');
      ids.add(profile.id);
      return {
        id: profile.id,
        email: email(profile.email) as string,
        plan: displayText(profile.plan, 80),
        hostId: 'ubuntu',
        available: profile.available === true,
        selected: profile.selected === true,
        runtimeVerified: profile.runtimeVerified === true,
        verifiedAt: timestamp(profile.verifiedAt),
      };
    }),
  };
}

/** The usage answer: every account public and judged against one instant. */
export function publicQuotas(accounts: AntigravityDashboardAccount[], at: number) {
  return {
    schemaVersion: 1 as const,
    hostId: 'ubuntu' as const,
    accounts: accounts.map((account) => markPassedResets(publicAccount(account), at)),
  };
}

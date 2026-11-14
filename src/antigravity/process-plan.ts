/**
 * The Antigravity process census the managed updater checks before and after it stops anything: validated, then
 * fingerprinted so a changed census is refused.
 */
import { AntigravityError } from './errors';
import { createHash } from 'crypto';
import type { ProcessPlan } from './types';

const LABELS = {
  cli: 'Antigravity CLI',
  desktop: 'Antigravity Desktop',
  'language-server': 'Antigravity language server',
} as const;

export function checkedPlan(plan: ProcessPlan): ProcessPlan {
  if (
    !plan ||
    typeof plan.complete !== 'boolean' ||
    !Array.isArray(plan.processes) ||
    plan.processes.length > 128
  )
    throw new AntigravityError('Unsafe process census.');
  const pids = new Set<number>();
  for (const process of plan.processes) {
    const identity = process.identity;
    if (
      !identity ||
      !Number.isSafeInteger(identity.pid) ||
      identity.pid <= 0 ||
      pids.has(identity.pid) ||
      !Object.prototype.hasOwnProperty.call(LABELS, process.role) ||
      typeof identity.startTime !== 'string' ||
      identity.startTime.length < 1 ||
      identity.startTime.length > 128 ||
      /[\x00-\x1f]/.test(identity.startTime) ||
      typeof identity.ownerId !== 'string' ||
      identity.ownerId.length < 1 ||
      identity.ownerId.length > 256 ||
      /[\x00-\x1f]/.test(identity.ownerId) ||
      !/^[a-f0-9]{64}$/.test(identity.fingerprint)
    )
      throw new AntigravityError('Unsafe process census.');
    pids.add(identity.pid);
  }
  if (
    plan.continuity !== null &&
    (!plan.continuity ||
      !/^[a-f0-9]{64}$/.test(plan.continuity.fingerprint) ||
      typeof plan.continuity.restorable !== 'boolean')
  )
    throw new AntigravityError('Unsafe session continuity.');
  return {
    complete: plan.complete,
    processes: plan.processes.map((process) => ({
      role: process.role,
      identity: {
        pid: process.identity.pid,
        startTime: process.identity.startTime,
        ownerId: process.identity.ownerId,
        fingerprint: process.identity.fingerprint,
      },
    })),
    continuity: plan.continuity ? { ...plan.continuity } : null,
  };
}

export function planFingerprint(plan: ProcessPlan): string {
  const safe = checkedPlan(plan);
  return createHash('sha256')
    .update(
      JSON.stringify({
        complete: safe.complete,
        processes: safe.processes.sort((left, right) => left.identity.pid - right.identity.pid),
        continuity: safe.continuity,
      })
    )
    .digest('hex');
}

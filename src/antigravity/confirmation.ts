import { AntigravityError } from './errors';
import { createHash, randomBytes } from 'crypto';
import type { ActivationConfirmation, ProcessPlan } from './types';

export interface ConfirmationBinding {
  profileId: string;
  currentIdentityKey: string;
  targetIdentityKey: string;
  credentialFingerprint: string;
  planFingerprint: string;
}

interface PendingConfirmation {
  binding: ConfirmationBinding;
  expiresAt: number;
}

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

export class AntigravityConfirmationStore {
  private readonly pending = new Map<string, PendingConfirmation>();

  issue(
    binding: ConfirmationBinding,
    email: string,
    plan: ProcessPlan,
    now: number
  ): ActivationConfirmation {
    for (const [token, record] of this.pending) {
      if (record.expiresAt <= now) this.pending.delete(token);
    }
    while (this.pending.size >= 32) {
      const oldest = this.pending.keys().next().value;
      if (oldest === undefined) break;
      this.pending.delete(oldest);
    }
    const token = randomBytes(32).toString('base64url');
    const expiresAt = now + 60_000;
    this.pending.set(token, { binding: { ...binding }, expiresAt });
    return {
      token,
      expiresAt: new Date(expiresAt).toISOString(),
      profileId: binding.profileId,
      hostId: 'ubuntu',
      email,
      processes: checkedPlan(plan).processes.map((process) => ({
        pid: process.identity.pid,
        role: process.role,
        label: LABELS[process.role],
      })),
      warning:
        'Antigravity is running on Ubuntu. Stop the reviewed programs, switch accounts, and resume the exact saved session in its original project and terminal? Prompts are never replayed.',
    };
  }

  /** All attempts are one-shot, including stale/incorrect bindings. Caller must hold activation lock. */
  consume(token: string, binding: ConfirmationBinding, now: number): boolean {
    const record = this.pending.get(token);
    this.pending.delete(token);
    return (
      !!record &&
      record.expiresAt > now &&
      JSON.stringify(record.binding) === JSON.stringify(binding)
    );
  }
}

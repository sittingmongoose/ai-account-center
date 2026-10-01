import { createHash, randomBytes } from 'crypto';

export type CodexActivationProcessRole = 'daemon' | 'desktop' | 'cli' | 'automation';

export interface CodexActivationProcessDisplay {
  label: string;
  pid: number;
  role: CodexActivationProcessRole;
}

/** Digests and kernel start identities only: never credentials, environment or prompts. */
export interface CodexActivationProcessIdentity {
  pid: number;
  ppid: number;
  startTime: string;
  fingerprint: string;
}

export interface CodexActivationStopPlan {
  identities: CodexActivationProcessIdentity[];
  roots: number[];
  processes: CodexActivationProcessDisplay[];
}

export interface CodexActivationConfirmation {
  token: string;
  expiresAt: string;
  targetProfile: string;
  processes: CodexActivationProcessDisplay[];
  warning: string;
}

interface PendingConfirmation {
  targetProfile: string;
  authHash: string;
  expiresAt: number;
  plan: CodexActivationStopPlan;
}

const pending = new Map<string, PendingConfirmation>();
const CONFIRMATION_TTL = 60_000;
const MAX_CONFIRMATIONS = 32;

export function codexAuthHash(content: Buffer): string {
  return createHash('sha256').update(content).digest('hex');
}

export function codexProcessFingerprint(process: {
  exe: string;
  args: string[];
  cwd: string;
}): string {
  return createHash('sha256')
    .update(JSON.stringify([process.exe, process.args, process.cwd]))
    .digest('hex');
}

function prune(now: number): void {
  for (const [token, record] of pending) if (record.expiresAt <= now) pending.delete(token);
  while (pending.size >= MAX_CONFIRMATIONS) {
    const oldest = pending.keys().next().value;
    if (oldest === undefined) break;
    pending.delete(oldest);
  }
}

export function issueCodexActivationConfirmation(
  targetProfile: string,
  authHash: string,
  plan: CodexActivationStopPlan,
  now = Date.now()
): CodexActivationConfirmation {
  prune(now);
  const token = randomBytes(32).toString('base64url');
  const expiresAt = now + CONFIRMATION_TTL;
  // Clone safe data so the runtime cannot silently enlarge an approved stop set.
  const savedPlan: CodexActivationStopPlan = {
    identities: plan.identities.map((identity) => ({ ...identity })),
    roots: [...plan.roots],
    processes: plan.processes.map((process) => ({ ...process })),
  };
  pending.set(token, { targetProfile, authHash, expiresAt, plan: savedPlan });
  return {
    token,
    expiresAt: new Date(expiresAt).toISOString(),
    targetProfile,
    processes: savedPlan.processes.map((process) => ({ ...process })),
    warning:
      'Stopping these programs interrupts active Codex work. CCS will switch the account and restart the affected programs without replaying prompts. Browser helpers reopen when the restarted Codex program next needs them.',
  };
}

/** Must be called under the account activation lock, before signalling anything. */
export function consumeCodexActivationConfirmation(
  token: string,
  targetProfile: string,
  authHash: string,
  now = Date.now()
): CodexActivationStopPlan | undefined {
  const record = pending.get(token);
  pending.delete(token); // Every attempt is one-shot, including a failed revalidation.
  if (
    !record ||
    record.expiresAt <= now ||
    record.targetProfile !== targetProfile ||
    record.authHash !== authHash
  ) {
    return undefined;
  }
  return record.plan;
}

export function isCodexActivationBody(body: unknown): body is { confirmationToken?: string } {
  if (!body || typeof body !== 'object' || Array.isArray(body)) return false;
  const keys = Object.keys(body);
  if (keys.length === 0) return true;
  const value = body as Record<string, unknown>;
  return (
    keys.length === 1 &&
    keys[0] === 'confirmationToken' &&
    typeof value.confirmationToken === 'string' &&
    /^[A-Za-z0-9_-]{43}$/.test(value.confirmationToken)
  );
}

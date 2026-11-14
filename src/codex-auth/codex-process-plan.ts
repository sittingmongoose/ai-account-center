/**
 * The Codex stop plan the process runtime builds and checks: what it would close, as kernel start identities and
 * digests. Shared by the app-update runtime bundle (codex-update-runtime-entry.ts) and the renewal planner.
 */
import { createHash } from 'crypto';

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

export function codexProcessFingerprint(process: {
  exe: string;
  args: string[];
  cwd: string;
}): string {
  return createHash('sha256')
    .update(JSON.stringify([process.exe, process.args, process.cwd]))
    .digest('hex');
}

/**
 * Claude "Open" progress (CONTRACT-serving-misc section 4.4, agreed with the
 * Codex owner on 2026-10-02).
 *
 * When a managed history policy applies, POST /api/claude/desktop-profiles/:id/open
 * answers 202 and the Open runs here, in the background; GET
 * /api/claude/desktop-profiles shows its progress as `openOperation`. Operations
 * live in memory only: a server restart never resumes one (the durable history
 * marker still holds an unconfirmed copy), and clients never replay the POST.
 */

import { randomBytes } from 'crypto';
import { ConfigError, ProfileError } from '../../errors/error-types';
import {
  CLAUDE_HISTORY_UNCONFIRMED_MESSAGE,
  ClaudeHistoryOpenHeldError,
  type ClaudeOpenObserver,
} from './claude-desktop-open-service';
import { ClaudeDesktopTransportError } from './claude-desktop-transport';

export type ClaudeOpenOperationState =
  | 'checking'
  | 'copying'
  | 'opening'
  | 'opened'
  | 'blocked_uncertain'
  | 'failed';

/** No UUIDs, titles, transcript text, ssh details or paths. */
export interface ClaudeOpenOperation {
  id: string;
  platform: 'mac' | 'windows';
  state: ClaudeOpenOperationState;
  confirmedCount: number | null;
  totalCount: number | null;
  /** A fixed sentence for blocked_uncertain and failed; null otherwise. */
  message: string | null;
}

export const CLAUDE_OPEN_FAILURE_MESSAGES = {
  notFound: 'Claude desktop profile was not found.',
  notConfigured: 'Claude desktop launcher is not configured for this platform.',
  timedOut: 'Claude desktop request timed out.',
  transport: 'Claude desktop request failed.',
  other: 'Claude account could not be opened safely.',
} as const;

const TERMINAL: ReadonlySet<ClaudeOpenOperationState> = new Set([
  'opened',
  'blocked_uncertain',
  'failed',
]);
const DEFAULT_RETAIN_MS = 10 * 60_000;
const MAX_OPERATIONS = 64;

export function isTerminalClaudeOpenState(state: ClaudeOpenOperationState): boolean {
  return TERMINAL.has(state);
}

interface Entry {
  scope: string;
  profileId: string;
  operation: ClaudeOpenOperation;
  startedAt: number;
  finishedAt: number | null;
}

function finalState(error: unknown): Pick<ClaudeOpenOperation, 'state' | 'message'> {
  if (error instanceof ClaudeHistoryOpenHeldError)
    return { state: 'blocked_uncertain', message: CLAUDE_HISTORY_UNCONFIRMED_MESSAGE };
  const message =
    error instanceof ProfileError
      ? CLAUDE_OPEN_FAILURE_MESSAGES.notFound
      : error instanceof ConfigError
        ? CLAUDE_OPEN_FAILURE_MESSAGES.notConfigured
        : error instanceof ClaudeDesktopTransportError
          ? error.timedOut
            ? CLAUDE_OPEN_FAILURE_MESSAGES.timedOut
            : CLAUDE_OPEN_FAILURE_MESSAGES.transport
          : CLAUDE_OPEN_FAILURE_MESSAGES.other;
  return { state: 'failed', message };
}

export interface ClaudeOpenOperationsOptions {
  now?: () => number;
  /** How long a finished operation stays visible for the clients' last poll. */
  retainMs?: number;
}

export class ClaudeOpenOperations {
  private readonly entries = new Map<string, Entry>();

  constructor(private readonly options: ClaudeOpenOperationsOptions = {}) {}

  private now(): number {
    return (this.options.now ?? Date.now)();
  }

  private prune(): void {
    const retainMs = this.options.retainMs ?? DEFAULT_RETAIN_MS;
    const now = this.now();
    for (const [key, entry] of this.entries) {
      if (entry.finishedAt !== null && now - entry.finishedAt >= retainMs) this.entries.delete(key);
    }
    // Bounded even if many profiles finish at once: drop the oldest finished first.
    const finished = [...this.entries].filter(([, entry]) => entry.finishedAt !== null);
    finished.sort(([, a], [, b]) => (a.finishedAt ?? 0) - (b.finishedAt ?? 0));
    while (this.entries.size > MAX_OPERATIONS && finished.length) {
      const oldest = finished.shift();
      if (oldest) this.entries.delete(oldest[0]);
    }
  }

  /**
   * Start an Open in the background, or return the one already running for the
   * same profile and platform; a running Open is never started twice. `scope` is
   * the CCS directory, so separate private scopes never share an operation.
   */
  start(
    scope: string,
    profileId: string,
    platform: 'mac' | 'windows',
    run: (observer: ClaudeOpenObserver) => Promise<void>
  ): ClaudeOpenOperation {
    this.prune();
    const key = JSON.stringify([scope, profileId, platform]);
    const existing = this.entries.get(key);
    if (existing && existing.finishedAt === null) return { ...existing.operation };
    const entry: Entry = {
      scope,
      profileId,
      startedAt: this.now(),
      finishedAt: null,
      operation: {
        id: `op_${randomBytes(12).toString('hex')}`,
        platform,
        state: 'checking',
        confirmedCount: null,
        totalCount: null,
        message: null,
      },
    };
    this.entries.set(key, entry);
    const update = (patch: Partial<ClaudeOpenOperation>): void => {
      if (entry.finishedAt === null) Object.assign(entry.operation, patch);
    };
    const finish = (patch: Pick<ClaudeOpenOperation, 'state' | 'message'>): void => {
      if (entry.finishedAt !== null) return;
      Object.assign(entry.operation, patch);
      entry.finishedAt = this.now();
    };
    const observer: ClaudeOpenObserver = {
      copying: (totalCount) => update({ state: 'copying', totalCount, confirmedCount: 0 }),
      synchronized: ({ status, createdCount }) => {
        if (status === 'synchronized' && entry.operation.totalCount !== null)
          update({ confirmedCount: createdCount });
      },
      opening: () => update({ state: 'opening' }),
    };
    let running: Promise<void>;
    try {
      running = Promise.resolve(run(observer));
    } catch (error) {
      running = Promise.reject(error);
    }
    // Both outcomes are handled here, so a failed Open never becomes an unhandled rejection.
    running.then(
      () => finish({ state: 'opened', message: null }),
      (error: unknown) => finish(finalState(error))
    );
    return { ...entry.operation };
  }

  /** The running operation for this profile and platform, if any. */
  running(
    scope: string,
    profileId: string,
    platform: 'mac' | 'windows'
  ): ClaudeOpenOperation | null {
    const entry = this.entries.get(JSON.stringify([scope, profileId, platform]));
    return entry && entry.finishedAt === null ? { ...entry.operation } : null;
  }

  /** The operation to show for a profile: a running one first, else the newest finished one. */
  forProfile(scope: string, profileId: string): ClaudeOpenOperation | null {
    this.prune();
    let chosen: Entry | null = null;
    for (const entry of this.entries.values()) {
      if (entry.scope !== scope || entry.profileId !== profileId) continue;
      const running = entry.finishedAt === null;
      const chosenRunning = chosen?.finishedAt === null;
      if (
        !chosen ||
        (running && !chosenRunning) ||
        (running === chosenRunning && entry.startedAt >= chosen.startedAt)
      )
        chosen = entry;
    }
    return chosen ? { ...chosen.operation } : null;
  }
}

let operations: ClaudeOpenOperations | null = null;

/** The process-wide store; a restarted server starts empty. */
export function getClaudeOpenOperations(): ClaudeOpenOperations {
  operations ??= new ClaudeOpenOperations();
  return operations;
}

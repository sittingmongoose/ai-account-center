/**
 * Claude "Open" progress (CONTRACT-serving-misc section 4.4, agreed with the
 * Codex owner on 2026-10-02).
 *
 * When a managed history policy applies, every Open runs here so GET
 * /api/claude/desktop-profiles can show its progress as `openOperation`. A POST
 * that sends `Prefer: respond-async` gets 202 at once; any other POST waits for
 * the outcome and keeps today's 200 or refusal. Operations live in memory only:
 * a server restart never resumes one (the durable history marker still holds an
 * unconfirmed copy), and clients never replay the POST.
 */

import { randomBytes } from 'crypto';
import { ConfigError, ProfileError } from '../../errors/error-types';
import {
  CLAUDE_HISTORY_UNCONFIRMED_MESSAGE,
  ClaudeHistoryOpenHeldError,
  type ClaudeOpenObserver,
} from './claude-desktop-open-service';
import { ClaudeDesktopTransportError } from './claude-desktop-transport';
import { createLogger } from '../../services/logging';

const logger = createLogger('web-server:claude-open');

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

/** How an Open ended, for a caller that waits for it. Never rejects. */
export type ClaudeOpenOutcome = { ok: true } | { ok: false; error: unknown };

interface Entry {
  scope: string;
  profileId: string;
  operation: ClaudeOpenOperation;
  startedAt: number;
  finishedAt: number | null;
  settled: Promise<ClaudeOpenOutcome>;
}

/** A short error class for the server log; never a message, stack or path. */
export function claudeOpenErrorKind(error: unknown): string {
  if (error instanceof ClaudeHistoryOpenHeldError) return 'history_held';
  if (error instanceof ProfileError) return 'not_found';
  if (error instanceof ConfigError) return 'not_configured';
  if (error instanceof ClaudeDesktopTransportError)
    return error.timedOut ? 'transport_timeout' : 'transport';
  return 'other';
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
    let settle: (outcome: ClaudeOpenOutcome) => void = () => {};
    const entry: Entry = {
      scope,
      profileId,
      startedAt: this.now(),
      finishedAt: null,
      settled: new Promise<ClaudeOpenOutcome>((resolve) => {
        settle = resolve;
      }),
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
    const finish = (
      patch: Pick<ClaudeOpenOperation, 'state' | 'message'>,
      outcome: ClaudeOpenOutcome
    ): void => {
      if (entry.finishedAt !== null) return;
      Object.assign(entry.operation, patch);
      entry.finishedAt = this.now();
      settle(outcome);
      try {
        // State and error class only: no message, stack, ids or paths.
        logger[outcome.ok ? 'info' : 'warn']('claude.open.finished', 'Claude Open finished', {
          state: patch.state,
          kind: outcome.ok ? 'none' : claudeOpenErrorKind(outcome.error),
          platform,
        });
      } catch {
        /* Logging never changes how an Open ends. */
      }
    };
    // The copy runs in bounded batches. Counts cover the whole plan and only move
    // forward: a confirmed record is never un-confirmed within one Open.
    const forward = (count: number): boolean =>
      Number.isSafeInteger(count) && count >= (entry.operation.confirmedCount ?? 0);
    const observer: ClaudeOpenObserver = {
      copying: (totalCount, confirmedCount = 0) => {
        if (forward(confirmedCount) && confirmedCount <= totalCount)
          update({ state: 'copying', totalCount, confirmedCount });
      },
      synchronized: ({ status, createdCount }) => {
        // A refusal after some batches keeps their confirmed count; a refusal
        // before any batch leaves the count unset (totalCount is null). A
        // `partial` copy reached the per-Open bound: the Open still goes ahead
        // and shows the confirmed part of the plan; the next Open copies the rest.
        if (
          (status === 'synchronized' || status === 'refused' || status === 'partial') &&
          entry.operation.totalCount !== null &&
          forward(createdCount) &&
          createdCount <= entry.operation.totalCount
        )
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
      () => finish({ state: 'opened', message: null }, { ok: true }),
      (error: unknown) => finish(finalState(error), { ok: false, error })
    );
    return { ...entry.operation };
  }

  /**
   * How the current Open for this profile and platform ends (a running one, or
   * the last finished one while it is retained); null when none is known. The
   * promise never rejects, so a caller that stops waiting leaves nothing unhandled.
   */
  settled(
    scope: string,
    profileId: string,
    platform: 'mac' | 'windows'
  ): Promise<ClaudeOpenOutcome> | null {
    return this.entries.get(JSON.stringify([scope, profileId, platform]))?.settled ?? null;
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

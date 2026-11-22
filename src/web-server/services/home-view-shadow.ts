/** R1 Home shadow evidence. Never changes the served answer and never reads a provider. */
import type { Request } from 'express';
import { createLogger } from '../../services/logging/logger';
import type { RustService } from '../rust-service';
import { rustGroupMode } from '../rust-service-groups';
import { feedsOf } from '../views/feed-publication';
import { ensureDashboardFeed, passFaithful } from '../views/dashboard-publication';
import { withPassedResetsAt } from './account-dashboard-service';
import type { AccountDashboard, ClaudeDashboardPlatform } from './account-dashboard-types';

const logger = createLogger('home-view-shadow');

export type HomeShadowOutcome = 'equal' | 'different' | 'fallback' | 'error' | 'reference-failed';
export interface HomeShadowReport {
  outcome: HomeShadowOutcome;
  /** The differing top-level member names; empty unless the outcome is `different`. */
  members: string[];
  homeMs: number;
  referenceMs: number;
}

/** The parity spec's only normalization: JSON object key order. Arrays keep their order. */
function canonicalJson(value: unknown): string {
  const sort = (node: unknown): unknown => {
    if (Array.isArray(node)) return node.map(sort);
    if (node && typeof node === 'object') {
      const row = node as Record<string, unknown>;
      const out: Record<string, unknown> = {};
      for (const key of Object.keys(row).sort()) out[key] = sort(row[key]);
      return out;
    }
    return node;
  };
  return JSON.stringify(sort(value));
}

/** Structural member names only; values never enter the log. */
function differingMembers(home: unknown, reference: unknown): string[] {
  const left = home && typeof home === 'object' ? (home as Record<string, unknown>) : {};
  const right =
    reference && typeof reference === 'object' ? (reference as Record<string, unknown>) : {};
  const keys = [...new Set([...Object.keys(left), ...Object.keys(right)])].sort();
  return keys.filter((key) => canonicalJson(left[key]) !== canonicalJson(right[key]));
}

/** The last logged difference class; a repeat of the same class is not logged again. */
let lastDifferenceClass: string | null = null;

/**
 * Shadow (the agent-verbs pattern): the reference answer is served as soon as it settles; the
 * Rust Home answer runs alongside and the comparison is logged in the background. Only a
 * difference is logged, once per difference class (the differing member names); bodies never
 * enter the log. A fallback or failed Rust attempt is not a difference.
 */
export async function runHomeShadow<T>(
  home: () => Promise<T | null>,
  reference: () => Promise<T>,
  onCompared?: (report: HomeShadowReport) => void
): Promise<T> {
  const homeStarted = Date.now();
  const referenceStarted = Date.now();
  const pending = Promise.resolve()
    .then(() => home())
    .then(
      (value) => ({ value, failed: false }),
      () => ({ value: null as T | null, failed: true })
    );
  const settled = await Promise.resolve()
    .then(() => reference())
    .then(
      (value) => ({ ok: true as const, value }),
      (error: unknown) => ({ ok: false as const, error })
    );
  const referenceMs = Date.now() - referenceStarted;
  void pending
    .then(({ value, failed }) => {
      const homeMs = Date.now() - homeStarted;
      const outcome: HomeShadowOutcome = !settled.ok
        ? 'reference-failed'
        : failed
          ? 'error'
          : value === null
            ? 'fallback'
            : canonicalJson(value) === canonicalJson(settled.value)
              ? 'equal'
              : 'different';
      const members =
        outcome === 'different' && settled.ok ? differingMembers(value, settled.value) : [];
      if (outcome === 'different') {
        const differenceClass = members.join(',');
        if (differenceClass !== lastDifferenceClass) {
          lastDifferenceClass = differenceClass;
          logger.info('home.view.shadow', 'shadow comparison', {
            outcome,
            members,
            homeMs,
            referenceMs,
          });
        }
      } else {
        lastDifferenceClass = null;
      }
      onCompared?.({ outcome, members, homeMs, referenceMs });
    })
    .catch(() => {
      /* A failing comparison hook never changes the served answer. */
    });
  if (!settled.ok) throw settled.error;
  return settled.value;
}

export interface HomeShadowInput {
  req: Request;
  service: RustService;
  platform: ClaudeDashboardPlatform;
  secure: boolean;
  dashboard: AccountDashboard;
  now: number;
  scope: string;
}

/**
 * `groups.home: shadow` only: ask the private service for the fed Home view beside the TS
 * dashboard answer and compare its `dashboard` member against the TS per-response pass. The
 * served answer is untouched. The fed Home request needs the `home-opens` and `home-antigravity`
 * generations; while the front door does not hold them the candidate is an honest fallback,
 * not a difference. The Rust public door reads the same setting and stays off for `shadow`.
 */
export function homeShadowAnswer(
  input: HomeShadowInput,
  onCompared?: (report: HomeShadowReport) => void
): void {
  if (rustGroupMode('home') !== 'shadow' || input.service.mode !== 'on') return;
  const { req, service, platform, secure, dashboard, now, scope } = input;
  void runHomeShadow(
    async () => {
      const boot = service.feedBoot(scope);
      if (
        !boot ||
        !Array.isArray(dashboard.accounts) ||
        typeof dashboard.updatedAt !== 'string' ||
        !passFaithful(dashboard)
      ) {
        return null;
      }
      const feeds = feedsOf(service, boot);
      const generation = await ensureDashboardFeed(service, feeds, platform, secure, dashboard);
      if ('declined' in generation) return null;
      const opens = feeds.held.get('home-opens')?.generation;
      const antigravity = feeds.held.get('home-antigravity')?.generation;
      if (!opens || !antigravity) return null;
      const reply = await service.fedView(
        req,
        '/api/views/home',
        {
          'x-aac-feed': JSON.stringify({
            platform,
            secure,
            now,
            updatedAt: dashboard.updatedAt,
            dashboard: generation.generation,
            opens,
            antigravity,
          }),
        },
        'home@3'
      );
      if (!reply || typeof reply === 'string') return null;
      const parsed: unknown = JSON.parse(reply.bytes.toString('utf8'));
      const member =
        parsed && typeof parsed === 'object'
          ? (parsed as Record<string, unknown>).dashboard
          : undefined;
      return member === undefined ? null : member;
    },
    async () => withPassedResetsAt(dashboard, now),
    onCompared
  ).catch(() => {
    /* Evidence gathering never changes the served answer. */
  });
}

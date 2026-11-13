import { createHash } from 'crypto';
import { getModelPricingWithSource, type ModelPricingResolution } from '../model-pricing';
import { normalizeUsageProvider } from './model-identity';
import { analyticsSessionKey } from './analytics-session-key';
import type { AccountAnalyticsActivityProvider } from '../services/account-analytics-types';
import type { CompactEntry } from './types';
import type { UsageCell } from '../../store/usage-report';

const SESSION_KEY_SHAPE = /^[0-9a-f]{16}$/;
const MAX_ROUTE = 64;
const MAX_MODEL = 200;

/** The full SHA-256 of the session's published key string (DATA-ARCHITECTURE: `sessionHash`). */
export function analyticsSessionHash(
  tool: AccountAnalyticsActivityProvider,
  sessionId: string
): string {
  return createHash('sha256').update(`aac-session-v1:${tool}:${sessionId}`).digest('hex');
}

/** A label the store accepts: no control characters, bounded; never empty for a model. */
function label(value: string, max: number): string {
  return value
    .replace(/[\u0000-\u001f\u007f]/g, '')
    .trim()
    .slice(0, max);
}

/**
 * The usage record contract's cells (store/usage-report.ts) from one read's compact rows: the
 * absolute totals of each UTC hour, session, route and model. Pricing mirrors `aggregateRows`: a
 * logged cost wins, the rest is priced at list rates, and the part priced at the unknown-model
 * fallback is kept apart. `keyed`: the rows' session ids are already published keys (the remote
 * helper hashes on the host it reads); otherwise each raw id is hashed here, so no id leaves.
 */
export function cellsFromCompact(
  rows: readonly CompactEntry[],
  tool: AccountAnalyticsActivityProvider,
  keyed = false
): UsageCell[] {
  interface Slot {
    cell: UsageCell;
    logged: number;
    unlogged: { input: number; output: number; write: number; read: number };
  }
  const slots = new Map<string, Slot>();
  for (const row of rows) {
    const entry = row.entry;
    const at = Date.parse(entry.timestamp);
    if (!Number.isFinite(at)) continue;
    const hour = new Date(Math.floor(at / 3_600_000) * 3_600_000).toISOString();
    const route = label(
      normalizeUsageProvider(entry.provider ?? entry.target) ?? '',
      MAX_ROUTE
    ).toLowerCase();
    const modelRaw = label(entry.model || '', MAX_MODEL) || 'unknown';
    const raw = entry.sessionId || '';
    const sessionKey = !raw
      ? null
      : keyed
        ? SESSION_KEY_SHAPE.test(raw)
          ? raw
          : null
        : analyticsSessionKey(tool, raw);
    const sessionHash = raw && !keyed ? analyticsSessionHash(tool, raw) : null;
    const id = `${hour}\0${sessionKey ?? ''}\0${route}\0${modelRaw}`;
    let slot = slots.get(id);
    if (!slot) {
      slot = {
        cell: {
          hour,
          sessionKey,
          ...(sessionHash ? { sessionHash } : {}),
          route,
          modelRaw,
          inputTokens: 0,
          outputTokens: 0,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          events: 0,
        },
        logged: 0,
        unlogged: { input: 0, output: 0, write: 0, read: 0 },
      };
      slots.set(id, slot);
    }
    const count = (v: number) => (Number.isFinite(v) && v > 0 ? Math.floor(v) : 0);
    slot.cell.inputTokens += count(entry.inputTokens);
    slot.cell.outputTokens += count(entry.outputTokens);
    slot.cell.cacheCreationTokens += count(entry.cacheCreationTokens);
    slot.cell.cacheReadTokens += count(entry.cacheReadTokens);
    slot.cell.events = (slot.cell.events ?? 0) + count(row.events);
    if (entry.costUsd !== undefined && entry.costUsd > 0) slot.logged += entry.costUsd;
    else {
      slot.unlogged.input += count(entry.inputTokens);
      slot.unlogged.output += count(entry.outputTokens);
      slot.unlogged.write += count(entry.cacheCreationTokens);
      slot.unlogged.read += count(entry.cacheReadTokens);
    }
  }
  const pricing = new Map<string, ModelPricingResolution>();
  const cells: UsageCell[] = [];
  for (const { cell, logged, unlogged } of slots.values()) {
    const key = `${cell.route}\0${cell.modelRaw}`;
    let resolved = pricing.get(key);
    if (!resolved) {
      resolved = getModelPricingWithSource(cell.modelRaw, { provider: cell.route || undefined });
      pricing.set(key, resolved);
    }
    const rates = resolved.pricing;
    const listed =
      (unlogged.input / 1_000_000) * rates.inputPerMillion +
      (unlogged.output / 1_000_000) * rates.outputPerMillion +
      (unlogged.write / 1_000_000) * rates.cacheCreationPerMillion +
      (unlogged.read / 1_000_000) * rates.cacheReadPerMillion;
    const tokens =
      cell.inputTokens + cell.outputTokens + cell.cacheCreationTokens + cell.cacheReadTokens;
    if (tokens === 0 && logged === 0) continue;
    cell.loggedCostUsd = logged > 0 ? logged : null;
    cell.costUsd = logged + listed;
    cell.fallbackCostUsd = resolved.source === 'fallback' ? listed : 0;
    cells.push(cell);
  }
  return cells;
}

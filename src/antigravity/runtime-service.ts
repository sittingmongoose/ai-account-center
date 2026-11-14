import { AntigravityError } from './errors';
import path from 'path';
import { getCcsDir } from '../utils/config-manager';
import type { AntigravityRuntime } from './runtime-composition';

export type AntigravityRuntimeFactory = (privateCcsDirectory: string) => AntigravityRuntime | null;
const runtimes = new Map<string, AntigravityRuntime | null>();
/** When a scope had no runtime (no saved profile yet) it is asked again after this long. */
export const ANTIGRAVITY_RUNTIME_RETRY_MS = 15_000;
const emptyChecks = new Map<string, number>();
let factory: AntigravityRuntimeFactory | null = null;

/** Explicit application composition; importing the API never initializes native adapters. */
export function configureAntigravityRuntimeFactory(next: AntigravityRuntimeFactory): void {
  if (factory && factory !== next)
    throw new AntigravityError('Antigravity runtime is already configured.');
  factory = next;
}

/**
 * A scope without saved profiles has no runtime. It is asked again after
 * ANTIGRAVITY_RUNTIME_RETRY_MS, so a first profile added by
 * `ai-account-center antigravity signin` appears without a server restart.
 */
export function getAntigravityRuntime(
  scope = getCcsDir(),
  now: number = Date.now()
): AntigravityRuntime | null {
  if (!factory) return null;
  const key = path.resolve(scope);
  const cached = runtimes.get(key);
  if (cached) return cached;
  if (runtimes.has(key) && now - (emptyChecks.get(key) ?? 0) < ANTIGRAVITY_RUNTIME_RETRY_MS)
    return null;
  emptyChecks.set(key, now);
  const created = factory(key);
  runtimes.set(key, created);
  return created;
}

/** The server's start: its scope's runtime is built now (it has no background work of its own). */
export function startAntigravityRuntime(scope = getCcsDir()): AntigravityRuntime | null {
  return getAntigravityRuntime(scope);
}

/** The server's cleanup. The runtime holds no timers, so there is nothing to stop. */
export function stopAntigravityRuntime(): void {}

export function hasAntigravityProfiles(scope = getCcsDir()): boolean {
  try {
    return getAntigravityRuntime(scope)?.hasProfiles() === true;
  } catch {
    return false;
  }
}

export function getAntigravityAccounts(refresh: boolean, scope = getCcsDir()) {
  const runtime = getAntigravityRuntime(scope);
  return runtime ? runtime.getAccounts({ refresh }) : Promise.resolve([]);
}

export function getCachedAntigravityAccounts(scope = getCcsDir()) {
  try {
    return getAntigravityRuntime(scope)?.cachedAccounts() ?? [];
  } catch {
    return [];
  }
}

export function readSelectedAntigravityProfileId(scope = getCcsDir()): Promise<string | null> {
  try {
    return getAntigravityRuntime(scope)?.readSelectedProfileId() ?? Promise.resolve(null);
  } catch {
    return Promise.resolve(null);
  }
}

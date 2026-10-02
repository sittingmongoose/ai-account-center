import { AntigravityError } from './errors';
import path from 'path';
import { getCcsDir } from '../utils/config-manager';
import {
  disabledAntigravityAutoSwitchStatus,
  type AntigravityRuntime,
} from './runtime-composition';

export type AntigravityRuntimeFactory = (privateCcsDirectory: string) => AntigravityRuntime | null;
const runtimes = new Map<string, AntigravityRuntime | null>();
/** When a scope had no runtime (no saved profile yet) it is asked again after this long. */
export const ANTIGRAVITY_RUNTIME_RETRY_MS = 15_000;
const emptyChecks = new Map<string, number>();
let factory: AntigravityRuntimeFactory | null = null;
/** The scope whose runtime runs its monitor (the server's own scope). */
let startedScope: string | null = null;

/** Explicit application composition; importing the API never initializes native adapters. */
export function configureAntigravityRuntimeFactory(next: AntigravityRuntimeFactory): void {
  if (factory && factory !== next)
    throw new AntigravityError('Antigravity runtime is already configured.');
  factory = next;
}

/**
 * A scope without saved profiles has no runtime. It is asked again after
 * ANTIGRAVITY_RUNTIME_RETRY_MS, so a first profile added by
 * `ai-account-center antigravity signin` appears without a server restart; a
 * runtime created later for the started scope starts its monitor then.
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
  if (created && startedScope === key) created.start();
  return created;
}

/** The server's start: its scope's runtime monitor runs now, or once a runtime exists. */
export function startAntigravityRuntime(scope = getCcsDir()): AntigravityRuntime | null {
  startedScope = path.resolve(scope);
  const runtime = getAntigravityRuntime(scope);
  runtime?.start();
  return runtime;
}

/** The server's cleanup: stop the started scope's monitor, created now or later. */
export function stopAntigravityRuntime(): void {
  const key = startedScope;
  startedScope = null;
  if (key) runtimes.get(key)?.stop();
}

export function getAntigravityAutoSwitchStatus() {
  try {
    return getAntigravityRuntime()?.getAutoSwitchStatus() ?? disabledAntigravityAutoSwitchStatus();
  } catch {
    return {
      ...disabledAntigravityAutoSwitchStatus(),
      outcome: 'error' as const,
      message: 'Antigravity automatic switching could not complete its check.',
    };
  }
}

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

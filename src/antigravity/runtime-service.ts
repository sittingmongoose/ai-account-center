import { AntigravityError } from './errors';
import path from 'path';
import { getCcsDir } from '../utils/config-manager';
import {
  disabledAntigravityAutoSwitchStatus,
  type AntigravityRuntime,
} from './runtime-composition';

export type AntigravityRuntimeFactory = (privateCcsDirectory: string) => AntigravityRuntime | null;
const runtimes = new Map<string, AntigravityRuntime | null>();
let factory: AntigravityRuntimeFactory | null = null;

/** Explicit application composition; importing the API never initializes native adapters. */
export function configureAntigravityRuntimeFactory(next: AntigravityRuntimeFactory): void {
  if (factory && factory !== next)
    throw new AntigravityError('Antigravity runtime is already configured.');
  factory = next;
}

export function getAntigravityRuntime(scope = getCcsDir()): AntigravityRuntime | null {
  if (!factory) return null;
  const key = path.resolve(scope);
  if (!runtimes.has(key)) runtimes.set(key, factory(key));
  return runtimes.get(key) ?? null;
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

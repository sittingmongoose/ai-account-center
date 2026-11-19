/**
 * Per-route-group switches of the private Rust service (DESIGN-M3-COMPLETE section 4): `rustService.groups.<g>`
 * in config.yaml, under the service mode, which stays the ceiling. R1 adds `home`, R2 `status`, R5/A2 `analytics`.
 * An environment variable (`AAC_RUST_GROUP_HOME`, `AAC_RUST_GROUP_STATUS`, `AAC_RUST_GROUP_ANALYTICS`) wins over
 * the file; absent, unreadable and invalid values are off.
 */
import { loadUnifiedConfig } from '../config/unified-config-loader';

export type RustGroup = 'home' | 'status' | 'analytics' | 'analytics-writers';

export function rustGroupOn(group: RustGroup, env: NodeJS.ProcessEnv = process.env): boolean {
  let value: unknown = env[`AAC_RUST_GROUP_${group.toUpperCase()}`];
  if (value === undefined) {
    try {
      const groups = (loadUnifiedConfig()?.rustService as { groups?: unknown } | undefined)?.groups;
      value =
        groups && typeof groups === 'object'
          ? (groups as Record<string, unknown>)[group]
          : undefined;
    } catch {
      value = undefined;
    }
  }
  return value === 'on';
}

/**
 * R3: the store host's own switch, `rustService.store` in config.yaml (`AAC_RUST_STORE` wins).
 * The supervisor also requires mode `on` (no shadow is possible for the one writer) and the
 * store itself not disabled; this reads only the switch.
 */
export function rustStoreSwitchOn(env: NodeJS.ProcessEnv = process.env): boolean {
  let value: unknown = env.AAC_RUST_STORE;
  if (value === undefined) {
    try {
      value = (loadUnifiedConfig()?.rustService as { store?: unknown } | undefined)?.store;
    } catch {
      value = undefined;
    }
  }
  return value === 'on';
}

/**
 * G14: the Analytics writers' switch, `rustService.groups['analytics-writers']`
 * (`AAC_RUST_GROUP_ANALYTICS_WRITERS` wins). The writers own quota sampling and activity collection
 * only when the analytics read group is on and the store is hosted by the service (the one writer);
 * otherwise the TypeScript writers keep running, so nothing is left unwritten.
 */
export function rustAnalyticsWritersOn(env: NodeJS.ProcessEnv = process.env): boolean {
  let value: unknown = env.AAC_RUST_GROUP_ANALYTICS_WRITERS;
  if (value === undefined) {
    try {
      const groups = (loadUnifiedConfig()?.rustService as { groups?: unknown } | undefined)?.groups;
      value =
        groups && typeof groups === 'object'
          ? (groups as Record<string, unknown>)['analytics-writers']
          : undefined;
    } catch {
      value = undefined;
    }
  }
  return value === 'on' && rustGroupOn('analytics', env) && rustStoreSwitchOn(env);
}

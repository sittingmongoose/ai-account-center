/**
 * Per-route-group switches of the private Rust service (DESIGN-M3-COMPLETE section 4): `rustService.groups.<g>`
 * in config.yaml, under the service mode, which stays the ceiling. R1 adds `home`, R2 `status`, R4 `scoreboard`.
 * An environment variable (`AAC_RUST_GROUP_HOME`, `AAC_RUST_GROUP_STATUS`, `AAC_RUST_GROUP_SCOREBOARD`) wins over
 * the file; absent, unreadable and invalid values are off.
 */
import { loadUnifiedConfig } from '../config/unified-config-loader';

export type RustGroup = 'home' | 'status' | 'scoreboard';

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
 * R4 (DESIGN-M3-COMPLETE 1.2): the owned Status readers. The status group's fed path (R2) stays the
 * request path; with the store host on as well, the service reads the share itself and the front door
 * forwards its snapshots instead of reading the share in TypeScript. Without the store host the group
 * stays fed: report captures need the hosted writer.
 */
export function rustOwnedStatusOn(env: NodeJS.ProcessEnv = process.env): boolean {
  return rustGroupOn('status', env) && rustStoreSwitchOn(env);
}

/**
 * R4 (DESIGN-M3-COMPLETE 1.4): the owned Scoreboard. With the scoreboard group and the store host on,
 * the service seeds the catalog, ingests the runs and answers the view from the store in process;
 * TypeScript keeps the filter parse and auth, and forwards. Without the store host nothing moves.
 */
export function rustOwnedScoreboardOn(env: NodeJS.ProcessEnv = process.env): boolean {
  return rustGroupOn('scoreboard', env) && rustStoreSwitchOn(env);
}

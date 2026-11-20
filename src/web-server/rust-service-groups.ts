/**
 * Per-route-group switches of the private Rust service (DESIGN-M3-COMPLETE section 4): `rustService.groups.<g>`
 * in config.yaml, under the service mode, which stays the ceiling. R1 adds `home`, R2 `status`. An environment
 * variable (`AAC_RUST_GROUP_HOME`, `AAC_RUST_GROUP_STATUS`) wins over the file.
 *
 * `home` is default-ready: with the service mode on and no explicit setting, the fed Home answers (R1's parity
 * corpus and the kit's Home set passed; the measurement report is `README-A.md` in the rust-monday share folder).
 * `groups.home: off` restores D7's path (or not offered). `status` stays off by default. An explicit invalid
 * value is off; an absent or unreadable setting takes the group's default.
 */
import { loadUnifiedConfig } from '../config/unified-config-loader';

export type RustGroup =
  | 'home'
  | 'status'
  | 'scoreboard'
  | 'analytics'
  | 'analytics-writers'
  | 'collection'
  | 'writes'
  | 'app-updates'
  | 'renewal'
  | 'usageSnapshot'
  | 'claude-open';

/** The group's switch when neither the environment nor the config names it. */
const DEFAULT: Record<RustGroup, boolean> = {
  home: true,
  status: false,
  scoreboard: true,
  analytics: false,
  'analytics-writers': false,
  collection: false,
  writes: false,
  'app-updates': false,
  renewal: false,
  usageSnapshot: false,
  'claude-open': false,
};

export function rustGroupOn(group: RustGroup, env: NodeJS.ProcessEnv = process.env): boolean {
  let value: unknown = env[`AAC_RUST_GROUP_${group.toUpperCase().replace(/-/g, '_')}`];
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
  return value === undefined ? env.AAC_RUST_CUT_WORKER === '1' || DEFAULT[group] : value === 'on';
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
  return value === 'on' || (value === undefined && env.AAC_RUST_CUT_WORKER === '1');
}

/** Select the public Rust composition without changing the installed TS default. */
export function rustComposeSelected(env: NodeJS.ProcessEnv = process.env): boolean {
  if (env.AAC_RUST_COMPOSE !== undefined) return env.AAC_RUST_COMPOSE === '1';
  try {
    return loadUnifiedConfig()?.rustService?.frontDoor === 'rust';
  } catch {
    return false;
  }
}

/**
 * R4 (DESIGN-M3-COMPLETE 1.2): the owned Status readers. The status group's fed path (R2) stays the
 * request path; with the store host on as well, the service reads the share itself and the front door
 * forwards its snapshots instead of reading the share in TypeScript. Without the store host the group
 * stays fed: report captures need the hosted writer.
 */
export function rustOwnedStatusOn(env: NodeJS.ProcessEnv = process.env): boolean {
  return (
    process.platform !== 'win32' &&
    (env.AAC_RUST_CUT_WORKER === '1' || rustGroupOn('status', env)) &&
    rustStoreSwitchOn(env)
  );
}

/**
 * R4 (DESIGN-M3-COMPLETE 1.4): the owned Scoreboard. With the scoreboard group and the store host on,
 * the service seeds the catalog, ingests the runs and answers the view from the store in process;
 * TypeScript keeps the filter parse and auth, and forwards. Without the store host nothing moves.
 */
export function rustOwnedScoreboardOn(env: NodeJS.ProcessEnv = process.env): boolean {
  return (
    process.platform !== 'win32' &&
    (env.AAC_RUST_CUT_WORKER === '1' || rustGroupOn('scoreboard', env)) &&
    rustStoreSwitchOn(env)
  );
}

/** G14: move the writers only to a supported host with the analytics group and store on. */
export function rustAnalyticsWritersOn(env: NodeJS.ProcessEnv = process.env): boolean {
  return (
    process.platform !== 'win32' &&
    rustGroupOn('analytics-writers', env) &&
    rustGroupOn('analytics', env) &&
    rustStoreSwitchOn(env)
  );
}

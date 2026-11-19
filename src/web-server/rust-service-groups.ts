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

export type RustGroup = 'home' | 'status';

/** The group's switch when neither the environment nor the config names it. */
const DEFAULT: Record<RustGroup, boolean> = { home: true, status: false };

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
  return value === undefined ? DEFAULT[group] : value === 'on';
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

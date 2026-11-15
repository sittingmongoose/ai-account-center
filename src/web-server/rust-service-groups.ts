/**
 * Per-route-group switches of the private Rust service (DESIGN-M3-COMPLETE section 4): `rustService.groups.<g>`
 * in config.yaml, under the service mode, which stays the ceiling. R1 adds `home`. An environment variable
 * (`AAC_RUST_GROUP_HOME`) wins over the file; absent, unreadable and invalid values are off.
 */
import { loadUnifiedConfig } from '../config/unified-config-loader';

export type RustGroup = 'home';

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

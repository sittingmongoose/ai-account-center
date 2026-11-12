/**
 * One package-owned computer table, shared with the browser, Python reader and
 * Rust Status readers. It is shipped code, never caller/browser configuration.
 * Persisted sources keep their OS platform plus sshHost (Nas1's precedent):
 * Alexis is platform 'windows' + sshHost 'alexis-windows'. DashboardPlatform
 * stays unchanged so old packages read new registry/manifest files and vice versa.
 */
import os from 'os';
import path from 'path';

export type DashboardHost = keyof typeof import('../../../web-dashboard/public/computers.json');
export type RemoteDashboardHost = Exclude<DashboardHost, 'ubuntu'>;
export type HostOs = 'linux' | 'mac' | 'windows';
export type HelperPlatform = 'ubuntu' | 'mac' | 'windows';
export type MachineId = 'vm' | Exclude<DashboardHost, 'ubuntu'>;
export interface Computer {
  label: string;
  os: HostOs;
  sshAlias: string | null;
  machineId: MachineId;
  machineLabel: string;
  shortLabel: string;
  subtitle: string;
  names: readonly string[];
  legacy: boolean;
}
export const COMPUTER_REGISTRY_FILE = path.resolve(
  __dirname,
  '../../../web-dashboard/public/computers.json'
);
export const COMPUTERS: Readonly<Record<DashboardHost, Computer>> = Object.freeze(
  Object.fromEntries(
    Object.entries(require(COMPUTER_REGISTRY_FILE)).map(([id, value]) => {
      const computer = value as Computer;
      return [id, Object.freeze({ ...computer, names: Object.freeze([...computer.names]) })];
    })
  ) as Record<DashboardHost, Computer>
);
export const DASHBOARD_HOSTS = Object.freeze(Object.keys(COMPUTERS) as DashboardHost[]);
/**
 * Immutable storage compatibility allowlist from d5ec0fc64, not a computer inventory.
 * Its readers reject later hosts/over 40 update rows. Do not expand this with the registry.
 * The registry's `legacy` flag predates Nas1 and describes a different boundary.
 */
export const LEGACY_STORAGE_HOSTS: readonly DashboardHost[] = Object.freeze([
  'ubuntu',
  'mac',
  'windows',
  'nas1',
]);
export const REMOTE_HOSTS = Object.freeze(
  DASHBOARD_HOSTS.filter((id): id is RemoteDashboardHost => id !== 'ubuntu')
);
export const HOST_OS = Object.freeze(
  Object.fromEntries(DASHBOARD_HOSTS.map((id) => [id, COMPUTERS[id].os]))
) as Readonly<Record<DashboardHost, HostOs>>;
export const HOST_LABELS = Object.freeze(
  Object.fromEntries(DASHBOARD_HOSTS.map((id) => [id, COMPUTERS[id].label]))
) as Readonly<Record<DashboardHost, string>>;
export const SSH_ALIASES = Object.freeze(
  Object.fromEntries(REMOTE_HOSTS.map((id) => [id, COMPUTERS[id].sshAlias]))
) as Readonly<Record<RemoteDashboardHost, string>>;
export const MACHINE_IDS = Object.freeze(DASHBOARD_HOSTS.map((id) => COMPUTERS[id].machineId));
export const MACHINE_LABELS = Object.freeze(
  Object.fromEntries(
    DASHBOARD_HOSTS.map((id) => [COMPUTERS[id].machineId, COMPUTERS[id].machineLabel])
  )
) as Readonly<Record<MachineId, string>>;
/** Compatibility export; consumers use the registry's maps. */
export const NAS1_SSH_ALIAS = SSH_ALIASES.nas1;
export function helperPlatform(host: DashboardHost): HelperPlatform {
  const kind = HOST_OS[host];
  return kind === 'linux' ? 'ubuntu' : kind;
}
export function sourceComputer(source: {
  platform: HelperPlatform;
  sshHost?: string | null;
}): DashboardHost {
  return (
    DASHBOARD_HOSTS.find(
      (id) =>
        source.sshHost &&
        COMPUTERS[id].sshAlias === source.sshHost &&
        helperPlatform(id) === source.platform
    ) ?? source.platform
  );
}
/** Exact known names first: AlexisGamingPC must never fall through to generic 'PC'. */
export function namedComputer(name: string): DashboardHost | null {
  const lower = name.toLowerCase();
  return (
    DASHBOARD_HOSTS.find((id) =>
      [id, COMPUTERS[id].sshAlias, ...COMPUTERS[id].names].some(
        (value) => value?.toLowerCase() === lower
      )
    ) ??
    [...DASHBOARD_HOSTS]
      .sort((a, b) => Number(COMPUTERS[a].legacy) - Number(COMPUTERS[b].legacy))
      .find((id) => COMPUTERS[id].names.some((value) => lower.includes(value.toLowerCase()))) ??
    null
  );
}
export function localComputer(
  hostname = os.hostname(),
  platform: NodeJS.Platform = process.platform
): DashboardHost {
  const known = namedComputer(hostname);
  const kind = platform === 'win32' ? 'windows' : platform === 'darwin' ? 'mac' : 'linux';
  return known && HOST_OS[known] === kind ? known : kind === 'linux' ? 'ubuntu' : kind;
}

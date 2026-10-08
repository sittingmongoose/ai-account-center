/**
 * The fixed computers the dashboard reaches: the local Ubuntu computer, the
 * Mac, the Windows computer and Nas1. Nas1 is a second Ubuntu computer reached
 * over the fixed ssh alias below, so its helpers run there with
 * --platform ubuntu and the Python helpers learn no new platform. Persisted
 * usage-source and registry files keep encoding a Nas1 source as platform
 * 'ubuntu' with sshHost 'nas1-agent': DashboardPlatform is intentionally
 * unchanged, so an older package still reads those files. No configurable
 * hosts, commands or paths.
 */

export const DASHBOARD_HOSTS = ['ubuntu', 'mac', 'windows', 'nas1'] as const;
export type DashboardHost = (typeof DASHBOARD_HOSTS)[number];
export type HostOs = 'linux' | 'mac' | 'windows';
export const HOST_OS: Readonly<Record<DashboardHost, HostOs>> = Object.freeze({
  ubuntu: 'linux',
  mac: 'mac',
  windows: 'windows',
  nas1: 'linux',
});
export const HOST_LABELS: Readonly<Record<DashboardHost, string>> = Object.freeze({
  ubuntu: 'Ubuntu',
  mac: 'Mac',
  windows: 'Windows',
  nas1: 'Nas1',
});
/** The second Ubuntu computer's fixed ssh alias; no Claude desktop launcher names it. */
export const NAS1_SSH_ALIAS = 'nas1-agent';
/** The --platform a fixed helper receives on that host (Python checks it against its own OS). */
export function helperPlatform(host: DashboardHost): 'ubuntu' | 'mac' | 'windows' {
  return HOST_OS[host] === 'linux' ? 'ubuntu' : (host as 'mac' | 'windows');
}

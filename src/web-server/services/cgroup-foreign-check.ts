/**
 * Read-only startup/status check: warn when the dashboard's cgroup holds
 * foreign processes (apps it launched that should have escaped to app.slice).
 *
 * Why: with KillMode=control-group a dashboard restart SIGKILLs everything in
 * its cgroup. launchApp() starts every app outside the cgroup, but a launch
 * from before the fix (or any other leak) leaves foreign processes caged;
 * this check surfaces them so nobody restarts the dashboard blind.
 *
 * Read-only by design: /proc/cgroup + /proc/<pid>/comm only, no process names
 * beyond the comm basename, no environ, no signals. Out of caution it never
 * touches the processes it reports.
 */

import { readdirSync, readFileSync } from 'fs';
import { createLogger } from '../../services/logging';

const logger = createLogger('cgroup-foreign-check');

export interface ForeignProcess {
  pid: number;
  /** /proc/<pid>/comm basename (no arguments, no secrets). */
  name: string;
}

export interface CgroupScanResult {
  /** The dashboard's own cgroup path (without the "0::" prefix). */
  selfCgroup: string;
  /** Foreign processes matching known launched-app names (warn). */
  apps: ForeignProcess[];
  /** Other foreign processes (info only). */
  others: ForeignProcess[];
}

export interface CgroupScanDeps {
  platform?: NodeJS.Platform;
  selfPid?: number;
  readTextFile?: (path: string) => string | null;
  listDir?: (path: string) => string[];
}

/** Basenames of apps the dashboard is known to launch (launchApp + updater). */
const KNOWN_APP_PATTERN = /^(codex|chatgpt|claude|antigravity|cursor|electron|tmux)/i;

function defaultReadTextFile(path: string): string | null {
  try {
    return readFileSync(path, 'utf8');
  } catch {
    return null;
  }
}

function defaultListDir(path: string): string[] {
  try {
    return readdirSync(path);
  } catch {
    return [];
  }
}

/** Parse this process's cgroup path from /proc/<pid>/cgroup text. Supports v2 and v1. */
export function parseSelfCgroup(cgroupText: string): string | null {
  for (const line of cgroupText.split('\n')) {
    const trimmed = line.trim();
    if (trimmed === '') continue;
    // v2: "0::/user.slice/..." ; v1: "<id>:<controllers>:/path"
    if (trimmed.startsWith('0::')) return trimmed.slice(3) || '/';
    const firstColon = trimmed.indexOf(':');
    const secondColon = firstColon === -1 ? -1 : trimmed.indexOf(':', firstColon + 1);
    if (firstColon !== -1 && secondColon !== -1) {
      const controllers = trimmed.slice(firstColon + 1, secondColon).split(',');
      if (controllers.includes('systemd')) return trimmed.slice(secondColon + 1) || '/';
    }
  }
  return null;
}

function inSameCgroupTree(procCgroupText: string, selfCgroup: string): boolean {
  for (const line of procCgroupText.split('\n')) {
    const trimmed = line.trim();
    if (trimmed === '') continue;
    let path: string | null = null;
    if (trimmed.startsWith('0::')) {
      path = trimmed.slice(3) || '/';
    } else {
      const firstColon = trimmed.indexOf(':');
      const secondColon = firstColon === -1 ? -1 : trimmed.indexOf(':', firstColon + 1);
      if (firstColon === -1 || secondColon === -1) continue;
      const controllers = trimmed.slice(firstColon + 1, secondColon).split(',');
      if (!controllers.includes('systemd')) continue;
      path = trimmed.slice(secondColon + 1) || '/';
    }
    if (
      path === selfCgroup ||
      path.startsWith(selfCgroup.endsWith('/') ? selfCgroup : `${selfCgroup}/`)
    ) {
      return true;
    }
  }
  return false;
}

/**
 * Scan /proc for processes living in the dashboard's own cgroup.
 * Returns null on non-Linux platforms or when cgroup info is unavailable.
 */
export function scanCgroupForForeignProcesses(deps: CgroupScanDeps = {}): CgroupScanResult | null {
  const platform = deps.platform ?? process.platform;
  if (platform !== 'linux') return null;
  const readTextFile = deps.readTextFile ?? defaultReadTextFile;
  const listDir = deps.listDir ?? defaultListDir;
  const selfPid = deps.selfPid ?? process.pid;

  const selfText = readTextFile(`/proc/${selfPid}/cgroup`);
  if (selfText === null) return null;
  const selfCgroup = parseSelfCgroup(selfText);
  if (selfCgroup === null) return null;

  const apps: ForeignProcess[] = [];
  const others: ForeignProcess[] = [];
  for (const entry of listDir('/proc')) {
    if (!/^[0-9]+$/.test(entry)) continue;
    const pid = Number(entry);
    if (pid === selfPid) continue;
    const procCgroup = readTextFile(`/proc/${pid}/cgroup`);
    if (procCgroup === null) continue; // Exited mid-scan.
    if (!inSameCgroupTree(procCgroup, selfCgroup)) continue;
    const comm = readTextFile(`/proc/${pid}/comm`);
    const name = comm === null ? '?' : (comm.trim().split('\n')[0] ?? '?');
    const foreign = { pid, name };
    if (KNOWN_APP_PATTERN.test(name)) apps.push(foreign);
    else others.push(foreign);
  }
  apps.sort((a, b) => a.pid - b.pid);
  others.sort((a, b) => a.pid - b.pid);
  return { selfCgroup, apps, others };
}

export interface CgroupForeignCheckOptions extends CgroupScanDeps {
  /** Re-check interval in ms. Default: hourly. */
  intervalMs?: number;
  warn?: (event: string, message: string, context?: Record<string, unknown>) => void;
  info?: (event: string, message: string, context?: Record<string, unknown>) => void;
}

const DEFAULT_INTERVAL_MS = 60 * 60 * 1000;

/**
 * Run the check once now and then hourly. Warns (not errors) so a caged
 * launch degrades to a loud log line, never a crash. Returns a stop function.
 */
export function startCgroupForeignCheck(options: CgroupForeignCheckOptions = {}): () => void {
  const warn = options.warn ?? ((event, message, context) => logger.warn(event, message, context));
  const info = options.info ?? ((event, message, context) => logger.info(event, message, context));

  const runOnce = () => {
    let result: CgroupScanResult | null;
    try {
      result = scanCgroupForForeignProcesses(options);
    } catch {
      return; // Best-effort: a failed scan must never break startup.
    }
    if (result === null) return;
    if (result.apps.length > 0) {
      const names = result.apps.map((p) => `${p.name} (pid ${p.pid})`).join(', ');
      warn(
        'web-server.cgroup_foreign_apps',
        `Dashboard cgroup ${result.selfCgroup} holds launched app(s) that should run outside it: ${names}. ` +
          'A dashboard restart would kill them (KillMode=control-group). See launchApp().',
        { cgroup: result.selfCgroup, apps: result.apps, otherCount: result.others.length }
      );
    } else if (result.others.length > 0) {
      info(
        'web-server.cgroup_foreign_processes',
        `Dashboard cgroup ${result.selfCgroup} holds ${result.others.length} other process(es). ` +
          'A dashboard restart would kill them (KillMode=control-group).',
        { cgroup: result.selfCgroup, otherCount: result.others.length }
      );
    }
  };

  runOnce();
  const timer = setInterval(runOnce, options.intervalMs ?? DEFAULT_INTERVAL_MS);
  if (typeof timer.unref === 'function') timer.unref();
  return () => clearInterval(timer);
}

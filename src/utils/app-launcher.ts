/**
 * Launch long-lived desktop/daemon apps OUTSIDE the dashboard's own cgroup.
 *
 * Why: the dashboard runs as a systemd user unit with KillMode=control-group.
 * A plain detached spawn keeps the child in the unit's cgroup, so a dashboard
 * restart or deploy would SIGKILL the launched app AND everything it started
 * (editors, terminals, user work). Verified 2026-10-05: the Codex app-server
 * and ChatGPT/Codex Desktop launched by auto-switch activation, plus ~60
 * descendant processes, all lived inside ccs-dashboard.service's cgroup.
 *
 * On Linux every launch therefore goes through a transient user service
 * (`systemd-run --user --unit=aac-launch-<app>-<id> ...`), which re-parents
 * the app to app.slice -- verified to escape both the service cgroup and the
 * scope cgroup. Transient *scopes* are deliberately NOT used: this host's
 * systemd aborts `--scope` units ("Unit ... not loaded" inside the send
 * loop), so scope launches silently do nothing while reporting success.
 *
 * When systemd-run is missing there is no safe launch: fail closed with an
 * error rather than cage the app in the service cgroup.
 */

import { accessSync, constants } from 'fs';
import { spawn, type ChildProcess, type SpawnOptions } from 'child_process';

export interface AppLaunchSpec {
  /** Short lowercase name used in the transient unit (aac-launch-<app>-<id>). */
  app: string;
  /** Executable path. */
  exe: string;
  /** Arguments AFTER argv0 (argv0 is replaced by `exe`, same as spawn). */
  args: string[];
  /** Working directory for the launched app. */
  cwd?: string;
  /** Full replacement environment for the launched app. */
  env?: Record<string, string | undefined>;
  /** Daemon log file (appended stdout+stderr). GUI apps omit it. */
  logFile?: string;
}

export type AppLaunchRoute = 'systemd-service' | 'direct';

export interface AppLaunchResult {
  route: AppLaunchRoute;
  /** Transient unit name (systemd-service route only). */
  unit?: string;
}

export interface AppLauncherDeps {
  platform?: NodeJS.Platform;
  /** Resolved systemd-run path, or null when unavailable. Defaults to a PATH lookup. */
  systemdRunPath?: string | null;
  spawn?: (file: string, args: string[], options: SpawnOptions) => ChildProcess;
  randomSuffix?: () => string;
  pid?: number;
}

const SYSTEMD_RUN_CANDIDATES = ['/usr/bin/systemd-run', '/bin/systemd-run'];

/** Sanitize an app name into a systemd-safe unit fragment. */
export function sanitizeAppName(app: string): string {
  const cleaned = app
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
  return cleaned === '' ? 'app' : cleaned;
}

export function transientUnitName(app: string, pid: number, suffix: string): string {
  return `aac-launch-${sanitizeAppName(app)}-${pid}-${suffix}`;
}

function defaultRandomSuffix(): string {
  return Math.random().toString(36).slice(2, 10);
}

function defaultSystemdRunPath(): string | null {
  for (const candidate of SYSTEMD_RUN_CANDIDATES) {
    try {
      accessSync(candidate, constants.X_OK);
      return candidate;
    } catch {
      // Try the next candidate.
    }
  }
  return null;
}

/**
 * Build the systemd-run argv for a launch. Exported for tests.
 * --collect drops the unit once the app exits so repeated launches never collide.
 */
export function buildSystemdRunArgs(spec: AppLaunchSpec, unit: string): string[] {
  const argv = ['--user', `--unit=${unit}`, '--collect'];
  if (spec.cwd !== undefined && spec.cwd !== '') {
    argv.push(`--working-directory=${spec.cwd}`);
  }
  if (spec.env !== undefined) {
    for (const [key, value] of Object.entries(spec.env)) {
      if (value !== undefined) argv.push(`--setenv=${key}=${value}`);
    }
  }
  if (spec.logFile !== undefined && spec.logFile !== '') {
    argv.push(`--property=StandardOutput=append:${spec.logFile}`);
    argv.push(`--property=StandardError=append:${spec.logFile}`);
  }
  argv.push('--', spec.exe, ...spec.args);
  return argv;
}

function waitForExit(child: ChildProcess): Promise<number | null> {
  return new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code) => resolve(code));
  });
}

/**
 * Launch an app outside the dashboard cgroup (see module docstring).
 * On non-Linux platforms (no systemd user cgroups) this is a plain detached spawn.
 */
export async function launchApp(
  spec: AppLaunchSpec,
  deps: AppLauncherDeps = {}
): Promise<AppLaunchResult> {
  const platform = deps.platform ?? process.platform;
  const spawnFn = deps.spawn ?? spawn;

  if (platform !== 'linux') {
    const child = spawnFn(spec.exe, spec.args, {
      cwd: spec.cwd,
      env: spec.env,
      stdio: 'ignore',
      detached: true,
    });
    if (typeof child.unref === 'function') child.unref();
    await new Promise<void>((resolve, reject) => {
      child.once('error', reject);
      child.once('spawn', () => resolve());
    });
    return { route: 'direct' };
  }

  const systemdRun =
    deps.systemdRunPath === undefined ? defaultSystemdRunPath() : deps.systemdRunPath;
  if (systemdRun === null) {
    throw new Error(
      `Cannot launch ${spec.app} outside the dashboard cgroup: systemd-run is unavailable. ` +
        'Refusing to start it inside ccs-dashboard.service, where a dashboard restart would kill it.'
    );
  }
  const pid = deps.pid ?? process.pid;
  const suffix = deps.randomSuffix ? deps.randomSuffix() : defaultRandomSuffix();
  const unit = transientUnitName(spec.app, pid, suffix);
  const child = spawnFn(systemdRun, buildSystemdRunArgs(spec, unit), { stdio: 'ignore' });
  let code: number | null;
  try {
    code = await waitForExit(child);
  } catch (error) {
    throw new Error(
      `Cannot launch ${spec.app} outside the dashboard cgroup: systemd-run failed to start (${error}).`
    );
  }
  if (code !== 0) {
    throw new Error(
      `Cannot launch ${spec.app} outside the dashboard cgroup: systemd-run exited with code ${code}.`
    );
  }
  return { route: 'systemd-service', unit };
}

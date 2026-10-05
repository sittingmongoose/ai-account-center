import { execFile, spawnSync } from 'child_process';
import fs from 'fs';
import path from 'path';
import type { AntigravityRuntimeServiceProblem } from './usage-contract';

/**
 * Why the installed Antigravity runtime service is not running, found
 * read-only (docs/antigravity-runtime.md, "When the runtime service fails").
 *
 * The service runs `/usr/bin/python3 -I <bundle>/lib/resident_main.py`. The
 * packaged `scripts/antigravity/runtime_health.py` asks that same interpreter,
 * with `-I -B` (no bytecode written into the immutable bundle), whether the
 * bundle's pinned parser loads; `systemctl --user show` reports a failed unit.
 * Neither reads accounts, credentials, settings or history, and neither starts,
 * stops or changes anything.
 */

export const ANTIGRAVITY_RUNTIME_UNIT = 'ai-account-center-antigravity.service';
const SERVICE_PYTHON = '/usr/bin/python3';
const PARSER_REASONS = [
  'missing-python-module',
  'parser-mismatch',
  'parser-import-failed',
] as const;
const REASONS: ReadonlyArray<AntigravityRuntimeServiceProblem['reason']> = [
  ...PARSER_REASONS,
  'service-failed',
];
const MODULE = /^[A-Za-z_][A-Za-z0-9_.]{0,63}$/;
const MINOR = /^\d{1,2}\.\d{1,3}$/;
const OUTPUT_LIMIT = 64 * 1024;

export function defaultRuntimeHealthScript(): string {
  return path.resolve(__dirname, '../../scripts/antigravity/runtime_health.py');
}

/** A bounded public copy; anything unexpected is dropped or reads as unknown. */
export function publicRuntimeServiceProblem(
  value: unknown
): AntigravityRuntimeServiceProblem | null {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const reason = record.reason as AntigravityRuntimeServiceProblem['reason'];
  if (!REASONS.includes(reason)) return null;
  const text = (item: unknown, pattern: RegExp) =>
    typeof item === 'string' && pattern.test(item) ? item : null;
  const exitStatus =
    Number.isInteger(record.exitStatus) &&
    (record.exitStatus as number) >= 0 &&
    (record.exitStatus as number) <= 255
      ? (record.exitStatus as number)
      : null;
  return {
    reason,
    module: text(record.module, MODULE),
    python: text(record.python, MINOR),
    builtFor: text(record.builtFor, MINOR),
    exitStatus,
  };
}

/** One JSON line from runtime_health.py; a parser problem, or null when none is known. */
export function parserProblemFromProbe(
  raw: string | null
): AntigravityRuntimeServiceProblem | null {
  if (raw === null || raw.length > OUTPUT_LIMIT) return null;
  let value: unknown;
  try {
    value = JSON.parse(raw.trim());
  } catch {
    return null;
  }
  if (typeof value !== 'object' || value === null) return null;
  const record = value as Record<string, unknown>;
  if (record.ok === true) return null;
  if (!(PARSER_REASONS as readonly unknown[]).includes(record.reason)) return null;
  return publicRuntimeServiceProblem({ ...record, exitStatus: null });
}

/** `systemctl --user show` output; a failed unit, or null when it is not failed. */
export function unitProblemFromShow(raw: string | null): AntigravityRuntimeServiceProblem | null {
  if (raw === null || raw.length > OUTPUT_LIMIT) return null;
  const fields = new Map<string, string>();
  for (const line of raw.split('\n')) {
    const index = line.indexOf('=');
    if (index > 0) fields.set(line.slice(0, index), line.slice(index + 1).trim());
  }
  if (fields.get('ActiveState') !== 'failed') return null;
  const status = Number(fields.get('ExecMainStatus'));
  return {
    reason: 'service-failed',
    module: null,
    python: null,
    builtFor: null,
    exitStatus: Number.isInteger(status) && status > 0 && status <= 255 ? status : null,
  };
}

/** The words after "Runtime service failed: ". */
export function runtimeServiceProblemDetail(problem: AntigravityRuntimeServiceProblem): string {
  const module = problem.module ?? 'unknown';
  const moved =
    problem.python && problem.builtFor && problem.python !== problem.builtFor
      ? ` (system Python is ${problem.python}; the runtime bundle was built for ${problem.builtFor})`
      : '';
  if (problem.reason === 'missing-python-module') return `missing Python module ${module}${moved}`;
  if (problem.reason === 'parser-mismatch')
    return `Python module ${module} is not the pinned version${moved}`;
  if (problem.reason === 'parser-import-failed')
    return `Python module ${module} does not load${moved}`;
  return problem.exitStatus !== null ? `exit status ${problem.exitStatus}` : 'the unit failed';
}

export function describeRuntimeServiceProblem(problem: AntigravityRuntimeServiceProblem): string {
  return `Runtime service failed: ${runtimeServiceProblemDetail(problem)}`;
}

export function isRuntimeParserProblem(problem: AntigravityRuntimeServiceProblem): boolean {
  return problem.reason !== 'service-failed';
}

/** Returns stdout of a zero exit, otherwise null. Never throws. */
export type RuntimeHealthRunner = (
  file: string,
  args: string[],
  timeoutMs: number
) => string | null;
export type AsyncRuntimeHealthRunner = (
  file: string,
  args: string[],
  timeoutMs: number
) => Promise<string | null>;

function probeEnvironment(): NodeJS.ProcessEnv {
  const env = { ...process.env };
  for (const key of ['PYTHONPATH', 'PYTHONHOME', 'PYTHONSTARTUP', 'PYTHONUSERBASE'])
    delete env[key];
  return env;
}

const runSync: RuntimeHealthRunner = (file, args, timeoutMs) => {
  try {
    const result = spawnSync(file, args, {
      encoding: 'utf8',
      timeout: timeoutMs,
      maxBuffer: OUTPUT_LIMIT,
      stdio: ['ignore', 'pipe', 'ignore'],
      env: probeEnvironment(),
    });
    return result.error || result.status !== 0 ? null : result.stdout;
  } catch {
    return null;
  }
};

const runAsync: AsyncRuntimeHealthRunner = (file, args, timeoutMs) =>
  new Promise((resolve) => {
    try {
      execFile(
        file,
        args,
        { encoding: 'utf8', timeout: timeoutMs, maxBuffer: OUTPUT_LIMIT, env: probeEnvironment() },
        (error, stdout) => resolve(error ? null : stdout)
      ).stdin?.end();
    } catch {
      resolve(null);
    }
  });

function probeArgs(bundleDirectory: string, script: string): string[] {
  return ['-I', '-B', script, bundleDirectory];
}

const UNIT_ARGS = [
  '--user',
  'show',
  ANTIGRAVITY_RUNTIME_UNIT,
  '--property=ActiveState,Result,ExecMainStatus',
];

export interface RuntimeHealthOptions {
  script?: string;
  python?: string;
}

/** For the read-only status command. A parser cause wins over a bare failed unit. */
export function diagnoseRuntimeService(
  bundleDirectory: string,
  run: RuntimeHealthRunner = runSync,
  options: RuntimeHealthOptions = {}
): AntigravityRuntimeServiceProblem | null {
  const script = options.script ?? defaultRuntimeHealthScript();
  const parser = parserProblemFromProbe(
    run(options.python ?? SERVICE_PYTHON, probeArgs(bundleDirectory, script), 15_000)
  );
  if (parser) return parser;
  return unitProblemFromShow(run('systemctl', UNIT_ARGS, 5_000));
}

export async function diagnoseRuntimeServiceAsync(
  bundleDirectory: string,
  run: AsyncRuntimeHealthRunner = runAsync,
  options: RuntimeHealthOptions = {}
): Promise<AntigravityRuntimeServiceProblem | null> {
  const script = options.script ?? defaultRuntimeHealthScript();
  const parser = parserProblemFromProbe(
    await run(options.python ?? SERVICE_PYTHON, probeArgs(bundleDirectory, script), 15_000)
  );
  if (parser) return parser;
  return unitProblemFromShow(await run('systemctl', UNIT_ARGS, 5_000));
}

function socketPresent(socketPath: string): boolean {
  try {
    return fs.lstatSync(socketPath).isSocket();
  } catch {
    return false;
  }
}

/**
 * The dashboard's reader: no probe while the service socket exists, and at
 * most one probe per `ttlMs` while it is missing.
 */
export function createRuntimeServiceProblemReader(
  installed: { bundleDirectory: string; socketPath: string },
  options: {
    diagnose?: (bundleDirectory: string) => Promise<AntigravityRuntimeServiceProblem | null>;
    now?: () => number;
    ttlMs?: number;
  } = {}
): () => Promise<AntigravityRuntimeServiceProblem | null> {
  const diagnose = options.diagnose ?? ((bundle: string) => diagnoseRuntimeServiceAsync(bundle));
  const now = options.now ?? Date.now;
  const ttl = options.ttlMs ?? 60_000;
  let cached: { at: number; value: AntigravityRuntimeServiceProblem | null } | null = null;
  let pending: Promise<AntigravityRuntimeServiceProblem | null> | null = null;
  return async () => {
    if (socketPresent(installed.socketPath)) {
      cached = null;
      return null;
    }
    const at = now();
    if (cached && at - cached.at < ttl) return cached.value;
    pending ??= diagnose(installed.bundleDirectory)
      .catch(() => null)
      .then((value) => {
        cached = { at, value: publicRuntimeServiceProblem(value) };
        pending = null;
        return cached.value;
      });
    return pending;
  };
}

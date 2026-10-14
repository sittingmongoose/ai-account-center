import { ValidationError, NetworkError } from '../../errors/error-types';
import { execFile } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { resolveAnalyticsRemoteHosts } from './analytics-remote-transport';
import { isSafeUsageSshAlias } from './additional-usage-transport';
import { helperPlatform } from './dashboard-hosts';
import {
  isRecord,
  parseUsageCandidate,
  parseUsageReading,
  safeUsageText,
  usageTimestamp,
  USAGE_FAILURE_CODES,
  type UsageHelperRequest,
  type UsageHelperResponse,
  type UsageHost,
} from './usage-failover-contract';

const MODULES = [
  'failover_usage',
  'failover_core',
  'failover_additional',
  'failover_special',
  'desktop_helpers',
  'desktop_usage',
  'cursor_usage',
  'plan_common',
  'plan_usage',
  'muse_console',
  'muse_native',
  'claude_usage',
] as const;
const MAX_BYTES = 2 * 1024 * 1024;
const BOOTSTRAP =
  "import sys,json,io;_p=json.loads(sys.stdin.buffer.read().decode('utf-8'));sys.stdin=io.StringIO(json.dumps(_p['request']));exec(compile(_p['helperSource'],'managed-usage','exec'))";

/** Reuse the installed private environment, including the Mac wallet's curl_cffi dependency. */
export function usageHelperCommand(platform: UsageHost['platform']): string {
  if (platform !== 'windows') {
    const quoted = `'${BOOTSTRAP.replace(/'/g, `'"'"'`)}'`;
    return `if [ -x "$HOME/.ccs/claude-session-migration/venv/bin/python3" ]; then exec "$HOME/.ccs/claude-session-migration/venv/bin/python3" -c ${quoted}; else exec /usr/bin/python3 -c ${quoted}; fi`;
  }
  const script = [
    "$ErrorActionPreference = 'Stop'",
    "$ProgressPreference = 'SilentlyContinue'",
    "$env:PYTHONIOENCODING = 'utf-8'",
    "$env:PYTHONUTF8 = '1'",
    "$venv = [IO.Path]::Combine($HOME, '.ccs', 'claude-session-migration', 'venv', 'Scripts', 'python.exe')",
    '$python = if (Test-Path -LiteralPath $venv -PathType Leaf) { $venv } else { (Get-Command python3.exe,python.exe -ErrorAction SilentlyContinue | Select-Object -First 1).Source }',
    'if (-not $python) { exit 1 }',
    `& $python -c '${BOOTSTRAP.replace(/'/g, "''")}'`,
    'exit $LASTEXITCODE',
  ].join('; ');
  return `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, 'utf16le').toString('base64')}`;
}

/** Resolve only the known installed environment; never install or search arbitrary account paths. */
export function localUsagePython(
  request: Pick<UsageHelperRequest, 'homeDir' | 'ccsDir'>,
  platform: NodeJS.Platform = process.platform
): string {
  const venv = path.join(
    request.ccsDir ?? path.join(request.homeDir ?? os.homedir(), '.ccs'),
    'claude-session-migration',
    'venv',
    platform === 'win32' ? 'Scripts/python.exe' : 'bin/python3'
  );
  try {
    if (fs.statSync(venv).isFile()) {
      fs.accessSync(venv, platform === 'win32' ? fs.constants.F_OK : fs.constants.X_OK);
      return venv;
    }
  } catch {
    // Hosts without the installed environment can still collect standard-library providers.
  }
  return platform === 'win32' ? 'python' : '/usr/bin/python3';
}
/** Sources come from this package, never a request, a remote installation, or an account file. */
export async function usageHelperBundle(): Promise<string> {
  const root = path.resolve(__dirname, '../../..');
  const modules: Record<string, string> = {};
  for (const name of MODULES) {
    modules[name] = await fs.promises.readFile(
      path.join(root, 'scripts/account-usage', `${name}.py`),
      'utf8'
    );
  }
  modules.opencode_console = await fs.promises.readFile(
    path.join(root, 'browser-bridge/opencode-muse/native-host/opencode_console.py'),
    'utf8'
  );
  if (Buffer.byteLength(JSON.stringify(modules)) > MAX_BYTES)
    throw new ValidationError('Usage helper unavailable.');
  // In-memory imports keep credentials and helper source off remote disks. Python itself reads stdin.
  return [
    'import sys,json,importlib.abc,importlib.util',
    `_sources=json.loads(${JSON.stringify(JSON.stringify(modules))})`,
    'class _Bundle(importlib.abc.MetaPathFinder,importlib.abc.Loader):',
    ' def find_spec(self,fullname,path=None,target=None):',
    '  return importlib.util.spec_from_loader(fullname,self) if fullname in _sources else None',
    ' def create_module(self,spec): return None',
    ' def exec_module(self,module):',
    "  module.__file__='managed-usage/'+module.__name__+'.py'",
    "  exec(compile(_sources[module.__name__],module.__file__,'exec'),module.__dict__)",
    'sys.meta_path.insert(0,_Bundle())',
    'import failover_usage',
    'print(json.dumps(failover_usage.run(json.load(sys.stdin)),allow_nan=False))',
  ].join('\n');
}

export function parseUsageHelperResponse(text: string): UsageHelperResponse {
  if (Buffer.byteLength(text) > MAX_BYTES) throw new ValidationError('Invalid usage response.');
  const value: unknown = JSON.parse(text);
  if (!isRecord(value) || value.version !== 1) throw new ValidationError('Invalid usage response.');
  if (
    value.action === 'inventory' &&
    Array.isArray(value.candidates) &&
    value.candidates.length <= 128
  ) {
    const candidates = value.candidates.map(parseUsageCandidate);
    if (candidates.some((candidate) => !candidate))
      throw new ValidationError('Invalid usage inventory.');
    const signedOut = value.signInNeededSelectors;
    if (
      signedOut !== undefined &&
      (!Array.isArray(signedOut) ||
        signedOut.length > 128 ||
        signedOut.some(
          (selector) =>
            typeof selector !== 'string' ||
            !/^claude-(?:desktop-)?profile:[A-Za-z0-9][A-Za-z0-9_.@-]{0,127}$/.test(selector)
        ))
    )
      throw new ValidationError('Invalid usage inventory.');
    return {
      version: 1,
      action: 'inventory',
      candidates: candidates as NonNullable<(typeof candidates)[number]>[],
      ...(Array.isArray(signedOut)
        ? { signInNeededSelectors: [...new Set(signedOut as string[])] }
        : {}),
    };
  }
  if (value.action !== 'collect') throw new ValidationError('Invalid usage response.');
  const candidate = parseUsageCandidate(value.candidate);
  const sample = value.sample === null ? null : parseUsageReading(value.sample);
  if (
    (value.candidate !== null && !candidate) ||
    (value.sample !== null && !sample) ||
    (value.failureCode !== null && !USAGE_FAILURE_CODES.has(String(value.failureCode)))
  ) {
    throw new ValidationError('Invalid usage response.');
  }
  const retry = value.retryAfterSeconds;
  const refresh = value.refreshAfterSeconds;
  const observedPlan = safeUsageText(value.observedPlan);
  const observedPlanAt = usageTimestamp(value.observedPlanAt);
  return {
    version: 1,
    action: 'collect',
    candidate,
    sample,
    failureCode: value.failureCode as string | null,
    ...(value.failureCode === 'source_changed' && value.rateLimited === true
      ? { rateLimited: true }
      : {}),
    ...(observedPlan && observedPlanAt ? { observedPlan, observedPlanAt } : {}),
    ...(typeof retry === 'number' && Number.isFinite(retry) && retry >= 0
      ? { retryAfterSeconds: Math.min(86400, retry) }
      : {}),
    ...(typeof refresh === 'number' && Number.isFinite(refresh) && refresh > 0
      ? { refreshAfterSeconds: Math.min(86400, refresh) }
      : {}),
  };
}

export async function runUsageHelper(
  host: UsageHost,
  request: UsageHelperRequest
): Promise<UsageHelperResponse> {
  if (host.sshHost !== null && !isSafeUsageSshAlias(host.sshHost))
    throw new ValidationError('Invalid usage host.');
  if (host.sshHost === null && host.platform !== localUsageHost().platform)
    throw new ValidationError('Usage source is not local.');
  const helperSource = await usageHelperBundle();
  const input = JSON.stringify({ helperSource, request });
  const executable = host.sshHost ? 'ssh' : localUsagePython(request);
  const args = host.sshHost
    ? [
        '-T',
        '-o',
        'BatchMode=yes',
        '-o',
        'ConnectTimeout=5',
        '-o',
        'ConnectionAttempts=1',
        '-o',
        'ServerAliveInterval=5',
        '-o',
        'ServerAliveCountMax=3',
        '--',
        host.sshHost,
        usageHelperCommand(host.platform),
      ]
    : ['-c', BOOTSTRAP];
  const stdout = await new Promise<string>((resolve, reject) => {
    const child = execFile(
      executable,
      args,
      {
        encoding: 'utf8',
        timeout: request.action === 'inventory' ? 12_000 : 35_000,
        maxBuffer: MAX_BYTES,
        windowsHide: true,
        env: { ...process.env, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1' },
      },
      (error, result) =>
        error ? reject(new NetworkError('Usage source unavailable.')) : resolve(result)
    );
    child.stdin?.on('error', () => {
      /* Only the sanitized process result is exposed. */
    });
    child.stdin?.end(input);
  });
  const response = parseUsageHelperResponse(stdout);
  if (response.action !== request.action) throw new ValidationError('Invalid usage response.');
  if (
    response.action === 'inventory' &&
    response.signInNeededSelectors?.length &&
    request.provider !== 'claude'
  )
    throw new ValidationError('Invalid usage response.');
  return response;
}

export function localUsageHost(): UsageHost {
  const host =
    process.platform === 'win32' ? 'windows' : process.platform === 'darwin' ? 'mac' : 'ubuntu';
  return { host, platform: helperPlatform(host), sshHost: null };
}

/** The same private aliases used by the dashboard; explicit account sources are added by the caller. */
export async function configuredUsageHosts(): Promise<UsageHost[]> {
  const local = localUsageHost();
  const aliases = await resolveAnalyticsRemoteHosts();
  return [
    local,
    ...(['mac', 'windows', 'nas1'] as const).flatMap((host) => {
      const sshHost = aliases[host];
      return sshHost && host !== local.host
        ? [{ host, platform: helperPlatform(host), sshHost }]
        : [];
    }),
  ];
}

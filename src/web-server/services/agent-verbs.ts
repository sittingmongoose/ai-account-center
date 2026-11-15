/** Opt-in adapter. Never ships, installs, or refreshes an agent. */
import fs from 'fs';
import { ConfigError } from '../../errors/error-types';
import path from 'path';
import { createHash, randomBytes } from 'crypto';
import { requestRustAgent } from '../rust-service-transport';
import transportPolicy from '../../../config/agent-transport-policy.json';
import { loadUnifiedConfig } from '../../config/unified-config-loader';
import { createLogger } from '../../services/logging';
import { runManagedCommand } from '../../utils/managed-process';
import { getCcsDir } from '../../utils/config-manager';
import { sshArgv } from './ssh-policy';
import type { UsageHost, UsageHelperRequest } from './usage-failover-contract';
import manifest from './agent-artifacts.json';
import { PROVIDER_ROWS } from '../../shared/tables';

const logger = createLogger('agent-verbs');
/** Independent transport switch. Invalid or absent values never start Rust transport. */
export function agentTransport(): 'off' | 'on' {
  try {
    return (process.env.AAC_AGENT_TRANSPORT ?? loadUnifiedConfig()?.agentTransport) === 'on'
      ? 'on'
      : 'off';
  } catch {
    return 'off';
  }
}

export type AgentVerbs = 'off' | 'on';
export type AgentVerb =
  | 'inventory'
  | 'signin-expiry'
  | 'quota-read'
  | 'scan-usage'
  | 't3-pins'
  | 'key-store'
  | 'claude-history'
  | 'failover-inventory';
export interface AgentArtifact {
  target: string;
  version: string;
  sha256: string;
  /** Package-relative binary; remote execution always uses the fixed managed cache. */
  file: string;
}

/** Invalid and absent settings stay off. The environment override supports isolated gates. */
export function agentVerbs(): AgentVerbs {
  const value = process.env.AAC_AGENT_VERBS ?? loadUnifiedConfig()?.agentVerbs;
  return value === 'on' ? 'on' : 'off';
}

/** Keep the one-release fallback visible without publishing host names or private paths. */
export function pythonSignInFallback(): void {
  logger.info('agent.verbs.fallback', 'fallback: python', { verb: 'signin-expiry' });
}

/** The service's single fallback boundary, shared by local and SSH collectors. */
export async function collectSignInWithFallback(
  agent: () => Promise<string | null>,
  python: () => Promise<string>
): Promise<string> {
  try {
    const result = await agent();
    if (result !== null) return result;
  } catch {
    /* Python remains the reference. */
  }
  pythonSignInFallback();
  return python();
}

function target(platform: UsageHost['platform']): string {
  return platform === 'mac'
    ? 'aarch64-apple-darwin'
    : platform === 'windows'
      ? 'x86_64-pc-windows-msvc'
      : 'x86_64-unknown-linux-musl';
}

export function agentLauncher(artifact: AgentArtifact, platform: UsageHost['platform']): string {
  if (!/^[a-f0-9]{64}$/.test(artifact.sha256)) throw new ConfigError('Invalid agent hash');
  if (platform === 'windows') {
    const script = `$ErrorActionPreference='Stop'; try { $p=Join-Path $env:LOCALAPPDATA 'ai-account-center\\bin\\aac-agent.exe'; if (!(Test-Path -LiteralPath $p -PathType Leaf)) { exit 87 };  $h=(Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash; if (!$h -or $h.ToLowerInvariant() -ne '${artifact.sha256}') { exit 87 }; & $p; exit $LASTEXITCODE } catch { exit 87 }`;
    return `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, 'utf16le').toString('base64')}`;
  }
  return `p="$HOME/.cache/ai-account-center/bin/aac-agent"; [ -f "$p" ] && [ -x "$p" ] || exit 87; h=$(sha256sum "$p" 2>/dev/null || shasum -a 256 "$p") || exit 87; [ "\${h%% *}" = '${artifact.sha256}' ] || exit 87; exec "$p"`;
}

function reply(stdout: string, id: string): string | null {
  const lines = stdout.trim().split('\n');
  if (lines.length !== 1) throw new ConfigError('Invalid agent reply');
  const event: unknown = JSON.parse(lines[0]);
  if (!event || typeof event !== 'object') throw new ConfigError('Invalid agent reply');
  const row = event as Record<string, unknown>;
  if (row.protocol !== 1 || row.id !== id || row.type !== 'result')
    throw new ConfigError('Agent failed');
  if (JSON.stringify(row.value) === '{"fallback":"python"}') return null;
  if (!row.value || typeof row.value !== 'object') throw new ConfigError('Invalid agent result');
  return JSON.stringify(row.value);
}

export interface AgentCallOptions {
  artifacts?: readonly AgentArtifact[];
  transport?: 'off' | 'on';
  onProgress?: (data: unknown) => void;
  run?: typeof runManagedCommand;
  mode?: AgentVerbs;
  deadlineMs: number;
  timeoutMs?: number;
  maxBytes: number;
}

/** Presence and pinned digest only; execution additionally verifies the version handshake. */
export function localAgentReady(
  platform: UsageHost['platform'],
  artifacts: readonly AgentArtifact[] = manifest.artifacts,
  mode: AgentVerbs = agentVerbs()
): boolean {
  if (mode !== 'on') return false;
  if (agentTransport() === 'on') return artifacts.some((a) => a.target === target(platform));
  const artifact = artifacts.find((a) => a.target === target(platform));
  if (!artifact || !/^[a-f0-9]{64}$/.test(artifact.sha256)) return false;
  try {
    const file = path.resolve(__dirname, '../../..', artifact.file);
    const stat = fs.lstatSync(file);
    return (
      stat.isFile() &&
      stat.size <= transportPolicy.artifactMaxBytes &&
      createHash('sha256').update(fs.readFileSync(file)).digest('hex') === artifact.sha256
    );
  } catch {
    return false;
  }
}

/** Every unavailable, unproven or failed agent delegates to the labelled Python helper. */
export async function runAgentVerb(
  host: Pick<UsageHost, 'platform' | 'sshHost'>,
  verb: AgentVerb,
  verbArgs: Record<string, unknown>,
  options: AgentCallOptions
): Promise<string | null> {
  try {
    const mode = options.mode ?? agentVerbs();
    if (mode === 'off') return null;
    if ((options.transport ?? agentTransport()) === 'on') {
      const started = Date.now();
      const result = await requestRustAgent(
        getCcsDir(),
        verb,
        {
          computer: { platform: host.platform, sshHost: host.sshHost ?? null },
          args: verbArgs,
          deadlineMs: options.deadlineMs,
          timeoutMs: options.timeoutMs ?? options.deadlineMs + 5000,
          maxBytes: options.maxBytes,
          callId: randomBytes(16).toString('hex'),
        },
        options.onProgress
      );
      if (result !== undefined) return result;
      logger.info('agent.transport.fallback', 'fallback: typescript transport', { verb });
      const remaining = (options.timeoutMs ?? options.deadlineMs + 5000) - (Date.now() - started);
      if (remaining <= 0) return null;
      return runAgentVerbTsFallback(host, verb, verbArgs, {
        ...options,
        timeoutMs: remaining,
        deadlineMs: Math.min(options.deadlineMs, remaining),
      });
    }
    return runAgentVerbTsFallback(host, verb, verbArgs, options);
  } catch {
    return null;
  }
}

/** Labelled one-release transport fallback while the Rust service is absent or old. */
async function runAgentVerbTsFallback(
  host: Pick<UsageHost, 'platform' | 'sshHost'>,
  verb: AgentVerb,
  verbArgs: Record<string, unknown>,
  options: AgentCallOptions
): Promise<string | null> {
  try {
    const mode = options.mode ?? agentVerbs();
    const artifacts = options.artifacts ?? manifest.artifacts;
    const run = options.run ?? runManagedCommand;
    if (mode === 'off') return null;
    const artifact = artifacts.find((a) => a.target === target(host.platform));
    if (!artifact || !/^[a-f0-9]{64}$/.test(artifact.sha256)) return null;
    const id = verb;
    let executable = 'ssh';
    let args: string[];
    if (host.sshHost) {
      args = sshArgv(host.sshHost, agentLauncher(artifact, host.platform), {
        interval: 5,
        countMax: verb === 'scan-usage' ? 6 : 1,
      });
    } else {
      executable = path.resolve(__dirname, '../../..', artifact.file);
      try {
        const info = await fs.promises.lstat(executable);
        if (!info.isFile() || info.size > transportPolicy.artifactMaxBytes) return null;
        const hash = createHash('sha256');
        for await (const chunk of fs.createReadStream(executable)) hash.update(chunk);
        if (hash.digest('hex') !== artifact.sha256) return null;
      } catch {
        return null;
      }
      const version = await run(executable, ['--version'], { timeoutMs: 3000, maxBytes: 4096 });
      if (version.failed || version.code !== 0 || version.timedOut || version.missing) return null;
      try {
        const identity = JSON.parse(version.stdout);
        if (
          identity.name !== 'aac-agent' ||
          identity.version !== artifact.version ||
          identity.protocol !== 1 ||
          identity.target !== artifact.target
        )
          return null;
      } catch {
        return null;
      }
      args = [];
    }
    const input =
      JSON.stringify({
        protocol: 1,
        verb,
        id,
        deadlineMs: options.deadlineMs,
        args: verbArgs,
      }) + '\n';
    if (Buffer.byteLength(input) > options.maxBytes) return null;
    const result = await run(executable, args, {
      timeoutMs: options.timeoutMs ?? options.deadlineMs + 5000,
      maxBytes: options.maxBytes,
      input,
      env: { ...process.env },
    });
    if (host.sshHost && result.code === 87) return null;
    if (result.failed || result.code !== 0 || result.timedOut || result.missing) return null;
    if (Buffer.byteLength(result.stdout) > options.maxBytes) return null;
    return reply(result.stdout, id);
  } catch {
    return null;
  }
}

/** Sign-in retains its existing DTO and labelled Python fallback boundary. */
export async function runSignInAgent(
  host: UsageHost,
  artifacts: readonly AgentArtifact[] = manifest.artifacts,
  run = runManagedCommand,
  mode: AgentVerbs = agentVerbs()
): Promise<string | null> {
  const result = await runAgentVerb(
    host,
    'signin-expiry',
    host.sshHost ? {} : { ccsDir: getCcsDir() },
    {
      artifacts,
      run,
      mode,
      deadlineMs: 15000,
      maxBytes: 1024 * 1024,
    }
  );
  if (result === null) return null;
  const value = JSON.parse(result) as { schemaVersion?: unknown; profiles?: unknown };
  if (value.schemaVersion !== 1 || !Array.isArray(value.profiles)) return null;
  logger.info('agent.verbs.result', 'Agent sign-in expiry collected', { verb: 'signin-expiry' });
  return result;
}

function validQuota(value: unknown): boolean {
  if (!value || typeof value !== 'object') return false;
  const row = value as Record<string, unknown>;
  return (
    typeof row.id === 'string' &&
    typeof row.provider === 'string' &&
    ['ok', 'unavailable', 'needs_sign_in', 'error'].includes(String(row.status)) &&
    Array.isArray(row.windows) &&
    typeof row.capabilities === 'object'
  );
}

/** Quota retains its DTO, provider/platform/account binding and existing fallback boundary. */
export async function runQuotaAgent(
  host: Pick<UsageHost, 'platform' | 'sshHost'>,
  args: Record<string, unknown>,
  artifacts: readonly AgentArtifact[] = manifest.artifacts,
  run = runManagedCommand,
  mode: AgentVerbs = agentVerbs()
): Promise<string | null> {
  const value = await runAgentVerb(host, 'quota-read', args, {
    artifacts,
    run,
    mode,
    deadlineMs: 15000,
    maxBytes: 1024 * 1024,
  });
  if (value === null) return null;
  const row = JSON.parse(value);
  if (
    !validQuota(row) ||
    row.provider !== args.provider ||
    row.platform !== args.platform ||
    row.id !== (args.account ?? `plan-${args.provider}-${args.platform}`)
  )
    return null;
  logger.info('agent.verbs.result', 'Agent verb collected', { verb: 'quota-read' });
  return value;
}

export async function collectQuotaWithFallback(
  agent: () => Promise<string | null>,
  python: () => Promise<string>
): Promise<string> {
  try {
    const result = await agent();
    if (result !== null) return result;
  } catch {
    /* Python remains authoritative. */
  }
  logger.info('agent.verbs.fallback', 'fallback: python', { verb: 'quota-read' });
  return python();
}

/** New verbs keep their reference helper visible on every failure. */
export async function collectAgentWithFallback<T>(
  verb: 'inventory' | 't3-pins' | 'key-store' | 'claude-history' | 'failover-inventory',
  agent: () => Promise<T | null>,
  helper: () => Promise<T>
): Promise<T> {
  try {
    const value = await agent();
    if (value !== null) return value;
  } catch {
    /* The reference helper owns unqualified inputs and failed executions. */
  }
  logger.info(
    'agent.verbs.fallback',
    verb === 'inventory'
      ? 'fallback: typescript probe'
      : verb === 't3-pins'
        ? 'fallback: sqlite3'
        : 'fallback: python',
    { verb }
  );
  return helper();
}

/** Inventory retains the existing Status machine DTO; the collector validates its fields. */
export async function runInventoryAgent(
  host: Pick<UsageHost, 'platform' | 'sshHost'>,
  accepts: (value: unknown) => boolean
): Promise<unknown | null> {
  const result = await runAgentVerb(host, 'inventory', {}, { deadlineMs: 15000, maxBytes: 65536 });
  if (result === null) return null;
  const value: unknown = JSON.parse(result);
  const row = value as { schemaVersion?: unknown; machine?: unknown };
  return row.schemaVersion === 1 && accepts(row.machine) ? row.machine : null;
}

/** Remote pins keep the existing sqlite row contract and native shape validator. */
export async function runPinsAgent(
  host: Pick<UsageHost, 'platform' | 'sshHost'>,
  options: Partial<AgentCallOptions> = {}
): Promise<string | null> {
  const result = await runAgentVerb(
    host,
    't3-pins',
    {},
    {
      deadlineMs: 25000,
      maxBytes: 1024 * 1024,
      ...options,
    }
  );
  return result !== null && Array.isArray(JSON.parse(result)) ? result : null;
}

/** Keys travel only on the protocol stdin pipe, never argv, environment or logs. */
export async function runKeyStoreAgent(
  host: Pick<UsageHost, 'platform' | 'sshHost'>,
  args: Record<string, unknown>,
  options: Partial<AgentCallOptions> = {}
): Promise<string | null> {
  const result = await runAgentVerb(host, 'key-store', args, {
    deadlineMs: 20000,
    maxBytes: 4096,
    ...options,
  });
  if (result === null) return null;
  const row = JSON.parse(result);
  const keys = Object.keys(row).sort().join(',');
  if (args.action === 'delete') return keys === 'ok' && row.ok === true ? result : null;
  if (typeof args.secret !== 'string') return null;
  const fingerprint = `sha256:${createHash('sha256').update(args.secret).digest('hex').slice(0, 16)}`;
  return keys === 'fingerprint,last4,ok' &&
    row.ok === true &&
    row.fingerprint === fingerprint &&
    row.last4 === args.secret.slice(-4)
    ? result
    : null;
}

/** History replies are private pipe data; existing service validation remains in place. */
export async function runHistoryAgent(
  host: Pick<UsageHost, 'platform' | 'sshHost'>,
  args: Record<string, unknown>,
  options: Partial<AgentCallOptions> = {}
): Promise<Buffer | null> {
  if (!['collect', 'closed-check', 'protected-check'].includes(String(args.mode))) return null;
  const result = await runAgentVerb(host, 'claude-history', args, {
    deadlineMs: 25000,
    maxBytes: 1024 * 1024,
    ...options,
  });
  if (result === null) return null;
  const row = JSON.parse(result);
  if (Array.isArray(row)) return null;
  if (args.mode === 'closed-check') {
    if (Object.keys(row).join(',') !== 'closed' || typeof row.closed !== 'boolean') return null;
  } else if (args.mode === 'protected-check') {
    if (Object.keys(row).join(',') !== 'unchanged' || typeof row.unchanged !== 'boolean')
      return null;
  } else if (args.mode === 'collect') {
    if (
      row.profileId !== args.profileId ||
      row.platform !== args.platform ||
      !Array.isArray(row.records) ||
      typeof row.revision !== 'string'
    )
      return null;
  } else {
    // No native mutation has been qualified. Keep append, receipts and pending markers
    // on the packaged Python/Node writer even if an artifact claims otherwise.
    return null;
  }
  return Buffer.from(result);
}

/** Each provider's failover identity kinds (config/tables/providers.json `failover.kinds`). */
const INVENTORY_KINDS: Record<string, readonly string[]> = Object.fromEntries(
  PROVIDER_ROWS.map(([id, row]) => [id, row.failover.kinds])
);

/** Bind a validated candidate to the requested provider before accepting an agent reply. */
export function inventoryKind(provider: UsageHelperRequest['provider'], kind: string): boolean {
  return INVENTORY_KINDS[provider]?.includes(kind) === true;
}

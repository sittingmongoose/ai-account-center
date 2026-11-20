/** Opt-in adapter. Never ships, installs, or refreshes an agent. */
import fs from 'fs';
import { ConfigError } from '../../errors/error-types';
import path from 'path';
import { createHash, createHmac, randomBytes } from 'crypto';
import { CollectionReadIndeterminate, requestRustAgent } from '../rust-service-transport';
import type { UnifiedConfig } from '../../config/schemas/unified-config';
import { privateFileOwners } from '../../shared/private-files';
import transportPolicy from '../../../config/tables/agent-transport-policy.json';
import { loadUnifiedConfig } from '../../config/unified-config-loader';
import { createLogger } from '../../services/logging';
import { runManagedCommand } from '../../utils/managed-process';
import { getCcsDir } from '../../utils/config-manager';
import { sshArgv, sshEnvironment, sshLaunch } from './ssh-policy';
import type { UsageHost, UsageHelperRequest } from './usage-failover-contract';
import manifest from '../../../config/agent-artifacts.json';
import { PROVIDER_ROWS } from '../../shared/tables';
import { CONFIRMATION_TTL_MS } from './account-confirmations';

const logger = createLogger('agent-verbs');
/** Independent transport switch. Invalid or absent values never start Rust transport. */
export function agentTransport(): 'off' | 'on' {
  try {
    const configured = process.env.AAC_AGENT_TRANSPORT ?? loadUnifiedConfig()?.agentTransport;
    return (configured ?? (process.env.AAC_RUST_CUT_WORKER === '1' ? 'on' : 'off')) === 'on'
      ? 'on'
      : 'off';
  } catch {
    return 'off';
  }
}

export type AgentVerbs = 'off' | 'shadow' | 'on';
export type AgentVerb =
  | 'inventory'
  | 'signin-expiry'
  | 'quota-read'
  | 'scan-usage'
  | 't3-pins'
  | 'key-store'
  | 'claude-history'
  | 'failover-inventory'
  | 'launch'
  | 'claude-host'
  | 'signin-session';
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
  if (process.env.AAC_RUST_CUT_WORKER === '1') return value === 'off' ? 'off' : 'on';
  return value === 'on' || value === 'shadow' ? value : 'off';
}

/** Every verb the adapter can gate; the family map admits no other key. */
const AGENT_VERBS: readonly AgentVerb[] = [
  'inventory',
  'signin-expiry',
  'quota-read',
  'scan-usage',
  't3-pins',
  'key-store',
  'claude-history',
  'failover-inventory',
  'launch',
  'claude-host',
  'signin-session',
];
/** key-store and claude-history write on the computer: shadow would duplicate the write, so
 * those two families treat shadow as off. The read-only families shadow safely. */
const WRITE_VERBS: Partial<Record<AgentVerb, true>> = {
  'key-store': true,
  'claude-history': true,
  launch: true,
  'claude-host': true,
  'signin-session': true,
};
const MODE_RANK: Record<AgentVerbs, number> = { off: 0, shadow: 1, on: 2 };

/** Per-family modes under the scalar ceiling; an absent map keeps the ceiling for every verb.
 * A present but invalid map stays empty (every family off), never the ceiling. An absent or
 * unreadable config leaves the legacy scalar behavior intact.
 * `AAC_AGENT_VERB_FAMILIES` (JSON) supports isolated gates. */
export function agentVerbFamilies(): Partial<Record<AgentVerb, AgentVerbs>> | null {
  const env = process.env.AAC_AGENT_VERB_FAMILIES;
  let raw: unknown;
  if (env !== undefined) {
    try {
      raw = JSON.parse(env);
    } catch {
      return {};
    }
  } else {
    let config: UnifiedConfig | null;
    try {
      config = loadUnifiedConfig();
    } catch {
      return null;
    }
    raw = config?.agentVerbFamilies;
  }
  if (raw === undefined || raw === null) return null;
  if (typeof raw !== 'object' || Array.isArray(raw)) return {};
  const families: Partial<Record<AgentVerb, AgentVerbs>> = {};
  for (const [key, value] of Object.entries(raw)) {
    if (!AGENT_VERBS.includes(key as AgentVerb)) return {};
    if (value !== 'off' && value !== 'shadow' && value !== 'on') return {};
    families[key as AgentVerb] = value;
  }
  return families;
}

/** One verb's effective mode: the scalar ceiling caps its family setting; an unlisted family
 * stays off while a map is present, so families turn on one at a time (DESIGN 4.5). */
export function agentVerbMode(verb: AgentVerb): AgentVerbs {
  const ceiling = agentVerbs();
  if (ceiling === 'off') return 'off';
  const families = agentVerbFamilies();
  const cut = process.env.AAC_RUST_CUT_WORKER === '1';
  const requested = families ? (families[verb] ?? (cut ? 'on' : 'off')) : ceiling;
  if (cut) return requested === 'off' ? 'off' : 'on';
  const mode = MODE_RANK[requested] <= MODE_RANK[ceiling] ? requested : ceiling;
  return mode === 'shadow' && WRITE_VERBS[verb] ? 'off' : mode;
}

export type AgentShadowOutcome =
  'equal' | 'different' | 'fallback' | 'error' | 'reference-failed' | 'reference-fallback';
export interface AgentShadowReport {
  verb: AgentVerb;
  outcome: AgentShadowOutcome;
  agentMs: number;
  referenceMs: number;
}

/** The parity spec's only normalization: JSON object key order. Arrays keep their order. */
function canonicalJson(value: unknown): string {
  const sort = (node: unknown): unknown => {
    if (Array.isArray(node)) return node.map(sort);
    if (node && typeof node === 'object') {
      const row = node as Record<string, unknown>;
      const out: Record<string, unknown> = {};
      for (const key of Object.keys(row).sort()) out[key] = sort(row[key]);
      return out;
    }
    return node;
  };
  return JSON.stringify(sort(value));
}

function shadowEqual(agent: unknown, reference: unknown): boolean {
  if (Buffer.isBuffer(agent) && Buffer.isBuffer(reference)) return agent.equals(reference);
  if (typeof agent === 'string' && typeof reference === 'string') {
    try {
      return canonicalJson(JSON.parse(agent)) === canonicalJson(JSON.parse(reference));
    } catch {
      return agent === reference;
    }
  }
  return canonicalJson(agent) === canonicalJson(reference);
}

/**
 * Shadow (DESIGN 4.5): the reference answer is served as soon as it settles; the agent runs
 * alongside and the comparison is logged in the background. Bodies never enter the log. A
 * fallback or failed agent attempt is not a difference; a reference the agent cannot match
 * (a python-fallback scan) is `reference-fallback`, also not a difference.
 */
export async function runAgentShadow<T>(
  verb: AgentVerb,
  agent: () => Promise<T | null>,
  reference: () => Promise<T>,
  onCompared?: (report: AgentShadowReport) => void,
  comparable: (reference: T) => boolean = () => true
): Promise<T> {
  const agentStarted = Date.now();
  const referenceStarted = Date.now();
  const pending = Promise.resolve()
    .then(() => agent())
    .then(
      (value) => ({ value, failed: false }),
      () => ({ value: null as T | null, failed: true })
    );
  const settled = await Promise.resolve()
    .then(() => reference())
    .then(
      (value) => ({ ok: true as const, value }),
      (error: unknown) => ({ ok: false as const, error })
    );
  const referenceMs = Date.now() - referenceStarted;
  void pending
    .then(({ value, failed }) => {
      const agentMs = Date.now() - agentStarted;
      const outcome: AgentShadowOutcome = !settled.ok
        ? 'reference-failed'
        : failed
          ? 'error'
          : value === null
            ? 'fallback'
            : !comparable(settled.value)
              ? 'reference-fallback'
              : shadowEqual(value, settled.value)
                ? 'equal'
                : 'different';
      logger.info('agent.verbs.shadow', 'shadow comparison', {
        verb,
        outcome,
        agentMs,
        referenceMs,
      });
      onCompared?.({ verb, outcome, agentMs, referenceMs });
    })
    .catch(() => {
      /* A failing comparison hook never changes the served answer. */
    });
  if (!settled.ok) throw settled.error;
  return settled.value;
}

/** Keep the one-release fallback visible without publishing host names or private paths. */
export function pythonSignInFallback(): void {
  logger.info('agent.verbs.fallback', 'fallback: python', { verb: 'signin-expiry' });
}

/** The service's single fallback boundary, shared by local and SSH collectors. */
export async function collectSignInWithFallback(
  agent: () => Promise<string | null>,
  python: () => Promise<string>,
  mode: AgentVerbs = agentVerbMode('signin-expiry'),
  onCompared?: (report: AgentShadowReport) => void
): Promise<string> {
  if (mode === 'shadow') return runAgentShadow('signin-expiry', agent, python, onCompared);
  try {
    const result = await agent();
    if (result !== null) return result;
  } catch {
    /* Python remains the reference. */
  }
  pythonSignInFallback();
  return python();
}

/** The targets one platform admits, in preference order; mirrors the service's
 * compatible_target (musl first on Linux, Apple Silicon first on the Mac). */
function agentTargets(platform: UsageHost['platform']): readonly string[] {
  return platform === 'mac'
    ? ['aarch64-apple-darwin', 'x86_64-apple-darwin']
    : platform === 'windows'
      ? ['x86_64-pc-windows-msvc']
      : ['x86_64-unknown-linux-musl', 'x86_64-unknown-linux-gnu'];
}

function findArtifact(
  artifacts: readonly AgentArtifact[],
  platform: UsageHost['platform']
): AgentArtifact | undefined {
  for (const target of agentTargets(platform)) {
    const artifact = artifacts.find((a) => a.target === target);
    if (artifact) return artifact;
  }
  return undefined;
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
  singleShot?: boolean;
}

/** Presence and pinned digest only; execution additionally verifies the version handshake. */
export function localAgentReady(
  platform: UsageHost['platform'],
  artifacts: readonly AgentArtifact[] = manifest.artifacts,
  mode: AgentVerbs = agentVerbMode('scan-usage')
): boolean {
  if (mode === 'off') return false;
  if (agentTransport() === 'on')
    return artifacts.some((a) => agentTargets(platform).includes(a.target));
  const artifact = findArtifact(artifacts, platform);
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

function serviceCollects(verb: AgentVerb): boolean {
  return (
    ['quota-read', 'signin-expiry', 'failover-inventory'].includes(verb) &&
    privateFileOwners()['account-usage-observations'] === 'rust-service'
  );
}

/** Every unavailable, unproven or failed agent delegates to the labelled Python helper. */
export async function runAgentVerb(
  host: Pick<UsageHost, 'platform' | 'sshHost'>,
  verb: AgentVerb,
  verbArgs: Record<string, unknown>,
  options: AgentCallOptions
): Promise<string | null> {
  try {
    const mode = options.mode ?? agentVerbMode(verb);
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
        options.onProgress,
        options.singleShot,
        serviceCollects(verb)
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
  } catch (error) {
    if (options.singleShot || error instanceof CollectionReadIndeterminate) throw error;
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
  let dispatched = false;
  try {
    const mode = options.mode ?? agentVerbMode(verb);
    const artifacts = options.artifacts ?? manifest.artifacts;
    // The default runner launches only the probed ssh, checked just before the spawn.
    const run =
      options.run ??
      (async (file: string, args: string[], opts: Parameters<typeof runManagedCommand>[2]) =>
        runManagedCommand(await sshLaunch(file, args), args, opts));
    if (mode === 'off') return null;
    const artifact = findArtifact(artifacts, host.platform);
    if (!artifact || !/^[a-f0-9]{64}$/.test(artifact.sha256)) return null;
    const id = verb;
    let executable = 'ssh';
    let args: string[];
    if (host.sshHost) {
      args = await sshArgv(host.sshHost, agentLauncher(artifact, host.platform), {
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
    dispatched = true;
    const result = await run(executable, args, {
      timeoutMs: options.timeoutMs ?? options.deadlineMs + 5000,
      maxBytes: options.maxBytes,
      input,
      env: host.sshHost ? sshEnvironment() : { ...process.env },
    });
    if (result.missing || (host.sshHost && result.code === 87 && !result.timedOut)) return null;
    if (
      result.failed ||
      result.code !== 0 ||
      result.timedOut ||
      Buffer.byteLength(result.stdout) > options.maxBytes
    )
      throw new ConfigError('Agent reply unavailable.');
    return reply(result.stdout, id);
  } catch (error) {
    if (dispatched && options.singleShot) throw error;
    return null;
  }
}

/** Sign-in retains its existing DTO and labelled Python fallback boundary. */
export async function runSignInAgent(
  host: UsageHost,
  artifacts: readonly AgentArtifact[] = manifest.artifacts,
  run: typeof runManagedCommand | undefined = undefined,
  mode: AgentVerbs = agentVerbMode('signin-expiry')
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
  run: typeof runManagedCommand | undefined = undefined,
  mode: AgentVerbs = agentVerbMode('quota-read')
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
  ) {
    if (agentTransport() === 'on' && serviceCollects('quota-read'))
      throw new CollectionReadIndeterminate();
    return null;
  }
  logger.info('agent.verbs.result', 'Agent verb collected', { verb: 'quota-read' });
  return value;
}

export async function collectQuotaWithFallback(
  agent: () => Promise<string | null>,
  python: () => Promise<string>,
  mode: AgentVerbs = agentVerbMode('quota-read'),
  onCompared?: (report: AgentShadowReport) => void
): Promise<string> {
  if (mode === 'shadow') return runAgentShadow('quota-read', agent, python, onCompared);
  try {
    const result = await agent();
    if (result !== null) return result;
  } catch (error) {
    if (error instanceof CollectionReadIndeterminate) throw error;
    /* Python remains authoritative. */
  }
  logger.info('agent.verbs.fallback', 'fallback: python', { verb: 'quota-read' });
  return python();
}

/** New verbs keep their reference helper visible on every failure. */
export async function collectAgentWithFallback<T>(
  verb:
    | 'inventory'
    | 't3-pins'
    | 'key-store'
    | 'claude-history'
    | 'failover-inventory'
    | 'launch'
    | 'claude-host',
  agent: () => Promise<T | null>,
  helper: () => Promise<T>,
  mode: AgentVerbs = agentVerbMode(verb),
  onCompared?: (report: AgentShadowReport) => void,
  singleShot = false
): Promise<T> {
  if (mode === 'shadow' && !singleShot) return runAgentShadow(verb, agent, helper, onCompared);
  try {
    const value = await agent();
    if (value !== null) return value;
  } catch (error) {
    if (singleShot) throw error;
    /* The reference helper owns unqualified inputs and failed executions. */
  }
  logger.info(
    'agent.verbs.fallback',
    verb === 'inventory'
      ? 'fallback: typescript probe'
      : verb === 't3-pins'
        ? 'fallback: sqlite3'
        : verb === 'launch'
          ? 'fallback: typescript launcher'
          : verb === 'claude-host'
            ? 'fallback: host bootstrap'
            : 'fallback: python',
    { verb }
  );
  return helper();
}

/**
 * M4b host steps keep the reference's small JSON answers; the ssh bootstrap stays the
 * labelled fallback. The envelope binds the host family; the agent checks it against its
 * own computer. Purge carries the permit `claudeHostPurgePermit` issues.
 */
export async function runClaudeHostAgent(
  host: { platform: 'mac' | 'windows'; sshHost?: string },
  args: Record<string, unknown>,
  options: Partial<AgentCallOptions> = {}
): Promise<Record<string, unknown> | null> {
  const result = await runAgentVerb(
    { platform: host.platform, sshHost: host.sshHost ?? null },
    'claude-host',
    { host: host.platform, ...args },
    { deadlineMs: 60000, maxBytes: 65536, ...options }
  );
  if (result === null) return null;
  const value: unknown = JSON.parse(result);
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  switch (args.op) {
    case 'create':
      return typeof row.launcherName === 'string' && typeof row.profilePath === 'string'
        ? row
        : null;
    case 'state':
      return typeof row.running === 'boolean' ? row : null;
    case 'session':
      return typeof row.signedIn === 'boolean' ? row : null;
    case 'trash':
      return row.result === 'moved' || row.result === 'cross_volume' ? row : null;
    case 'undo':
    case 'restore':
    case 'purge':
    case 'open':
      return row.ok === true ? row : null;
    default:
      return null;
  }
}

/**
 * The TS-issued purge permit for the agent boundary: an HMAC-SHA256 over the host, the
 * exact trash folder and the expiry, with the one shared policy key. The route's
 * confirmation is consumed in the front door first; this binds the same decision for the
 * agent, which refuses purge without it. Never logged.
 */
export function claudeHostPurgePermit(
  host: 'mac' | 'windows',
  trashName: string,
  now: number = Date.now()
): string {
  const expiresMs = now + CONFIRMATION_TTL_MS;
  const key = Buffer.from(transportPolicy.claudeHostPurgeKey, 'hex');
  const mac = createHmac('sha256', key)
    .update(`aac-claude-host-purge\0${host}\0${trashName}\0${expiresMs}`, 'utf8')
    .digest('hex');
  return `${expiresMs}.${mac}`;
}

/**
 * Inventory retains the existing Status machine DTO; the collector validates its fields.
 * The attempt is 8s, not the verb's 15s plus 5s grace: the TypeScript probe that follows a miss
 * is another 20s, and the two must not add up to 40s (L5 review L6). The agent is told 6s, so it
 * ends with its own answer before the kill; the worst case is 8s plus the 20s probe, 28s.
 */
export async function runInventoryAgent(
  host: Pick<UsageHost, 'platform' | 'sshHost'>,
  accepts: (value: unknown) => boolean,
  options: Partial<AgentCallOptions> = {}
): Promise<unknown | null> {
  const result = await runAgentVerb(
    host,
    'inventory',
    {},
    {
      ...options,
      // The agent's own deadline ends first, so it answers (or declines) before the 8s kill.
      deadlineMs: 6000,
      timeoutMs: 8000,
      maxBytes: 65536,
    }
  );
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
  const cleanup = args.mode === 'snapshot-cleanup';
  if (cleanup) {
    // Windows keeps the existing helper until the native target can prove its
    // directory and file identities. Every other operand comes from the route's
    // bound profile resolution.
    if (
      host.platform !== 'mac' ||
      args.platform !== 'mac' ||
      typeof args.profileId !== 'string' ||
      !args.policy ||
      typeof args.policy !== 'object'
    )
      return null;
    const artifacts = options.artifacts ?? manifest.artifacts;
    if (!artifacts.some((artifact) => agentTargets(host.platform).includes(artifact.target)))
      return null;
  } else if (!['collect', 'closed-check', 'protected-check'].includes(String(args.mode))) {
    return null;
  }
  const result = await runAgentVerb(host, 'claude-history', args, {
    deadlineMs: 25000,
    maxBytes: 1024 * 1024,
    ...options,
    singleShot: cleanup,
  });
  if (result === null) return null;
  const row = JSON.parse(result);
  if (cleanup) {
    if (
      Object.keys(row).join(',') !== 'kept,deleted,skipped' ||
      ![row.kept, row.deleted, row.skipped].every(
        (items) =>
          Array.isArray(items) &&
          items.length <= 1024 &&
          items.every(
            (name) =>
              typeof name === 'string' && /^\.history-index-snapshot-[a-f0-9]{32}$/.test(name)
          )
      )
    )
      throw new ConfigError('Snapshot cleanup reply unavailable.');
    return Buffer.from(result);
  }
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

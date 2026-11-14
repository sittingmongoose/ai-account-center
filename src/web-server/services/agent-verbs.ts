/** M2 read-only opt-in adapter. Never ships, installs, or refreshes an agent. */
import fs from 'fs';
import { ConfigError } from '../../errors/error-types';
import path from 'path';
import { createHash } from 'crypto';
import { loadUnifiedConfig } from '../../config/unified-config-loader';
import { createLogger } from '../../services/logging';
import { runManagedCommand } from '../../utils/managed-process';
import { getCcsDir } from '../../utils/config-manager';
import { sshArgv } from './ssh-policy';
import type { UsageHost } from './usage-failover-contract';
import manifest from './agent-artifacts.json';

const logger = createLogger('agent-verbs');
export type AgentVerbs = 'off' | 'on';
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
    const script = `$p=Join-Path $env:LOCALAPPDATA 'ai-account-center\\bin\\aac-agent.exe'; if (!(Test-Path -LiteralPath $p -PathType Leaf)) { exit 87 }; if ((Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash.ToLowerInvariant() -ne '${artifact.sha256}') { exit 87 }; & $p; exit $LASTEXITCODE`;
    return `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, 'utf16le').toString('base64')}`;
  }
  return `p="$HOME/.cache/ai-account-center/bin/aac-agent"; [ -f "$p" ] && [ -x "$p" ] || exit 87; h=$(sha256sum "$p" 2>/dev/null || shasum -a 256 "$p") || exit 87; [ "\${h%% *}" = '${artifact.sha256}' ] || exit 87; exec "$p"`;
}

function reply(stdout: string, id: string): string {
  const lines = stdout.trim().split('\n');
  if (lines.length !== 1) throw new ConfigError('Invalid agent reply');
  const event: unknown = JSON.parse(lines[0]);
  if (!event || typeof event !== 'object') throw new ConfigError('Invalid agent reply');
  const row = event as Record<string, unknown>;
  if (row.protocol !== 1 || row.id !== id || row.type !== 'result')
    throw new ConfigError('Agent failed');
  const value = row.value as { schemaVersion?: unknown; profiles?: unknown } | null;
  if (value?.schemaVersion !== 1 || !Array.isArray(value.profiles))
    throw new ConfigError('Invalid agent result');
  return JSON.stringify(value);
}

/** Null means no verified agent; a running agent's failure stays a failure. */
export async function runSignInAgent(
  host: UsageHost,
  artifacts: readonly AgentArtifact[] = manifest.artifacts,
  run = runManagedCommand,
  mode: AgentVerbs = agentVerbs()
): Promise<string | null> {
  if (mode === 'off') return null;
  const artifact = artifacts.find((a) => a.target === target(host.platform));
  if (!artifact || !/^[a-f0-9]{64}$/.test(artifact.sha256)) return null;
  const id = 'signin-expiry';
  let executable = 'ssh';
  let args: string[];
  if (host.sshHost) {
    args = sshArgv(host.sshHost, agentLauncher(artifact, host.platform), {
      interval: 5,
      countMax: 1,
    });
  } else {
    executable = path.resolve(__dirname, '../../..', artifact.file);
    try {
      const info = await fs.promises.lstat(executable);
      if (!info.isFile() || info.size > 128 * 1024 * 1024) return null;
      const hash = createHash('sha256');
      for await (const chunk of fs.createReadStream(executable)) hash.update(chunk);
      if (hash.digest('hex') !== artifact.sha256) return null;
    } catch {
      return null;
    }
    const version = await run(executable, ['--version'], { timeoutMs: 3000, maxBytes: 4096 });
    if (version.failed) return null;
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
      verb: 'signin-expiry',
      id,
      deadlineMs: 15000,
      args: host.sshHost ? {} : { ccsDir: getCcsDir() },
    }) + '\n';
  const result = await run(executable, args, {
    timeoutMs: 20000,
    maxBytes: 1024 * 1024,
    input,
    env: { ...process.env },
  });
  if (host.sshHost && result.code === 87) return null;
  if (result.failed) throw new ConfigError('collector failed');
  logger.info('agent.verbs.result', 'Agent sign-in expiry collected', { verb: 'signin-expiry' });
  return reply(result.stdout, id);
}

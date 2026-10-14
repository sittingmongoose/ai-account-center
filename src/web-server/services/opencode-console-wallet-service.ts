import { execFile } from 'child_process';
import fs from 'fs/promises';
import path from 'path';
import { createHash, randomUUID } from 'crypto';
import { ConfigError } from '../../errors/error-types';
import { getCcsDir } from '../../utils/config-manager';
import { isSafeUsageSshAlias } from './additional-usage-transport';
import type { DashboardAccount, DashboardAccountWindow } from './account-dashboard-types';
import { getUsageFailoverService, type UsageFailoverService } from './usage-failover-service';
import {
  parseUsageCandidate,
  type UsageCandidate,
  type UsageFailoverTarget,
} from './usage-failover-contract';
import { runUsageHelper } from './usage-failover-transport';

const SOURCE = 'Authenticated OpenCode console workspace on Mac';
const MAX_BYTES = 64 * 1024;
const TTL_MS = 120_000;
const BACKOFF_MS = 30_000;
const DEBOUNCE_MS = 5_000;
const WALLET_ID = /^plan-opencode-go-console-mac-[a-f0-9]{12}$/;
const WINDOW_KEYS = ['console-fiveHour', 'console-week', 'console-month', 'zen-balance'];

export interface OpenCodeWalletDeps {
  ccsDir?: string;
  readSource?: () => Promise<string | null>;
  runSource?: (sshHost: string) => Promise<string>;
  inventorySource?: (sshHost: string) => Promise<UsageCandidate[]>;
  failover?: Pick<UsageFailoverService, 'collect' | 'recover'>;
  now?: () => number;
}

const ACCOUNT_CACHE = 'opencode-console-wallet-account.json';
const WORKSPACE_NOTE = 'Console workspace; API-key account identity has not been linked.';

function sourceHash(binding: string): string {
  return createHash('sha256').update(binding).digest('hex');
}

async function privateCacheDirectory(ccsDir: string): Promise<boolean> {
  try {
    const info = await fs.lstat(ccsDir);
    return (
      info.isDirectory() &&
      !info.isSymbolicLink() &&
      (process.platform === 'win32' ||
        ((info.mode & 0o077) === 0 && (!process.getuid || info.uid === process.getuid())))
    );
  } catch {
    return false;
  }
}

/** Non-secret workspace pointer only; the common service owns verified identity and usage. */
async function readBoundAccount(ccsDir: string, binding: string): Promise<string | null> {
  try {
    if (!(await privateCacheDirectory(ccsDir))) return null;
    const file = path.join(ccsDir, ACCOUNT_CACHE);
    const info = await fs.lstat(file);
    if (
      !info.isFile() ||
      info.isSymbolicLink() ||
      info.size > 512 ||
      (process.platform !== 'win32' &&
        ((info.mode & 0o077) !== 0 || (process.getuid && info.uid !== process.getuid())))
    )
      return null;
    const value: unknown = JSON.parse(await fs.readFile(file, 'utf8'));
    return record(value) &&
      value.version === 1 &&
      value.binding === sourceHash(binding) &&
      typeof value.accountId === 'string' &&
      WALLET_ID.test(value.accountId) &&
      Object.keys(value).sort().join(',') === 'accountId,binding,version'
      ? value.accountId
      : null;
  } catch {
    return null;
  }
}

async function rememberBoundAccount(
  ccsDir: string,
  binding: string,
  accountId: string
): Promise<void> {
  const temporary = path.join(ccsDir, `.opencode-console-wallet-account.${randomUUID()}.tmp`);
  try {
    if (!WALLET_ID.test(accountId) || !(await privateCacheDirectory(ccsDir))) return;
    const file = path.join(ccsDir, ACCOUNT_CACHE);
    const before = await fs.lstat(file).catch(() => null);
    if (before && (!before.isFile() || before.isSymbolicLink())) return;
    await fs.writeFile(
      temporary,
      JSON.stringify({ version: 1, binding: sourceHash(binding), accountId }),
      { encoding: 'utf8', mode: 0o600, flag: 'wx' }
    );
    await fs.rename(temporary, file);
  } catch {
    // Persistence is best-effort; it cannot turn a valid current reading into an error.
  } finally {
    await fs.unlink(temporary).catch(() => undefined);
  }
}

function missingWallet(accountId: string): DashboardAccount {
  return {
    id: accountId,
    provider: 'opencode-go',
    providerLabel: 'OpenCode Go',
    label: 'OpenCode console wallet',
    email: null,
    plan: null,
    platform: 'mac',
    source: SOURCE,
    status: 'unavailable',
    message: WORKSPACE_NOTE,
    fetchedAt: null,
    sampledAt: null,
    isActive: false,
    windows: [],
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  };
}

function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

function timestamp(value: unknown): string | null {
  if (typeof value !== 'string' || value.length > 64 || !/^\d{4}-\d{2}-\d{2}T/.test(value))
    return null;
  const parsed = new Date(value);
  return Number.isFinite(parsed.getTime()) &&
    parsed.getUTCFullYear() >= 2000 &&
    parsed.getUTCFullYear() <= 2200
    ? parsed.toISOString()
    : null;
}

function finite(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function isPresent(value: unknown): boolean {
  return value !== null && value !== undefined;
}

function normalize(contents: string): DashboardAccount | null {
  if (Buffer.byteLength(contents) > MAX_BYTES) return null;
  const value: unknown = JSON.parse(contents);
  if (
    !record(value) ||
    typeof value.id !== 'string' ||
    !WALLET_ID.test(value.id) ||
    value.provider !== 'opencode-go' ||
    value.platform !== 'mac' ||
    value.status !== 'ok' ||
    value.source !== SOURCE ||
    value.label !== 'OpenCode console wallet' ||
    value.email !== null ||
    !Array.isArray(value.windows) ||
    value.windows.length < 1 ||
    value.windows.length > 4
  )
    return null;
  const fetchedAt = timestamp(value.fetchedAt);
  const sampledAt = timestamp(value.sampledAt);
  if (!fetchedAt || !sampledAt) return null;
  const seen = new Set<string>();
  const windows: DashboardAccountWindow[] = [];
  for (const item of value.windows) {
    if (
      !record(item) ||
      typeof item.key !== 'string' ||
      !WINDOW_KEYS.includes(item.key) ||
      seen.has(item.key)
    )
      return null;
    seen.add(item.key);
    const resetAt = timestamp(item.resetAt);
    const expiresAt = timestamp(item.expiresAt);
    if ((isPresent(item.resetAt) && !resetAt) || (isPresent(item.expiresAt) && !expiresAt))
      return null;
    if (item.key === 'zen-balance') {
      const remaining = finite(item.remaining);
      if (
        item.kind !== 'balance' ||
        item.unit !== 'USD' ||
        remaining === null ||
        isPresent(item.usedPercent) ||
        isPresent(item.remainingPercent) ||
        isPresent(item.resetAt)
      )
        return null;
      windows.push({
        key: item.key,
        label: 'Zen balance',
        kind: 'balance',
        remaining,
        unit: 'USD',
        expiresAt,
        usedPercent: null,
        remainingPercent: null,
        resetAt: null,
        windowMinutes: null,
        used: null,
        limit: null,
      });
    } else {
      const usedPercent = finite(item.usedPercent);
      if (item.kind !== 'rate_limit' || usedPercent === null || usedPercent < 0) return null;
      const reportedRemaining = finite(item.remainingPercent);
      const remainingPercent =
        reportedRemaining !== null && reportedRemaining >= 0 && reportedRemaining <= 100
          ? reportedRemaining
          : Math.max(0, 100 - usedPercent);
      const index = WINDOW_KEYS.indexOf(item.key);
      windows.push({
        key: item.key,
        label: ['Console 5 hours', 'Console weekly', 'Console monthly'][index],
        kind: 'rate_limit',
        usedPercent,
        remainingPercent,
        resetAt,
        expiresAt,
        windowMinutes: [300, 10080, null][index],
        used: null,
        limit: null,
        unit: null,
      });
    }
  }
  if (!seen.has('zen-balance')) return null;
  return {
    id: value.id,
    provider: 'opencode-go',
    providerLabel: 'OpenCode Go',
    label: 'OpenCode console wallet',
    email: null,
    plan: value.plan === 'Go' && windows.length > 1 ? 'Go' : null,
    platform: 'mac',
    source: SOURCE,
    status: 'ok',
    message: 'Console workspace; API-key account identity has not been linked.',
    fetchedAt,
    sampledAt,
    isActive: false,
    windows,
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  };
}

export function opencodeWalletSourceFile(ccsDir: string): string {
  return path.join(ccsDir, 'opencode-console-wallet-source.json');
}

async function readSourceFile(ccsDir: string): Promise<string | null> {
  try {
    const file = opencodeWalletSourceFile(ccsDir);
    const stat = await fs.lstat(file);
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 4096) return null;
    return await fs.readFile(file, 'utf8');
  } catch {
    return null;
  }
}

/**
 * The stored wallet source for Remove's fingerprint: the opt-in source file
 * contents, or null when absent or unreadable. Never throws.
 */
export async function readOpencodeWalletSource(ccsDir: string): Promise<string | null> {
  return readSourceFile(ccsDir);
}

/**
 * Delete the stored wallet source (the opt-in file only; the browser
 * extension's session is never touched). Returns true when a file was
 * removed, false when none existed. Throws on any other failure.
 */
export async function deleteOpencodeWalletSource(ccsDir: string): Promise<boolean> {
  const file = opencodeWalletSourceFile(ccsDir);
  try {
    await fs.unlink(file);
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
    throw new ConfigError('The console wallet source could not be removed safely.');
  }
}

/** The command can only read the installed bridge's verified safe sample. */
async function runSource(sshHost: string): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
      'ssh',
      [
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
        'ServerAliveCountMax=1',
        '--',
        sshHost,
        'exec "$HOME/.ccs/opencode-usage-bridge/native-host/launch-host.sh" --collect',
      ],
      { encoding: 'utf8', timeout: 25_000, maxBuffer: MAX_BYTES, windowsHide: true },
      (error, stdout) => {
        if (error) reject(new Error('Optional console wallet unavailable.'));
        else resolve(stdout);
      }
    );
  });
}

async function inventorySource(sshHost: string): Promise<UsageCandidate[]> {
  const response = await runUsageHelper(
    { host: 'mac', platform: 'mac', sshHost },
    { version: 1, action: 'inventory', provider: 'opencode-wallet' }
  );
  return response.action === 'inventory' ? response.candidates : [];
}

/** Missing browser capsules produce no account, rather than a phantom sign-in card. */
export class OpenCodeConsoleWalletService {
  private source: string | null = null;
  private accountId: string | null = null;
  private result: DashboardAccount | null = null;
  private pending: Promise<DashboardAccount | null> | null = null;
  private fetchedAt = -Infinity;
  private refreshedAt = -Infinity;
  private failed = false;

  constructor(private readonly deps: OpenCodeWalletDeps = {}) {}

  private clear(): void {
    this.source = this.accountId = null;
    this.result = this.pending = null;
    this.fetchedAt = this.refreshedAt = -Infinity;
    this.failed = false;
  }

  private async collectSource(sshHost: string, binding: string): Promise<DashboardAccount | null> {
    // Legacy injected readers remain available to isolated compatibility fixtures.
    // Production discovers only local metadata, then lets the coordinator own
    // every provider request and its shared workspace cooldown.
    const failover =
      this.deps.failover ??
      (this.deps.runSource || this.deps.readSource ? null : getUsageFailoverService());
    if (failover && !this.deps.runSource)
      return this.collectManagedSource(sshHost, binding, failover);
    const primary = await Promise.resolve()
      .then(() => (this.deps.runSource ?? runSource)(sshHost))
      .then(normalize)
      .catch(() => null);
    const accountId = primary?.id ?? this.accountId ?? this.result?.id;
    if (!accountId || this.source !== binding) return null;
    const target: UsageFailoverTarget = {
      accountId,
      provider: 'opencode-wallet',
      ccsDir: this.deps.ccsDir ?? getCcsDir(),
      binding,
      primarySources: [{ host: 'mac', platform: 'mac', sshHost, selector: 'wallet' }],
    };
    // Existing isolated legacy fixtures supply runSource/readSource. They must
    // explicitly inject the new transport rather than touching real accounts.
    const result = failover
      ? await (
          primary
            ? failover.recover(target, primary)
            : failover.collect(target, missingWallet(accountId))
        ).catch(() => primary ?? null)
      : primary;
    if (this.source !== binding) return null;
    if (primary) {
      this.accountId = primary.id;
      if (this.deps.ccsDir) await rememberBoundAccount(this.deps.ccsDir, binding, primary.id);
    }
    // The legacy bridge may have verified a sample before the common inventory
    // could establish its full workspace identity. Retain that exact wallet's
    // dated reading during a later source outage as well.
    if (
      !failover &&
      !primary &&
      result?.statusReason !== 'identity_unbound' &&
      (!result || result.status === 'unavailable' || result.status === 'error') &&
      this.result?.id === accountId &&
      (this.result.status === 'ok' || this.result.status === 'cached') &&
      this.result.windows.length > 0
    ) {
      return {
        ...this.result,
        status: 'cached',
        message: `Live console wallet usage is unavailable; showing the last saved sample. ${WORKSPACE_NOTE}`,
      };
    }
    if (result && result.id === accountId && result.provider === 'opencode-go') {
      return {
        ...result,
        message:
          result.message && result.message !== WORKSPACE_NOTE
            ? `${result.message} ${WORKSPACE_NOTE}`
            : WORKSPACE_NOTE,
        capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
      };
    }
    // A previously verified legacy capsule remains an honest dated reading.
    return !failover && this.result && this.result.id === accountId
      ? {
          ...this.result,
          status: 'cached',
          message: `Live console wallet usage is unavailable; showing the last saved sample. ${WORKSPACE_NOTE}`,
        }
      : null;
  }

  private async collectManagedSource(
    sshHost: string,
    binding: string,
    failover: Pick<UsageFailoverService, 'collect' | 'recover'>
  ): Promise<DashboardAccount | null> {
    let accountId = this.accountId ?? this.result?.id;
    try {
      const candidates = (await (this.deps.inventorySource ?? inventorySource)(sshHost))
        .map(parseUsageCandidate)
        .filter(
          (candidate) =>
            candidate?.identity.kind === 'opencode-workspace' &&
            candidate.selectors.includes('wallet')
        );
      const selected = candidates.length === 1 ? candidates[0] : null;
      if (selected && this.source === binding) {
        accountId = `plan-opencode-go-console-mac-${selected.identity.key.slice(0, 12)}`;
        this.accountId = accountId;
        if (this.deps.ccsDir) await rememberBoundAccount(this.deps.ccsDir, binding, accountId);
      }
    } catch {
      // An offline primary may still use the previously bound workspace cache.
    }
    if (!accountId || this.source !== binding) return null;
    const target: UsageFailoverTarget = {
      accountId,
      provider: 'opencode-wallet',
      ccsDir: this.deps.ccsDir ?? getCcsDir(),
      binding,
      primarySources: [{ host: 'mac', platform: 'mac', sshHost, selector: 'wallet' }],
    };
    const result = await failover.collect(target, missingWallet(accountId)).catch(() => null);
    if (this.source !== binding || result?.id !== accountId || result.provider !== 'opencode-go')
      return null;
    if ((result.status === 'ok' || result.status === 'cached') && result.windows.length) {
      this.accountId = accountId;
      if (this.deps.ccsDir) await rememberBoundAccount(this.deps.ccsDir, binding, accountId);
    }
    return {
      ...result,
      message:
        result.message && result.message !== WORKSPACE_NOTE
          ? `${result.message} ${WORKSPACE_NOTE}`
          : WORKSPACE_NOTE,
      capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
    };
  }

  async get(opts: { refresh?: boolean } = {}): Promise<DashboardAccount[]> {
    let sshHost: string;
    let binding: string;
    try {
      const contents = await (
        this.deps.readSource ?? (() => readSourceFile(this.deps.ccsDir ?? getCcsDir()))
      )();
      if (!contents || Buffer.byteLength(contents) > 4096) {
        this.clear();
        return [];
      }
      const value: unknown = JSON.parse(contents);
      if (
        !record(value) ||
        value.version !== 1 ||
        value.platform !== 'mac' ||
        !isSafeUsageSshAlias(value.sshHost) ||
        Object.keys(value).some((key) => !['version', 'platform', 'sshHost'].includes(key))
      ) {
        this.clear();
        return [];
      }
      sshHost = value.sshHost;
      binding = JSON.stringify({ version: 1, platform: 'mac', sshHost });
    } catch {
      this.clear();
      return [];
    }
    if (binding !== this.source) {
      this.clear();
      this.source = binding;
      if (this.deps.ccsDir) {
        const savedId = await readBoundAccount(this.deps.ccsDir, binding);
        if (this.source !== binding) return [];
        this.accountId ??= savedId;
      }
    }
    const now = (this.deps.now ?? Date.now)();
    const force = opts.refresh === true && now - this.refreshedAt >= DEBOUNCE_MS;
    const expired = now - this.fetchedAt >= (this.result && !this.failed ? TTL_MS : BACKOFF_MS);
    const backoff = (this.failed || !this.result) && now - this.fetchedAt < BACKOFF_MS;
    let fetched = false;
    if (!this.pending && !backoff && (force || expired)) {
      this.refreshedAt = now;
      const pending = this.collectSource(sshHost, binding).catch(() => null);
      this.pending = pending;
      void pending.then((result) => {
        if (this.pending !== pending || this.source !== binding) return;
        this.result = result;
        this.failed = result?.status !== 'ok';
        this.fetchedAt = (this.deps.now ?? Date.now)();
        this.pending = null;
      });
    }
    const pending = this.pending;
    if (pending) {
      fetched = true;
      await pending;
    }
    if (this.source !== binding || !this.result) return [];
    return [
      {
        ...this.result,
        status: !fetched && this.result.status === 'ok' ? 'cached' : this.result.status,
        windows: this.result.windows.map((window) => ({ ...window })),
      },
    ];
  }
}

const services = new Map<string, OpenCodeConsoleWalletService>();
export function getOpenCodeConsoleWalletAccounts(
  opts: { refresh?: boolean } = {}
): Promise<DashboardAccount[]> {
  const scope = getCcsDir();
  let service = services.get(scope);
  if (!service) {
    service = new OpenCodeConsoleWalletService({ ccsDir: scope });
    services.set(scope, service);
    while (services.size > 16) services.delete(services.keys().next().value as string);
  }
  return service.get(opts);
}

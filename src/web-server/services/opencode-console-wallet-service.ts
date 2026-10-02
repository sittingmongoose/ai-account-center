import { execFile } from 'child_process';
import fs from 'fs/promises';
import path from 'path';
import { ConfigError } from '../../errors/error-types';
import { getCcsDir } from '../../utils/config-manager';
import { isSafeUsageSshAlias } from './additional-usage-transport';
import type { DashboardAccount, DashboardAccountWindow } from './account-dashboard-types';

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
  now?: () => number;
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

/** Missing browser capsules produce no account, rather than a phantom sign-in card. */
export class OpenCodeConsoleWalletService {
  private source: string | null = null;
  private result: DashboardAccount | null = null;
  private pending: Promise<DashboardAccount | null> | null = null;
  private fetchedAt = -Infinity;
  private refreshedAt = -Infinity;

  constructor(private readonly deps: OpenCodeWalletDeps = {}) {}

  async get(opts: { refresh?: boolean } = {}): Promise<DashboardAccount[]> {
    let sshHost: string;
    try {
      const contents = await (
        this.deps.readSource ?? (() => readSourceFile(this.deps.ccsDir ?? getCcsDir()))
      )();
      if (!contents || Buffer.byteLength(contents) > 4096) return [];
      const value: unknown = JSON.parse(contents);
      if (
        !record(value) ||
        value.version !== 1 ||
        value.platform !== 'mac' ||
        !isSafeUsageSshAlias(value.sshHost) ||
        Object.keys(value).some((key) => !['version', 'platform', 'sshHost'].includes(key))
      )
        return [];
      sshHost = value.sshHost;
    } catch {
      return [];
    }
    if (sshHost !== this.source) {
      this.source = sshHost;
      this.result = null;
      this.pending = null;
      this.fetchedAt = this.refreshedAt = -Infinity;
    }
    const now = (this.deps.now ?? Date.now)();
    const force = opts.refresh === true && now - this.refreshedAt >= DEBOUNCE_MS;
    const expired = now - this.fetchedAt >= (this.result ? TTL_MS : BACKOFF_MS);
    const backoff = !this.result && now - this.fetchedAt < BACKOFF_MS;
    let fetched = false;
    if (!this.pending && !backoff && (force || expired)) {
      this.refreshedAt = now;
      const currentHost = sshHost;
      const pending = Promise.resolve()
        .then(() => (this.deps.runSource ?? runSource)(currentHost))
        .then(normalize)
        .catch(() => null);
      this.pending = pending;
      void pending.then((result) => {
        if (this.pending !== pending || this.source !== currentHost) return;
        this.result = result;
        this.fetchedAt = (this.deps.now ?? Date.now)();
        this.pending = null;
      });
    }
    const pending = this.pending;
    if (pending) {
      fetched = true;
      await pending;
    }
    if (this.source !== sshHost || !this.result) return [];
    return [
      {
        ...this.result,
        status: fetched ? 'ok' : 'cached',
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

import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { decodeCodexActivationIdentity } from '../../codex-auth/codex-activation-identity';
import { getCodexProfileNameError } from '../../codex-auth/types';
import { getCodexProfileRetryBlocked } from '../usage/native-quota-collector';
import type { DashboardAccount } from './account-dashboard-types';
import {
  isRecord,
  usageEmail,
  type UsageFailoverTarget,
  type UsageIdentity,
} from './usage-failover-contract';
import { getUsageFailoverService } from './usage-failover-service';
import { localUsageHost } from './usage-failover-transport';
import { codexUsageIdentity } from './usage-failover-codex-identity';
export { codexUsageIdentity } from './usage-failover-codex-identity';

const MAX_AUTH_BYTES = 1024 * 1024;
interface BoundAuth {
  digest: string;
  identity: UsageIdentity;
  email: string;
}
interface TargetBinding {
  file: string;
  profile: string;
  auth: BoundAuth;
}
const bindings = new WeakMap<UsageFailoverTarget, TargetBinding>();

export interface CodexUsageTargetOptions {
  /** Bare default only. Explicit temporary homes keep tests away from real logins. */
  homeDir?: string;
}
export interface CodexUsageRecovery {
  recover(target: UsageFailoverTarget, account: DashboardAccount): Promise<DashboardAccount>;
  peek(target: UsageFailoverTarget, account: DashboardAccount): DashboardAccount;
}

/** Read the same bytes the native collector binds, without logging auth/parse errors. */
function readBoundAuth(file: string): BoundAuth | null {
  try {
    const before = fs.lstatSync(file);
    if (
      !before.isFile() ||
      before.isSymbolicLink() ||
      before.size > MAX_AUTH_BYTES ||
      (process.getuid && before.uid !== process.getuid())
    )
      return null;
    const descriptor = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
    let raw: Buffer;
    try {
      const opened = fs.fstatSync(descriptor);
      if (opened.dev !== before.dev || opened.ino !== before.ino) return null;
      const buffer = Buffer.alloc(MAX_AUTH_BYTES + 1);
      let size = 0;
      while (size < buffer.length) {
        const count = fs.readSync(descriptor, buffer, size, buffer.length - size, null);
        if (!count) break;
        size += count;
      }
      if (size > MAX_AUTH_BYTES) return null;
      const after = fs.lstatSync(file);
      if (
        !after.isFile() ||
        after.isSymbolicLink() ||
        after.dev !== before.dev ||
        after.ino !== before.ino ||
        after.size !== before.size ||
        after.mtimeMs !== before.mtimeMs
      )
        return null;
      raw = buffer.subarray(0, size);
    } finally {
      fs.closeSync(descriptor);
    }
    const parsed: unknown = JSON.parse(raw.toString('utf8').replace(/^\uFEFF/, ''));
    if (!isRecord(parsed) || !isRecord(parsed.tokens)) return null;
    const identity = codexUsageIdentity(parsed.tokens);
    const decoded =
      typeof parsed.tokens.id_token === 'string'
        ? decodeCodexActivationIdentity(parsed.tokens.id_token, parsed.tokens.account_id)
        : null;
    const email = usageEmail(decoded?.email);
    if (!identity || !email) return null;
    return { digest: createHash('sha256').update(raw).digest('hex'), identity, email };
  } catch {
    return null;
  }
}

/** Capture before native quota collection so an old result cannot seed a new login's cache. */
export function codexUsageTarget(
  scope: string,
  profile: string | { name: string },
  options: CodexUsageTargetOptions = {}
): UsageFailoverTarget | null {
  const name = typeof profile === 'string' ? profile : profile.name;
  if (name !== 'default' && getCodexProfileNameError(name)) return null;
  const ccsDir = path.resolve(scope);
  const file =
    name === 'default'
      ? path.join(options.homeDir ?? os.homedir(), '.codex', 'auth.json')
      : path.join(ccsDir, 'codex-instances', name, 'auth.json');
  const auth = readBoundAuth(file);
  if (!auth) return null;
  const target: UsageFailoverTarget = {
    accountId: `codex:${name}`,
    provider: 'codex',
    ccsDir,
    binding: JSON.stringify(['codex-native-auth-v1', name, auth.digest]),
    primarySources: [{ ...localUsageHost(), selector: `codex-profile:${name}` }],
    expectedEmail: auth.email,
    expectedIdentity: auth.identity,
  };
  bindings.set(target, { file, profile: name, auth });
  return target;
}

function currentTarget(target: UsageFailoverTarget, account: DashboardAccount): boolean {
  const binding = bindings.get(target);
  if (
    !binding ||
    target.provider !== 'codex' ||
    account.provider !== 'codex' ||
    target.accountId !== account.id ||
    (account.email && account.email.toLowerCase() !== binding.auth.email.toLowerCase())
  )
    return false;
  const current = readBoundAuth(binding.file);
  return current?.digest === binding.auth.digest;
}

function changedLogin(account: DashboardAccount): DashboardAccount {
  const result: DashboardAccount = {
    ...account,
    email: null,
    plan: null,
    status: 'unavailable',
    source: 'Codex saved login',
    message: 'The saved Codex login changed; usage will refresh shortly.',
    windows: [],
    sampledAt: null,
    fetchedAt: null,
  };
  delete result.host;
  return result;
}

/** Existing native fetching/renewal stays primary; only its display reading may recover. */
export async function recoverCodexUsage(
  target: UsageFailoverTarget | null,
  account: DashboardAccount,
  service: CodexUsageRecovery = getUsageFailoverService(),
  retryBlocked: (profile: string) => boolean = getCodexProfileRetryBlocked
): Promise<DashboardAccount> {
  if (!target) return account;
  const binding = bindings.get(target);
  if (!binding || !currentTarget(target, account)) return changedLogin(account);
  if (retryBlocked(binding.profile)) return peekCodexUsage(target, account, service);
  const recovered = await service.recover(target, account).catch(() => account);
  return currentTarget(target, account) ? recovered : changedLogin(account);
}

/** Revalidate the same auth binding even when serving only a persisted fallback. */
export function peekCodexUsage(
  target: UsageFailoverTarget | null,
  account: DashboardAccount,
  service: CodexUsageRecovery = getUsageFailoverService()
): DashboardAccount {
  if (!target) return account;
  if (!currentTarget(target, account)) return changedLogin(account);
  const cached = service.peek(target, account);
  return currentTarget(target, account) ? cached : changedLogin(account);
}

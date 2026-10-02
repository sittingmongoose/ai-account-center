import { execFile } from 'child_process';
import { createHash, randomBytes } from 'crypto';
import { constants } from 'fs';
import fs from 'fs/promises';
import path from 'path';
import { ConfigError, NetworkError } from '../../errors/error-types';
import type { DashboardPlatform } from './account-dashboard-types';
import { isSafeUsageSshAlias } from './additional-usage-transport';

/**
 * The AAC-owned API-key store (CONTRACT-registry-lifecycle section 3.2).
 *
 * - Ubuntu and Mac: `~/.ccs/account-usage/keys/<provider>-<keyId>.json`, 0600,
 *   in a 0700 folder. The record is `{version:1, provider, keyId, secret,
 *   fingerprint, last4, createdAt}`, the format `plan_common.aac_key_credential`
 *   reads (scripts/account-usage/README.md, "Key file format").
 * - The dashboard host (Ubuntu) writes its own keys in-process: no child
 *   process, so the secret is never in any argv or environment.
 * - Another host is written through the fixed helper `key_store.py
 *   put|delete --provider <id> --key-id <hex>` over ssh, with the secret on
 *   stdin only; the helper prints only `{"ok":true,"fingerprint","last4"}`.
 * - Nothing here returns a secret: callers get `last4` and `fingerprint`.
 */
export const KEY_PROVIDERS = ['kimi-code', 'zai', 'opencode-go'] as const;
export type KeyProvider = (typeof KEY_PROVIDERS)[number];
/** 8-512 printable ASCII characters, no whitespace (the helper's KEY_SECRET). */
export const API_KEY_PATTERN = /^[\x21-\x7e]{8,512}$/;
export const KEY_ID_PATTERN = /^[a-f0-9]{8}$/;
const FINGERPRINT_PATTERN = /^sha256:[a-f0-9]{16}$/;
const MAX_KEY_FILE_BYTES = 4096;
const HELPER_TIMEOUT_MS = 25_000;
const MAX_HELPER_OUTPUT = 4096;

export interface StoredKeyInfo {
  fingerprint: string;
  last4: string;
  storedOn: DashboardPlatform;
}

export class KeyStoreError extends ConfigError {
  constructor() {
    super('The key store could not be written safely.');
    this.name = 'KeyStoreError';
  }
}

export function isKeyProvider(value: unknown): value is KeyProvider {
  return typeof value === 'string' && (KEY_PROVIDERS as readonly string[]).includes(value);
}

export function keyFingerprint(secret: string): string {
  return `sha256:${createHash('sha256').update(secret, 'utf8').digest('hex').slice(0, 16)}`;
}

export function keyLast4(secret: string): string {
  return secret.slice(-4);
}

export function newKeyId(): string {
  return randomBytes(4).toString('hex');
}

export interface KeyStoreBackend {
  readonly platform: DashboardPlatform;
  put(provider: KeyProvider, keyId: string, secret: string): Promise<StoredKeyInfo>;
  delete(provider: KeyProvider, keyId: string): Promise<void>;
  /** last4 and fingerprint of a stored key; null when absent, unreadable or remote. */
  info(provider: KeyProvider, keyId: string): Promise<StoredKeyInfo | null>;
  /** Fingerprints of every readable key of this provider (duplicate detection). */
  fingerprints(provider: KeyProvider): Promise<Set<string>>;
}

function checkIds(provider: unknown, keyId: unknown): void {
  if (!isKeyProvider(provider) || typeof keyId !== 'string' || !KEY_ID_PATTERN.test(keyId)) {
    throw new KeyStoreError();
  }
}

/** The dashboard host's own key folder. */
export class LocalKeyStore implements KeyStoreBackend {
  readonly platform: DashboardPlatform;
  private readonly directory: string;

  constructor(ccsDir: string, platform: DashboardPlatform = 'ubuntu') {
    this.directory = path.join(ccsDir, 'account-usage', 'keys');
    this.platform = platform;
  }

  private file(provider: KeyProvider, keyId: string): string {
    return path.join(this.directory, `${provider}-${keyId}.json`);
  }

  /** The folder is created at 0700, never a link, and tightened to 0700 if it was looser. */
  private async ensureDirectory(): Promise<void> {
    await fs.mkdir(this.directory, { recursive: true, mode: 0o700 });
    const stat = await fs.lstat(this.directory);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new KeyStoreError();
    const uid = process.getuid?.();
    if (uid !== undefined && stat.uid !== uid) throw new KeyStoreError();
    if (process.platform !== 'win32' && (stat.mode & 0o077) !== 0) {
      await fs.chmod(this.directory, 0o700);
    }
  }

  async put(provider: KeyProvider, keyId: string, secret: string): Promise<StoredKeyInfo> {
    checkIds(provider, keyId);
    if (!API_KEY_PATTERN.test(secret)) throw new KeyStoreError();
    await this.ensureDirectory();
    const info = { fingerprint: keyFingerprint(secret), last4: keyLast4(secret) };
    const record = {
      version: 1,
      provider,
      keyId,
      secret,
      ...info,
      createdAt: new Date().toISOString().replace(/\.\d{3}Z$/, 'Z'),
    };
    const target = this.file(provider, keyId);
    const temporary = path.join(this.directory, `.${provider}-${keyId}.${newKeyId()}.tmp`);
    let handle: Awaited<ReturnType<typeof fs.open>> | undefined;
    try {
      handle = await fs.open(temporary, 'wx', 0o600);
      await handle.writeFile(`${JSON.stringify(record)}\n`, 'utf8');
      await handle.chmod(0o600);
      await handle.sync();
      await handle.close();
      handle = undefined;
      await fs.rename(temporary, target);
    } catch {
      throw new KeyStoreError();
    } finally {
      await handle?.close().catch(() => undefined);
      await fs.rm(temporary, { force: true }).catch(() => undefined);
    }
    return { ...info, storedOn: this.platform };
  }

  async delete(provider: KeyProvider, keyId: string): Promise<void> {
    checkIds(provider, keyId);
    try {
      const stat = await fs.lstat(this.file(provider, keyId));
      if (!stat.isFile() && !stat.isSymbolicLink()) throw new KeyStoreError();
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return;
      throw new KeyStoreError();
    }
    await fs.rm(this.file(provider, keyId), { force: true }).catch(() => {
      throw new KeyStoreError();
    });
  }

  async info(provider: KeyProvider, keyId: string): Promise<StoredKeyInfo | null> {
    if (!isKeyProvider(provider) || !KEY_ID_PATTERN.test(keyId)) return null;
    let handle: Awaited<ReturnType<typeof fs.open>> | undefined;
    try {
      const directory = await fs.lstat(this.directory);
      if (!directory.isDirectory() || (process.platform !== 'win32' && directory.mode & 0o077)) {
        return null;
      }
      const noFollow = process.platform === 'win32' ? 0 : constants.O_NOFOLLOW;
      handle = await fs.open(this.file(provider, keyId), constants.O_RDONLY | noFollow);
      const stat = await handle.stat();
      if (
        !stat.isFile() ||
        stat.size > MAX_KEY_FILE_BYTES ||
        (process.platform !== 'win32' && stat.mode & 0o077)
      ) {
        return null;
      }
      const record = JSON.parse(await handle.readFile('utf8')) as Record<string, unknown>;
      const secret = record.secret;
      if (
        record.version !== 1 ||
        record.provider !== provider ||
        record.keyId !== keyId ||
        typeof secret !== 'string' ||
        !API_KEY_PATTERN.test(secret)
      ) {
        return null;
      }
      const fingerprint = keyFingerprint(secret);
      if (record.fingerprint !== undefined && record.fingerprint !== fingerprint) return null;
      return { fingerprint, last4: keyLast4(secret), storedOn: this.platform };
    } catch {
      return null;
    } finally {
      await handle?.close().catch(() => undefined);
    }
  }

  async fingerprints(provider: KeyProvider): Promise<Set<string>> {
    const found = new Set<string>();
    let names: string[];
    try {
      names = await fs.readdir(this.directory);
    } catch {
      return found;
    }
    const pattern = new RegExp(`^${provider}-([a-f0-9]{8})\\.json$`);
    for (const name of names) {
      const keyId = pattern.exec(name)?.[1];
      const info = keyId ? await this.info(provider, keyId) : null;
      if (info) found.add(info.fingerprint);
    }
    return found;
  }
}

/** Runs the fixed helper over ssh; `input` goes to stdin only. */
export type KeyHelperRunner = (
  sshHost: string,
  remoteCommand: string,
  input: string | null
) => Promise<string>;

const SSH_OPTIONS = [
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
];

export const runKeyHelperOverSsh: KeyHelperRunner = (sshHost, remoteCommand, input) =>
  new Promise((resolve, reject) => {
    if (!isSafeUsageSshAlias(sshHost)) {
      reject(new KeyStoreError());
      return;
    }
    const child = execFile(
      'ssh',
      [...SSH_OPTIONS, '--', sshHost, remoteCommand],
      {
        encoding: 'utf8',
        timeout: HELPER_TIMEOUT_MS,
        maxBuffer: MAX_HELPER_OUTPUT,
        windowsHide: true,
      },
      (error, stdout) => {
        // Helper output and ssh diagnostics are never surfaced or logged.
        if (error) reject(new NetworkError('The key store helper could not be reached.'));
        else resolve(stdout);
      }
    );
    child.stdin?.on('error', () => {
      /* The exit status reports the failure. */
    });
    child.stdin?.end(input ?? '');
  });

/** The helper's remote command line: fixed path, enumerated values, never the secret. */
export function keyHelperCommand(
  platform: 'mac' | 'windows',
  action: 'put' | 'delete',
  provider: KeyProvider,
  keyId: string
): string {
  checkIds(provider, keyId);
  const args = `${action} --provider '${provider}' --key-id '${keyId}'`;
  if (platform === 'windows') {
    const script = [
      "$ErrorActionPreference = 'Stop'",
      "$env:PYTHONIOENCODING = 'utf-8'",
      "$env:PYTHONUTF8 = '1'",
      "$helper = [IO.Path]::Combine($HOME, '.ccs', 'account-usage', 'key_store.py')",
      `$input | & python.exe $helper ${args}`,
      'exit $LASTEXITCODE',
    ].join('; ');
    return `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, 'utf16le').toString('base64')}`;
  }
  return `/usr/bin/python3 "$HOME/.ccs/account-usage/key_store.py" ${args}`;
}

function parseHelperReply(stdout: string, platform: DashboardPlatform): StoredKeyInfo {
  let reply: unknown;
  try {
    reply = JSON.parse(stdout.trim());
  } catch {
    throw new KeyStoreError();
  }
  const value = reply as Record<string, unknown> | null;
  if (
    !value ||
    typeof value !== 'object' ||
    Object.keys(value).sort().join(',') !== 'fingerprint,last4,ok' ||
    value.ok !== true ||
    typeof value.fingerprint !== 'string' ||
    !FINGERPRINT_PATTERN.test(value.fingerprint) ||
    typeof value.last4 !== 'string' ||
    !/^[\x21-\x7e]{1,4}$/.test(value.last4)
  ) {
    throw new KeyStoreError();
  }
  return { fingerprint: value.fingerprint, last4: value.last4, storedOn: platform };
}

/** A key kept on the Mac or Windows host that runs that account's collector. */
export class RemoteKeyStore implements KeyStoreBackend {
  constructor(
    readonly platform: 'mac' | 'windows',
    private readonly sshHost: string,
    private readonly run: KeyHelperRunner = runKeyHelperOverSsh
  ) {}

  async put(provider: KeyProvider, keyId: string, secret: string): Promise<StoredKeyInfo> {
    if (!API_KEY_PATTERN.test(secret)) throw new KeyStoreError();
    const command = keyHelperCommand(this.platform, 'put', provider, keyId);
    const info = parseHelperReply(await this.run(this.sshHost, command, secret), this.platform);
    if (info.fingerprint !== keyFingerprint(secret)) throw new KeyStoreError();
    return info;
  }

  async delete(provider: KeyProvider, keyId: string): Promise<void> {
    await this.run(this.sshHost, keyHelperCommand(this.platform, 'delete', provider, keyId), null);
  }

  async info(): Promise<StoredKeyInfo | null> {
    return null;
  }

  async fingerprints(): Promise<Set<string>> {
    return new Set();
  }
}

export interface KeyStoreLocation {
  platform: DashboardPlatform;
  sshHost: string | null;
}

export interface KeyStoreFactoryDeps {
  ccsDir: string;
  localPlatform?: DashboardPlatform;
  runHelper?: KeyHelperRunner;
}

/** The store for an account's collector host; null when that host cannot be reached safely. */
export function keyStoreFor(
  location: KeyStoreLocation,
  deps: KeyStoreFactoryDeps
): KeyStoreBackend | null {
  const local = deps.localPlatform ?? 'ubuntu';
  if (location.sshHost === null) {
    return location.platform === local ? new LocalKeyStore(deps.ccsDir, local) : null;
  }
  if (location.platform === 'ubuntu' || !isSafeUsageSshAlias(location.sshHost)) return null;
  return new RemoteKeyStore(location.platform, location.sshHost, deps.runHelper);
}

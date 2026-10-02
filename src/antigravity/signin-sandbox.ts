import { randomBytes } from 'crypto';
import { spawnSync } from 'child_process';
import fs from 'fs';
import path from 'path';
import { AntigravityError } from './errors';

/**
 * The isolated home an Antigravity sign-in runs in (the reviewed isolation of
 * Codex's staged sign-in lane, capture03 to V7):
 *
 * - bubblewrap with new user, PID, IPC and UTS namespaces; the whole root
 *   read-only; an owned 0700 folder bound over `~/.gemini`, so the official
 *   CLI sees an empty login and writes its new credential only there; private
 *   /tmp and log folders; the same HOME path, never a reassigned HOME;
 * - a private session bus with no service activation (`dbus-run-session` with
 *   a fixed config), so the Secret Service of the real session is out of
 *   reach and the CLI falls back to its credential file;
 * - only the AppArmor policy-query leaf is bound writable, when it exists;
 * - an environment allowlist without provider keys or desktop sockets, and
 *   browser stubs, so the CLI shows its SSH authorization URL instead of
 *   opening a browser.
 *
 * Nothing here reads or writes the live native login, shared history or
 * settings. The staging folder lives below the private `.ccs` folder and is
 * removed after every attempt.
 */
export const ANTIGRAVITY_SIGNIN_DIR = 'antigravity-signin';
export const ANTIGRAVITY_SIGNIN_STAGING_PREFIX = '.staging-';
const STAGING_NAME = /^\.staging-[a-f0-9]{16}$/;
const STAGING_MAX_AGE_MS = 60 * 60_000;
export const BWRAP = '/usr/bin/bwrap';
export const DBUS_RUN_SESSION = '/usr/bin/dbus-run-session';
export const APPARMOR_QUERY_LEAF = '/sys/kernel/security/apparmor/.access';
const BROWSER_STUBS = ['xdg-open', 'open', 'sensible-browser', 'x-www-browser', 'www-browser'];
const WORKDIR = 'AAC-signin';
/** Credential envelope bound: the native file is small JSON. */
const MAX_TOKEN_BYTES = 16_384;

/** A session bus that activates no service: nothing can answer for the Secret Service. */
export const PRIVATE_SESSION_BUS_CONFIG = [
  '<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"',
  ' "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">',
  '<busconfig>',
  '  <type>session</type>',
  '  <listen>unix:tmpdir=/tmp</listen>',
  '  <auth>EXTERNAL</auth>',
  '  <policy context="default">',
  '    <allow send_destination="*" eavesdrop="true"/>',
  '    <allow eavesdrop="true"/>',
  '    <allow own="*"/>',
  '  </policy>',
  '</busconfig>',
  '',
].join('\n');

/** Variables the official CLI may see; nothing else from the caller's environment. */
const PASSED_ENV = [
  'USER',
  'LOGNAME',
  'LANG',
  'LC_ALL',
  'TERM',
  'COLORTERM',
  'COLUMNS',
  'LINES',
  'SSH_CLIENT',
  'SSH_CONNECTION',
  'SSH_TTY',
  'HTTPS_PROXY',
  'HTTP_PROXY',
  'NO_PROXY',
  'https_proxy',
  'http_proxy',
  'no_proxy',
  'SSL_CERT_FILE',
  'SSL_CERT_DIR',
];

export interface SignInStaging {
  /** `<ccs>/antigravity-signin/.staging-<16 hex>` */
  directory: string;
  /** A home whose `.gemini` is the mask: the credential worker reads the new login here. */
  home: string;
  /** Bound over the real `~/.gemini` inside the sandbox. */
  mask: string;
  /** `<mask>/antigravity-cli/antigravity-oauth-token` */
  token: string;
  stubs: string;
  busConfig: string;
}

function privateFolder(target: string): void {
  fs.mkdirSync(target, { mode: 0o700 });
  fs.chmodSync(target, 0o700);
}

/** `<ccs>/antigravity-signin`, made when missing: an owned 0700 folder. */
export function signInRoot(ccsDir: string): string {
  const root = path.join(path.resolve(ccsDir), ANTIGRAVITY_SIGNIN_DIR);
  fs.mkdirSync(root, { recursive: true, mode: 0o700 });
  const rootStat = fs.lstatSync(root);
  if (
    !rootStat.isDirectory() ||
    rootStat.isSymbolicLink() ||
    (process.getuid && rootStat.uid !== process.getuid())
  )
    throw new AntigravityError('antigravity-signin-staging-unsafe');
  fs.chmodSync(root, 0o700);
  return root;
}

/** Owned 0700 folders, browser stubs and the bus config; nothing else. */
export function prepareSignInStaging(ccsDir: string): SignInStaging {
  const root = signInRoot(ccsDir);
  const directory = path.join(
    root,
    `${ANTIGRAVITY_SIGNIN_STAGING_PREFIX}${randomBytes(8).toString('hex')}`
  );
  privateFolder(directory);
  const home = path.join(directory, 'home');
  const mask = path.join(home, '.gemini');
  const stubs = path.join(directory, 'browser-stubs');
  for (const folder of [
    home,
    mask,
    path.join(mask, 'antigravity-cli'),
    path.join(mask, 'antigravity-cli', 'log'),
    path.join(mask, WORKDIR),
    stubs,
  ])
    privateFolder(folder);
  for (const name of BROWSER_STUBS) {
    fs.writeFileSync(path.join(stubs, name), '#!/bin/sh\nexit 0\n', { mode: 0o700, flag: 'wx' });
  }
  const busConfig = path.join(directory, 'session-bus.conf');
  fs.writeFileSync(busConfig, PRIVATE_SESSION_BUS_CONFIG, { mode: 0o600, flag: 'wx' });
  return {
    directory,
    home,
    mask,
    token: path.join(mask, 'antigravity-cli', 'antigravity-oauth-token'),
    stubs,
    busConfig,
  };
}

/**
 * The exact bubblewrap argv; the native CLI gets no argument at all. The
 * private /tmp comes first, so the binds after it stay visible even when a
 * home or the CCS folder lives below /tmp; the bus config and the CLI are
 * bound read-only by their own paths for the same reason. The user's runtime
 * folder (`/run/user/<uid>`, with the real session bus and keyring sockets)
 * is masked by an empty one when it exists.
 */
export function sandboxArgs(
  staging: SignInStaging,
  options: {
    realHome: string;
    nativeBinary: string;
    apparmorLeaf: string | null;
    runtimeDir?: string | null;
  }
): string[] {
  const realGemini = path.join(path.resolve(options.realHome), '.gemini');
  return [
    '--die-with-parent',
    '--unshare-user',
    '--unshare-pid',
    '--unshare-ipc',
    '--unshare-uts',
    '--ro-bind',
    '/',
    '/',
    '--tmpfs',
    '/tmp',
    ...(options.runtimeDir ? ['--tmpfs', options.runtimeDir] : []),
    '--ro-bind',
    staging.busConfig,
    staging.busConfig,
    '--ro-bind',
    options.nativeBinary,
    options.nativeBinary,
    '--bind',
    staging.mask,
    realGemini,
    ...(options.apparmorLeaf ? ['--bind', options.apparmorLeaf, options.apparmorLeaf] : []),
    '--tmpfs',
    path.join(realGemini, 'antigravity-cli', 'log'),
    '--proc',
    '/proc',
    '--dev',
    '/dev',
    '--chdir',
    path.join(realGemini, WORKDIR),
    DBUS_RUN_SESSION,
    '--config-file',
    staging.busConfig,
    options.nativeBinary,
  ];
}

/** The allowlisted environment, with the real HOME path and browser stubs first on PATH. */
export function sandboxEnvironment(
  staging: SignInStaging,
  options: { realHome: string; source: NodeJS.ProcessEnv }
): Record<string, string> {
  const env: Record<string, string> = {};
  for (const key of PASSED_ENV) {
    const value = options.source[key];
    if (typeof value === 'string' && !/[\x00-\x1f]/.test(value)) env[key] = value;
  }
  return {
    ...env,
    HOME: path.resolve(options.realHome),
    PATH: `${staging.stubs}:/usr/bin:/bin`,
    BROWSER: path.join(staging.stubs, 'xdg-open'),
    TERM: env.TERM ?? 'xterm-256color',
  };
}

export type SignInPreflight =
  | { ok: true; nativeBinary: string; apparmorLeaf: string | null; runtimeDir?: string | null }
  | { ok: false; reason: 'tool_missing' | 'preflight_failed'; detail: string };

export interface PreflightDeps {
  platform?: NodeJS.Platform;
  uid?: number | null;
  /** Runs the harmless namespace probe; returns its exit status. */
  probe?: () => number | null;
  exists?: (file: string) => boolean;
  /** The user's runtime folder to mask; defaults to `/run/user/<uid>` when it is a folder. */
  runtimeDir?: string | null;
}

function executable(file: string): boolean {
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return fs.statSync(file).isFile();
  } catch {
    return false;
  }
}

/** The official CLI at `~/.local/bin/agy`, owned by this user and not writable by others. */
export function nativeBinaryProblem(realHome: string, uid: number | null): string | null {
  const binary = path.join(path.resolve(realHome), '.local', 'bin', 'agy');
  try {
    const stat = fs.lstatSync(binary);
    if (!stat.isFile() || stat.isSymbolicLink())
      return 'The Antigravity CLI is not a regular file.';
    if (uid !== null && stat.uid !== uid) return 'The Antigravity CLI is owned by another user.';
    if ((stat.mode & 0o022) !== 0) return 'The Antigravity CLI is writable by other users.';
    if ((stat.mode & 0o100) === 0) return 'The Antigravity CLI is not executable.';
    return null;
  } catch {
    return 'The Antigravity CLI is not installed at ~/.local/bin/agy.';
  }
}

/**
 * The isolation preflight: Linux, the official CLI, bubblewrap and
 * dbus-run-session present, and one harmless namespace probe (`/bin/true`
 * in the same namespaces with the root read-only). It never starts the CLI.
 */
export function antigravitySignInPreflight(
  realHome: string,
  deps: PreflightDeps = {}
): SignInPreflight {
  const platform = deps.platform ?? process.platform;
  if (platform !== 'linux')
    return { ok: false, reason: 'preflight_failed', detail: 'Antigravity sign-in runs on Ubuntu.' };
  const uid = deps.uid === undefined ? (process.getuid?.() ?? null) : deps.uid;
  const problem = nativeBinaryProblem(realHome, uid);
  if (problem) return { ok: false, reason: 'tool_missing', detail: problem };
  const exists = deps.exists ?? executable;
  for (const tool of [BWRAP, DBUS_RUN_SESSION]) {
    if (!exists(tool))
      return {
        ok: false,
        reason: 'preflight_failed',
        detail: `${tool} is needed for the isolated sign-in.`,
      };
  }
  const probe =
    deps.probe ??
    (() =>
      spawnSync(
        BWRAP,
        [
          '--die-with-parent',
          '--unshare-user',
          '--unshare-pid',
          '--unshare-ipc',
          '--unshare-uts',
          '--ro-bind',
          '/',
          '/',
          '--tmpfs',
          '/tmp',
          '--proc',
          '/proc',
          '--dev',
          '/dev',
          '/bin/true',
        ],
        { stdio: 'ignore', timeout: 10_000, env: { PATH: '/usr/bin:/bin' } }
      ).status);
  if (probe() !== 0)
    return {
      ok: false,
      reason: 'preflight_failed',
      detail: 'This computer does not allow the isolated namespaces the sign-in needs.',
    };
  let apparmorLeaf: string | null = null;
  try {
    if (fs.statSync(APPARMOR_QUERY_LEAF).isFile()) apparmorLeaf = APPARMOR_QUERY_LEAF;
  } catch {
    apparmorLeaf = null;
  }
  let runtimeDir: string | null = null;
  if (deps.runtimeDir !== undefined) runtimeDir = deps.runtimeDir;
  else if (uid !== null) {
    try {
      const candidate = `/run/user/${uid}`;
      if (fs.lstatSync(candidate).isDirectory()) runtimeDir = candidate;
    } catch {
      runtimeDir = null;
    }
  }
  return {
    ok: true,
    nativeBinary: path.join(path.resolve(realHome), '.local', 'bin', 'agy'),
    apparmorLeaf,
    runtimeDir,
  };
}

/**
 * The state of the new credential in the mask: absent, still being written, a
 * complete consumer login (JSON with a consumer method and both tokens) in an
 * owned 0600 regular file with one link, or `unsafe`: a complete login in a
 * file that fails those checks, which the import then refuses. The file is
 * opened without following links and must be the file that was judged. Its
 * contents are never returned here.
 */
export function maskedCredentialState(
  staging: SignInStaging
): 'absent' | 'partial' | 'complete' | 'unsafe' {
  let before: fs.Stats;
  try {
    before = fs.lstatSync(staging.token);
  } catch {
    return 'absent';
  }
  if (!before.isFile() || before.isSymbolicLink()) return 'partial';
  let fd: number;
  try {
    fd = fs.openSync(staging.token, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
  } catch {
    return 'partial';
  }
  try {
    const opened = fs.fstatSync(fd);
    if (
      !opened.isFile() ||
      opened.dev !== before.dev ||
      opened.ino !== before.ino ||
      opened.size === 0 ||
      opened.size > MAX_TOKEN_BYTES
    )
      return 'partial';
    const value = JSON.parse(fs.readFileSync(fd, 'utf8')) as {
      auth_method?: unknown;
      token?: { access_token?: unknown; refresh_token?: unknown };
    };
    const complete =
      value?.auth_method === 'consumer' &&
      typeof value.token?.access_token === 'string' &&
      value.token.access_token.length > 0 &&
      typeof value.token?.refresh_token === 'string' &&
      value.token.refresh_token.length > 0;
    if (!complete) return 'partial';
    // The same guards as the read that imports it (readMaskedCredential).
    return opened.nlink !== 1 ||
      (opened.mode & 0o777) !== 0o600 ||
      (process.getuid && opened.uid !== process.getuid())
      ? 'unsafe'
      : 'complete';
  } catch {
    return 'partial';
  } finally {
    fs.closeSync(fd);
  }
}

/** Remove one staging folder and everything in it (the new credential included). */
export function removeSignInStaging(staging: Pick<SignInStaging, 'directory'>): void {
  if (!STAGING_NAME.test(path.basename(staging.directory))) return;
  fs.rmSync(staging.directory, { recursive: true, force: true });
}

/** At startup and daily: staging folders older than one hour are removed. */
export function sweepSignInStaging(ccsDir: string, now: number = Date.now()): number {
  const root = path.join(path.resolve(ccsDir), ANTIGRAVITY_SIGNIN_DIR);
  let names: string[];
  try {
    names = fs.readdirSync(root);
  } catch {
    return 0;
  }
  let removed = 0;
  for (const name of names) {
    if (!STAGING_NAME.test(name)) continue;
    const target = path.join(root, name);
    try {
      const stat = fs.lstatSync(target);
      if (!stat.isDirectory() || now - stat.mtimeMs < STAGING_MAX_AGE_MS) continue;
      fs.rmSync(target, { recursive: true, force: true });
      removed += 1;
    } catch {
      /* Retried at the next sweep. */
    }
  }
  return removed;
}

import { spawn, spawnSync } from 'child_process';
import fs from 'fs';
import tty from 'tty';
import {
  ANTIGRAVITY_MAX_PROFILES,
  AntigravityAccountLifecycle,
  AntigravityLifecycleError,
  type AntigravityLifecycleCode,
} from './account-lifecycle';
import {
  BWRAP,
  antigravitySignInPreflight,
  maskedCredentialState,
  prepareSignInStaging,
  removeSignInStaging,
  sandboxArgs,
  sandboxEnvironment,
  type SignInPreflight,
  type SignInStaging,
} from './signin-sandbox';
import { claimAntigravitySignInMarker, type SignInMarker } from './signin-marker';
import type { NativeCredential } from './types';

/**
 * `ai-account-center antigravity signin <profile>`: the terminal sign-in for
 * Antigravity on Ubuntu (CONTRACT-registry-lifecycle 6.6, terminal fallback).
 *
 * The official CLI runs inside the isolated home (signin-sandbox.ts) on the
 * user's own terminal: the user picks Google OAuth, opens the authorization
 * link in a browser, signs in to the account this profile is for, and pastes
 * the code into the CLI. This program never reads the link, the code or the
 * screen. It watches only for the new credential file in the isolated home;
 * once it is complete, the CLI is stopped before any task can be typed into
 * it, the credential is verified with the provider and saved as the profile
 * (a new profile for Add, the same Google account for Sign in again), and the
 * isolated home is removed. The live native login, history and settings are
 * never touched.
 */
export const ANTIGRAVITY_CREDENTIAL_FORMAT = 'antigravity-consumer-json';
const POLL_MS = 250;
const TIMEOUT_MS = 15 * 60_000;
const STOP_GRACE_MS = 2_000;
const MAX_TOKEN_BYTES = 16_384;
/** Leave the alternate screen, show the cursor, reset colours, mouse, paste and keyboard modes. */
const TERMINAL_RESET =
  '\x1b[?1049l\x1b[?25h\x1b[0m\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[<u\r\n';

const FAILURES: Readonly<Record<AntigravityLifecycleCode, string>> = Object.freeze({
  account_active:
    'This profile is the live Antigravity login. Switch to another profile first, then sign in again.',
  activation_running: 'An Antigravity switch is running. Wait for it to finish, then try again.',
  signin_running: 'A sign-in for this profile is already running.',
  unknown_account: 'That Antigravity profile no longer exists. Nothing was saved.',
  remove_failed: 'The profile could not be changed safely. Nothing was saved.',
  id_in_use: 'Another sign-in saved a profile with this name meanwhile. Nothing was saved.',
  too_many_accounts: 'The most Antigravity profiles allowed are already saved.',
  duplicate_identity: 'That Google account is already saved in another profile. Nothing was saved.',
  identity_mismatch:
    'You signed in to a different Google account than this profile has. Nothing was saved.',
  write_failed: 'The sign-in could not be verified or saved safely. Nothing was saved.',
});

export interface SandboxChild {
  onExit(listener: (code: number | null) => void): void;
  kill(signal: NodeJS.Signals): void;
}

export interface TerminalSignInIo {
  isInteractive(): boolean;
  write(text: string): void;
  /** `stty -g` of the user's terminal, restored after the CLI stops. */
  saveTerminal(): string | null;
  restoreTerminal(saved: string | null): void;
}

export interface TerminalSignInDeps {
  ccsDir: string;
  realHome: string;
  env: NodeJS.ProcessEnv;
  lifecycle: AntigravityAccountLifecycle;
  io?: TerminalSignInIo;
  preflight?: () => SignInPreflight;
  spawnSandbox?: (file: string, args: string[], env: Record<string, string>) => SandboxChild;
  wait?: (ms: number) => Promise<void>;
  now?: () => number;
  timeoutMs?: number;
  /**
   * While the CLI owns the terminal this process ignores SIGINT, SIGQUIT and
   * SIGTSTP; on SIGHUP or SIGTERM it stops the CLI, removes the isolated home
   * with the new credential and the running marker, and exits.
   */
  guardSignals?: boolean;
  /** Exit after SIGHUP or SIGTERM; process.exit by default. */
  exit?: (code: number) => void;
  /** Where the guarded signals arrive; this process by default. */
  signals?: Pick<NodeJS.EventEmitter, 'on' | 'off'>;
}

function defaultIo(): TerminalSignInIo {
  return {
    // isatty on the descriptors: creating process.stdin would open a reader
    // on the terminal the CLI is about to own.
    isInteractive: () => tty.isatty(0) && tty.isatty(1),
    write: (text) => {
      process.stdout.write(text);
    },
    saveTerminal: () => {
      const result = spawnSync('/bin/stty', ['-g'], {
        stdio: ['inherit', 'pipe', 'ignore'],
        encoding: 'utf8',
      });
      const value = result.status === 0 ? result.stdout.trim() : '';
      return /^[0-9a-fA-F:]+$/.test(value) ? value : null;
    },
    restoreTerminal: (saved) => {
      spawnSync('/bin/stty', saved ? [saved] : ['sane'], {
        stdio: ['inherit', 'ignore', 'ignore'],
      });
    },
  };
}

function defaultSpawn(file: string, args: string[], env: Record<string, string>): SandboxChild {
  // Same process group as this program: the CLI must own the terminal's
  // foreground to read keys, and Ctrl+C reaches it directly.
  const child = spawn(file, args, { stdio: 'inherit', env, shell: false });
  let exited: number | null | undefined;
  const listeners: Array<(code: number | null) => void> = [];
  const finish = (code: number | null) => {
    if (exited !== undefined) return;
    exited = code;
    for (const listener of listeners) listener(code);
  };
  child.once('exit', (code) => finish(code));
  child.once('error', () => finish(127));
  return {
    onExit: (listener) => {
      if (exited !== undefined) listener(exited);
      else listeners.push(listener);
    },
    kill: (signal) => {
      try {
        child.kill(signal);
      } catch {
        /* Already gone. */
      }
    },
  };
}

/** The new credential, read with the same guards as the native store: owned 0600, one link, bounded. */
export function readMaskedCredential(staging: SignInStaging): NativeCredential {
  const before = fs.lstatSync(staging.token);
  const fd = fs.openSync(staging.token, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
  try {
    const opened = fs.fstatSync(fd);
    if (
      !opened.isFile() ||
      opened.dev !== before.dev ||
      opened.ino !== before.ino ||
      opened.nlink !== 1 ||
      opened.size === 0 ||
      opened.size > MAX_TOKEN_BYTES ||
      (opened.mode & 0o777) !== 0o600 ||
      (process.getuid && opened.uid !== process.getuid())
    )
      throw new AntigravityLifecycleError('write_failed');
    const bytes = fs.readFileSync(fd);
    if (bytes.length === 0 || bytes.length > MAX_TOKEN_BYTES)
      throw new AntigravityLifecycleError('write_failed');
    return { format: ANTIGRAVITY_CREDENTIAL_FORMAT, bytes };
  } finally {
    fs.closeSync(fd);
  }
}

/** Runs one sign-in; returns the process exit code. */
export async function runAntigravityTerminalSignIn(
  profileId: string,
  deps: TerminalSignInDeps
): Promise<number> {
  const io = deps.io ?? defaultIo();
  const lifecycle = deps.lifecycle;
  const say = (line: string) => io.write(`${line}\n`);
  const fail = (line: string) => {
    say(`[X] ${line}`);
    return 1;
  };
  if (lifecycle.nameError(profileId))
    return fail(
      'Profile names start with a lowercase letter and use lowercase letters, digits, - or _ (at most 48).'
    );
  let mode: 'add' | 'signin-again';
  try {
    mode = lifecycle.hasProfile(profileId) ? 'signin-again' : 'add';
    if (lifecycle.activationRunning()) return fail(FAILURES.activation_running);
    if (mode === 'add' && lifecycle.profileCount() >= ANTIGRAVITY_MAX_PROFILES)
      return fail(FAILURES.too_many_accounts);
    if (mode === 'signin-again') {
      if (lifecycle.runtimeActiveProfileId() === profileId) return fail(FAILURES.account_active);
      let live: boolean;
      try {
        live = await lifecycle.isLiveNativeProfile(profileId);
      } catch {
        return fail(
          'Could not check whether this profile is the live Antigravity login. Nothing was started.'
        );
      }
      if (live) return fail(FAILURES.account_active);
    }
  } catch {
    return fail('The saved Antigravity profiles could not be read safely. Nothing was started.');
  }
  if (!io.isInteractive())
    return fail('Run this in an interactive terminal: the sign-in needs your keyboard.');
  const preflight = (deps.preflight ?? (() => antigravitySignInPreflight(deps.realHome)))();
  if (!preflight.ok) return fail(`${preflight.detail} Nothing was started.`);

  let marker: SignInMarker | null;
  let staging: SignInStaging;
  try {
    marker = claimAntigravitySignInMarker(deps.ccsDir, profileId);
  } catch {
    return fail('The private sign-in folder could not be prepared. Nothing was started.');
  }
  if (!marker) return fail(FAILURES.signin_running);
  try {
    staging = prepareSignInStaging(deps.ccsDir);
  } catch {
    marker.release();
    return fail('The private sign-in folder could not be prepared. Nothing was started.');
  }
  const held = marker;
  const wait =
    deps.wait ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const now = deps.now ?? Date.now;
  const ignored: NodeJS.Signals[] = ['SIGINT', 'SIGQUIT', 'SIGTSTP'];
  const ignore = () => undefined;
  let saved: string | null = null;
  let terminalSaved = false;
  let sandbox: SandboxChild | null = null;
  // A closed terminal (SIGHUP) or a stop request (SIGTERM) never leaves the
  // new credential behind for the daily sweep: it is removed before exiting.
  const abort = (code: number) => {
    sandbox?.kill('SIGKILL');
    try {
      removeSignInStaging(staging);
    } catch {
      /* Swept within a day. */
    }
    held.release();
    if (terminalSaved) {
      terminalSaved = false;
      try {
        io.restoreTerminal(saved);
        io.write(TERMINAL_RESET);
      } catch {
        /* The terminal is gone. */
      }
    }
    (deps.exit ?? ((value: number) => process.exit(value)))(code);
  };
  const onHangup = () => abort(129);
  const onTerminate = () => abort(143);
  const signals = deps.signals ?? process;
  if (deps.guardSignals !== false) {
    signals.on('SIGHUP', onHangup);
    signals.on('SIGTERM', onTerminate);
  }
  try {
    say(
      mode === 'add'
        ? `Adding Antigravity profile "${profileId}".`
        : `Signing in again to Antigravity profile "${profileId}".`
    );
    say('Antigravity starts in a private sign-in home on this computer. Your current login,');
    say('history and settings are not touched.');
    say('1. Choose Google OAuth.');
    say('2. Open the link it shows in your browser and sign in to the Google account for');
    say(`   "${profileId}".`);
    say('3. Paste the code back here and press Enter.');
    say('Antigravity closes by itself as soon as the sign-in is saved. Ctrl+C cancels.');
    say('');
    saved = io.saveTerminal();
    terminalSaved = true;
    if (deps.guardSignals !== false) for (const name of ignored) signals.on(name, ignore);
    const child = (deps.spawnSandbox ?? defaultSpawn)(
      BWRAP,
      sandboxArgs(staging, {
        realHome: deps.realHome,
        nativeBinary: preflight.nativeBinary,
        apparmorLeaf: preflight.apparmorLeaf,
        runtimeDir: preflight.runtimeDir ?? null,
      }),
      sandboxEnvironment(staging, { realHome: deps.realHome, source: deps.env })
    );
    sandbox = child;
    let exitCode: number | null | undefined;
    child.onExit((code) => {
      exitCode = code;
    });
    const deadline = now() + (deps.timeoutMs ?? TIMEOUT_MS);
    let complete = false;
    let timedOut = false;
    let stableChecks = 0;
    // A finished credential (complete, or complete in an unsafe file the
    // import refuses) stops the CLI before a task can be typed into it.
    const finished = () => ['complete', 'unsafe'].includes(maskedCredentialState(staging));
    while (exitCode === undefined) {
      await wait(POLL_MS);
      if (finished()) {
        // Twice in a row, so a credential still being written is never cut off.
        stableChecks += 1;
        if (stableChecks >= 2) {
          complete = true;
          break;
        }
      } else stableChecks = 0;
      if (now() >= deadline) {
        timedOut = true;
        break;
      }
    }
    if (exitCode === undefined) {
      child.kill('SIGTERM');
      const stopBy = now() + STOP_GRACE_MS;
      while (exitCode === undefined && now() < stopBy) await wait(50);
      if (exitCode === undefined) child.kill('SIGKILL');
      while (exitCode === undefined) await wait(50);
    }
    terminalSaved = false;
    io.restoreTerminal(saved);
    io.write(TERMINAL_RESET);
    if (!complete) complete = finished();
    if (!complete) {
      return fail(
        timedOut
          ? 'The sign-in took longer than 15 minutes and was stopped. Nothing was saved.'
          : 'The sign-in did not finish. Nothing was saved.'
      );
    }
    say('Checking the new sign-in with Google...');
    let credential: NativeCredential;
    try {
      credential = readMaskedCredential(staging);
    } catch {
      return fail(FAILURES.write_failed);
    }
    try {
      const result = await lifecycle.importSignIn({ profileId, mode, credential });
      say(
        mode === 'add'
          ? `Saved Antigravity profile "${profileId}" (${result.email}). It now appears in the dashboard.`
          : `Signed in again: Antigravity profile "${profileId}" (${result.email}) is saved.`
      );
      return 0;
    } catch (error) {
      return fail(
        error instanceof AntigravityLifecycleError ? FAILURES[error.code] : FAILURES.write_failed
      );
    }
  } finally {
    if (terminalSaved) {
      io.restoreTerminal(saved);
      io.write(TERMINAL_RESET);
    }
    if (deps.guardSignals !== false) {
      for (const name of ignored) signals.off(name, ignore);
      signals.off('SIGHUP', onHangup);
      signals.off('SIGTERM', onTerminate);
    }
    try {
      removeSignInStaging(staging);
    } catch {
      say('[!] The private sign-in folder could not be removed; it is swept within a day.');
    }
    held.release();
  }
}

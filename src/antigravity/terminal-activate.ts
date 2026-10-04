import fs from 'fs';
import path from 'path';
import readline from 'readline';
import tty from 'tty';
import { AntigravityAccountLifecycle } from './account-lifecycle';
import type { ActivateRequest, ActivationResult, SafeProcessDisplay } from './types';

/** The guarded switch-service path, shared with the dashboard activate route. */
export interface TerminalActivateRuntime {
  activate(request: ActivateRequest): Promise<ActivationResult>;
}

export interface TerminalActivateIo {
  isInteractive(): boolean;
  write(text: string): void;
  readAnswer(): Promise<string>;
}

export interface TerminalActivateDeps {
  runtime: TerminalActivateRuntime | null;
  lifecycle: AntigravityAccountLifecycle;
  ccsDir: string;
  uid: number | null;
  io?: TerminalActivateIo;
  ownsState?: (ccsDir: string, uid: number | null) => boolean;
}

function defaultIo(): TerminalActivateIo {
  return {
    isInteractive: () => tty.isatty(0) && tty.isatty(1),
    write: (text) => {
      process.stdout.write(text);
    },
    readAnswer: () =>
      new Promise((resolve) => {
        const input = readline.createInterface({ input: process.stdin });
        input.question('', (answer) => {
          input.close();
          resolve(answer);
        });
        input.on('close', () => resolve(''));
      }),
  };
}

/**
 * The local activate command runs only as the user that owns this computer's
 * Antigravity state: the state directory itself, owned, never a symlink.
 */
export function ownsAntigravityState(ccsDir: string, uid: number | null): boolean {
  try {
    const stat = fs.lstatSync(path.resolve(ccsDir));
    return stat.isDirectory() && !stat.isSymbolicLink() && uid !== null && stat.uid === uid;
  } catch {
    return false;
  }
}

const PROCESS_LABELS: Record<SafeProcessDisplay['role'], string> = {
  cli: 'Antigravity CLI',
  desktop: 'Antigravity Desktop',
  'language-server': 'Antigravity language server',
};

function safeProcesses(processes: unknown): Array<{ pid: number; label: string }> | null {
  if (!Array.isArray(processes) || processes.length > 32) return null;
  const rows: Array<{ pid: number; label: string }> = [];
  for (const process of processes) {
    const row = process as Partial<SafeProcessDisplay>;
    if (
      typeof row.pid !== 'number' ||
      !Number.isInteger(row.pid) ||
      row.pid < 1 ||
      row.pid > 2_147_483_647 ||
      !Object.prototype.hasOwnProperty.call(PROCESS_LABELS, row.role ?? '')
    )
      return null;
    rows.push({ pid: row.pid, label: PROCESS_LABELS[row.role as SafeProcessDisplay['role']] });
  }
  return rows;
}

const TERMINAL_FAILURES: Record<string, string> = {
  'invalid-profile': 'No saved Antigravity profile has that name.',
  busy: 'An Antigravity switch is running. Try again when it finishes.',
  deferred: 'Antigravity activation deferred this switch. Try again when the computer is idle.',
  'failed-rolled-back':
    'Antigravity activation failed safely and rolled the live login back. Try again.',
  'recovery-required':
    'The Antigravity switch is stuck. Run: ai-account-center antigravity recover',
  'stale-confirmation':
    'The activation review expired. Run the activate command again to review the running programs.',
  'unsupported-runtime-probe':
    'The Antigravity CLI is not the reviewed build; switching is paused until the runtime is reviewed.',
};

export async function runAntigravityTerminalActivate(
  profileId: string,
  deps: TerminalActivateDeps
): Promise<number> {
  const io = deps.io ?? defaultIo();
  const say = (line: string) => io.write(`${line}\n`);
  const fail = (line: string) => {
    say(`[X] ${line}`);
    return 1;
  };
  if (deps.lifecycle.nameError(profileId))
    return fail(
      'Profile names start with a lowercase letter and use lowercase letters, digits, - or _ (at most 48).'
    );
  if (!(deps.ownsState ?? ownsAntigravityState)(deps.ccsDir, deps.uid))
    return fail(
      "Antigravity activation runs only as the user that owns this computer's Antigravity state."
    );
  if (!deps.runtime) return fail('No Antigravity profiles on this computer.');
  const who = (result: ActivationResult) =>
    result.email ? `${result.profileId} (${result.email})` : result.profileId;
  const done = (result: ActivationResult): number | null => {
    if (result.status === 'active') {
      say('[OK] Antigravity activation switched the Ubuntu login.');
      say(`${who(result)} is now the active profile.`);
      return 0;
    }
    if (result.status === 'already-active') {
      say('[OK] That Antigravity profile is already active on Ubuntu.');
      return 0;
    }
    return null;
  };
  const attempt = { profileId, hostId: 'ubuntu', mode: 'manual' } as const;
  let result: ActivationResult;
  try {
    result = await deps.runtime.activate({ ...attempt });
  } catch (error) {
    const { PrivateStorageError } = await import('./registry');
    if (error instanceof PrivateStorageError && error.code === 'busy')
      return fail('An Antigravity switch is running. Try again when it finishes.');
    throw error;
  }
  const finished = done(result);
  if (finished !== null) return finished;
  if (result.status !== 'confirmation-required') {
    const message = TERMINAL_FAILURES[result.status];
    return message ? fail(message) : fail('Antigravity activation failed safely. Try again.');
  }
  if (!io.isInteractive())
    return fail(
      'Running Antigravity programs need your review first, and this is not an interactive terminal. Nothing was changed.'
    );
  const processes = safeProcesses(result.confirmation?.processes);
  if (!result.confirmation || !processes)
    return fail(
      'The running Antigravity programs could not be listed safely. Nothing was changed.'
    );
  say('Another Antigravity program is running on Ubuntu.');
  for (const process of processes) say(`  ${process.label}, process ${process.pid}`);
  say('Stop the listed programs, switch accounts, and restore their reviewed sessions? [y/N]');
  const answer = (await io.readAnswer().catch(() => '')).trim().toLowerCase();
  if (answer !== 'y' && answer !== 'yes') {
    say('Activation cancelled. Nothing was changed.');
    return 1;
  }
  let confirmed: ActivationResult;
  try {
    confirmed = await deps.runtime.activate({
      ...attempt,
      confirmationToken: result.confirmation.token,
    });
  } catch (error) {
    const { PrivateStorageError } = await import('./registry');
    if (error instanceof PrivateStorageError && error.code === 'busy')
      return fail('An Antigravity switch is running. Try again when it finishes.');
    throw error;
  }
  const confirmedDone = done(confirmed);
  if (confirmedDone !== null) return confirmedDone;
  const message = TERMINAL_FAILURES[confirmed.status];
  return message ? fail(message) : fail('Antigravity activation failed safely. Try again.');
}

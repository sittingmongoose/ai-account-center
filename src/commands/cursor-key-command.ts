/**
 * `ai-account-center dashboard cursor-key <command>`: AAC's own Cursor API key for the Cursor
 * account reader (DESIGN 17.3 in the share folder 2026-11-13-analytics-discovery).
 *
 *   set      Read the key from stdin (paste it, then Enter) and keep it in AAC's key store
 *   status   Whether a key is kept, with its last four characters
 *   remove   Forget the key
 *
 * The key is made for AAC on the Cursor dashboard. It is a `credential`: a 0600 file in
 * `~/.ccs/account-usage/keys/` (DPAPI on Windows), never in the history store, logs, argv or the
 * environment. Typed into a terminal, it is read with echo off, so it never reaches the
 * terminal's scrollback or a recording of it. This command hands stdin straight to the reader
 * binary, which stores it; nothing prints more than its last four characters and fingerprint.
 */
import { getCcsDir } from '../utils/config-manager';
import { runCursorAccount } from '../web-server/services/cursor-account-reader';

export interface CursorKeyCommandIo {
  out: (text: string) => void;
  err: (text: string) => void;
  /** The whole of stdin (the key), read only by `set`. */
  readStdin: () => Promise<string>;
  run?: (args: string[], input: string) => Promise<unknown>;
  ccsDir?: string;
}

const MAX_STDIN = 4096;
/** A Cursor key is 8 to 512 characters (the key store's own rule); nothing longer can be one. */
const MAX_KEY_TYPED = 512;

function readStdinPiped(): Promise<string> {
  return new Promise((resolve, reject) => {
    let text = '';
    process.stdin.setEncoding('utf8');
    process.stdin.on('data', (chunk: string) => {
      text += chunk;
      if (text.length > MAX_STDIN) reject(new Error('too_long'));
    });
    process.stdin.on('end', () => resolve(text));
    process.stdin.on('error', reject);
  });
}

/**
 * The key typed by hand at a terminal: the terminal is asked not to echo (raw mode), the input
 * is bounded, and Enter finishes it; Ctrl-C cancels. The terminal is restored on every exit path,
 * and a newline is printed so the next message starts on its own line. Not a terminal: the
 * piped reader, which still ends on EOF.
 */
function readStdinHidden(): Promise<string> {
  if (!process.stdin.isTTY) return readStdinPiped();
  try {
    process.stdin.setRawMode(true);
  } catch {
    return readStdinPiped();
  }
  return new Promise((resolve, reject) => {
    const stdin = process.stdin;
    const stop = (error: Error | null, value?: string) => {
      stdin.off('data', onData);
      stdin.off('end', onEnd);
      stdin.off('error', onError);
      for (const signal of ['SIGINT', 'SIGTERM'] as const)
        process.off(signal, onSignal as (...args: unknown[]) => void);
      try {
        stdin.setRawMode(false);
      } catch {
        /* The terminal is already gone; nothing to restore. */
      }
      stdin.pause();
      process.stderr.write('\n');
      if (error) reject(error);
      else resolve(value ?? '');
    };
    let text = '';
    const onSignal = () => stop(new Error('cancelled'));
    const onData = (chunk: string | Buffer) => {
      // With `setEncoding` the chunk is text, but Bun can still deliver bytes; a Cursor key is
      // ASCII either way.
      const input = typeof chunk === 'string' ? chunk : chunk.toString('utf8');
      for (const char of input) {
        if (char === '\r' || char === '\n') return stop(null, text);
        if (char === '\u0003') return stop(new Error('cancelled'));
        if (char === '\u0004') return stop(null, text);
        if (char === '\u007f' || char === '\b') {
          text = text.slice(0, -1);
          continue;
        }
        text += char;
        if (text.length > MAX_KEY_TYPED) return stop(new Error('too_long'));
      }
    };
    const onEnd = () => stop(null, text);
    const onError = (error: Error) => stop(error);
    stdin.on('data', onData);
    stdin.on('end', onEnd);
    stdin.on('error', onError);
    // Raw mode keeps the terminal from raising SIGINT itself, so every way out goes through
    // `stop`; an external signal still restores the terminal first.
    process.once('SIGINT', onSignal as (...args: unknown[]) => void);
    process.once('SIGTERM', onSignal as (...args: unknown[]) => void);
    process.stderr.write('Paste the key, then Enter (it is not shown): ');
    stdin.resume();
  });
}

function readStdin(): Promise<string> {
  return process.stdin.isTTY ? readStdinHidden() : readStdinPiped();
}

const defaultIo: CursorKeyCommandIo = {
  out: (text) => process.stdout.write(text),
  err: (text) => process.stderr.write(text),
  readStdin,
};

export function cursorKeyCommandHelp(): string {
  return [
    'Cursor account key (whole-account Cursor usage in Analytics)',
    '',
    'Usage: ai-account-center dashboard cursor-key <command>',
    '',
    'Commands:',
    '  set      Paste the API key made for AAC on the Cursor dashboard, then Enter (stdin;',
    '           typed input is not shown; piped input reads to the end)',
    '  status   Show whether a key is kept, with its last four characters',
    '  remove   Forget the key',
    '',
    'The key is never an argument and never printed.',
    '',
  ].join('\n');
}

function object(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

export async function handleCursorKeyCommand(
  args: string[],
  io: CursorKeyCommandIo = defaultIo
): Promise<number> {
  const run = io.run ?? runCursorAccount;
  const ccsDir = io.ccsDir ?? getCcsDir();
  const [command, ...rest] = args;
  if (!command || rest.length || ['help', '--help', '-h'].includes(command)) {
    io.out(cursorKeyCommandHelp());
    return command && !['help', '--help', '-h'].includes(command) ? 2 : 0;
  }
  try {
    if (command === 'set') {
      let secret: string;
      try {
        secret = (await io.readStdin()).trim();
      } catch (error) {
        if (error instanceof Error && error.message === 'cancelled') {
          io.err('[X] Cancelled; nothing was kept.\n');
          return 130;
        }
        io.err('[X] The key is longer than any Cursor key; nothing was kept.\n');
        return 1;
      }
      if (!secret) {
        io.err('[X] No key on stdin. Paste the key, then Enter (or pipe it in).\n');
        return 2;
      }
      const answer = object(await run(['key', 'put', '--ccs-dir', ccsDir], `${secret}\n`));
      if (answer.ok !== true) {
        io.err(
          answer.error === 'invalid_key'
            ? '[X] That is not a Cursor key (8 to 512 characters, no spaces); nothing was kept.\n'
            : '[X] The key could not be kept safely; nothing was kept.\n'
        );
        return 1;
      }
      io.out(
        `[OK] Cursor key kept (ending ${String(answer.last4)}). Analytics reads the account at its next refresh.\n`
      );
      return 0;
    }
    if (command === 'status') {
      const answer = object(await run(['key', 'status', '--ccs-dir', ccsDir], ''));
      if (answer.present === true) io.out(`Cursor key kept (ending ${String(answer.last4)}).\n`);
      else if (answer.error)
        io.out('A Cursor key file is there but is not safe to use (permissions or format).\n');
      else io.out('No Cursor key kept.\n');
      return 0;
    }
    if (command === 'remove') {
      const answer = object(await run(['key', 'delete', '--ccs-dir', ccsDir], ''));
      io.out(
        Number(answer.removed) > 0 ? '[OK] Cursor key forgotten.\n' : 'No Cursor key was kept.\n'
      );
      return 0;
    }
  } catch (error) {
    io.err(
      error instanceof Error && error.message === 'no_reader'
        ? '[X] The Cursor account reader is not built for this computer.\n'
        : '[X] The Cursor account reader did not answer.\n'
    );
    return 1;
  }
  io.out(cursorKeyCommandHelp());
  return 2;
}

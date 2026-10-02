import { randomInt, timingSafeEqual } from 'crypto';
import fs from 'fs/promises';
import { authFile, authNow, ensureAuthDirectory, withAuthWriteGate } from './dashboard-auth-files';
import { writePrivateTextFile } from './private-json-store';

/**
 * The one-time setup code for a first run from the LAN (CONTRACT-auth-devices
 * section 4): 8 characters from an alphabet without 0, O, 1 or I, shown as
 * `XXXX-XXXX`, compared timing-safe ignoring the dash and case. It is written
 * to `~/.ccs/auth/setup-code` (0600) and printed once on the server's stdout,
 * never to the log files. It lasts 60 minutes or until used; a restart makes a
 * new one. The code lives in memory; the file is for the person at the VM.
 */
const ALPHABET = '23456789ABCDEFGHJKLMNPQRSTUVWXYZ';
export const SETUP_CODE_LIFETIME_MS = 60 * 60 * 1000;

interface PendingCode {
  file: string;
  code: string;
  createdAt: number;
}

let pending: PendingCode | null = null;

function makeCode(): string {
  let code = '';
  for (let index = 0; index < 8; index += 1) code += ALPHABET[randomInt(ALPHABET.length)];
  return code;
}

export function formatSetupCode(code: string): string {
  return `${code.slice(0, 4)}-${code.slice(4)}`;
}

function normalize(value: string): string | null {
  const bare = value.replace(/-/g, '').toUpperCase();
  return bare.length === 8 && [...bare].every((char) => ALPHABET.includes(char)) ? bare : null;
}

/**
 * Make a new code for this server run (replacing any earlier one), write the
 * file and hand the formatted code to `print`. Returns the formatted code.
 */
export function createSetupCode(print: (formatted: string) => void): Promise<string> {
  return withAuthWriteGate(async () => {
    await ensureAuthDirectory();
    const file = authFile('setup-code');
    const code = makeCode();
    // The same atomic 0600 write as the other auth files (random temporary name, `wx`, fsync).
    await writePrivateTextFile(file, `${formatSetupCode(code)}\n`);
    pending = { file, code, createdAt: authNow() };
    print(formatSetupCode(code));
    return formatSetupCode(code);
  });
}

/** Whether a code is waiting for this CCS directory and has not expired. */
export function hasActiveSetupCode(): boolean {
  return (
    pending !== null &&
    pending.file === authFile('setup-code') &&
    authNow() - pending.createdAt < SETUP_CODE_LIFETIME_MS
  );
}

/** Timing-safe check of a submitted code against the active one. */
export function setupCodeMatches(submitted: string): boolean {
  if (!hasActiveSetupCode() || !pending) return false;
  const candidate = normalize(submitted);
  const expected = Buffer.from(pending.code, 'ascii');
  // A malformed code is still compared (against itself) so its timing matches.
  if (candidate === null) return !timingSafeEqual(expected, expected);
  return timingSafeEqual(Buffer.from(candidate, 'ascii'), expected);
}

/** After setup: the code is used up and its file deleted. Call inside the auth write gate. */
export async function consumeSetupCode(): Promise<void> {
  const file = authFile('setup-code');
  if (pending?.file === file) pending = null;
  await fs.rm(file, { force: true });
}

/** Tests only. */
export function resetSetupCodeForTests(): void {
  pending = null;
}

/**
 * The interactive half of `cursor-key set` (the H1 fix): at a terminal the key is read with echo
 * off, and Enter finishes it. The test runs the real command from source under a
 * pseudo-terminal, the way an operator pastes into a terminal, using `script` (util-linux),
 * which is already on every machine this suite runs on; nothing is installed.
 *
 * The reader binary is deliberately absent (the command runs from source): the key is read
 * hidden first, then the command says its reader is not built. That is enough to prove the two
 * security properties: the typed key never reaches the terminal's output, and Enter alone
 * completes the command (the old reader waited for EOF, with echo on).
 */
import { spawn } from 'child_process';
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

const SCRIPT = '/usr/bin/script';
const KEY = 'key_TEST_0123456789abcdef_NOT_REAL';

describe('the interactive key prompt, under a pseudo-terminal', () => {
  let dir: string;
  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-cursor-key-pty-'));
  });
  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it.skipIf(!fs.existsSync(SCRIPT) || !process.env.PATH?.includes('bun'))(
    'hides the typed key and finishes on Enter',
    async () => {
      const entry = path.join(import.meta.dir, '../../../src/ccs.ts');
      const command = `bun ${JSON.stringify(entry)} dashboard cursor-key set`;
      const child = spawn(SCRIPT, ['-qec', command, '/dev/null'], {
        env: { ...process.env, NO_COLOR: '1', CCS_HOME: path.join(dir, 'home') },
        stdio: ['pipe', 'pipe', 'pipe'],
      });
      let seen = '';
      const arrive = (chunk: Buffer | string) => {
        seen += String(chunk);
      };
      child.stdout.on('data', arrive);
      child.stderr.on('data', arrive);
      const done = new Promise<number | null>((resolve, reject) => {
        child.on('error', reject);
        child.on('exit', (code) => resolve(code));
      });
      try {
        // Wait for the prompt: only then is the terminal in raw mode, and only then is the key
        // written, so nothing can be echoed before echo is off.
        const started = Date.now();
        while (!seen.includes('Paste the key')) {
          if (Date.now() - started > 15_000) throw new Error('the prompt never appeared');
          await new Promise((resolve) => setTimeout(resolve, 50));
        }
        child.stdin.write(`${KEY}\r`);
        const code = await Promise.race([
          done,
          new Promise<never>((_, reject) =>
            setTimeout(() => reject(new Error('Enter did not finish the command')), 15_000)
          ),
        ]);
        // The typed key never reaches the terminal's output (scrollback, recordings, screenshots).
        expect(seen.includes(KEY)).toBe(false);
        // Enter completed the command; stdin was never closed.
        expect(code).toBe(1);
        expect(seen).toContain('The Cursor account reader is not built');
      } finally {
        child.kill();
      }
    },
    45_000
  );
});

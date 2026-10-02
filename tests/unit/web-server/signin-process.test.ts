/**
 * The sign-in process launcher with a real fake CLI (a temporary Python
 * script): the PTY relay gives the CLI a terminal, input reaches it without
 * echo, output arrives in memory, and kill ends the whole process group.
 * Spawns child processes, so it is in the slow bucket.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { spawnSignInProcess } from '../../../src/web-server/services/signin-process';
import { SignInOutputParser } from '../../../src/web-server/services/signin-output';

const dirs: string[] = [];
afterEach(() => {
  for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

const FAKE_CLI = `
import os, sys, time
print("tty" if os.isatty(0) and os.isatty(1) else "notty", flush=True)
print("home=" + os.environ.get("CODEX_HOME", ""), flush=True)
print("secret-env=" + os.environ.get("AAC_TEST_SECRET", "absent"), flush=True)
print("\\x1b[94mhttps://auth.openai.com/codex/device\\x1b[0m", flush=True)
print("   ABCD-12345", flush=True)
open(sys.argv[1], "w").write(str(os.getpid()))
if len(sys.argv) > 2:
    line = sys.stdin.readline()
    print("got=" + line.strip(), flush=True)
    sys.exit(0)
time.sleep(60)
`;

function setup() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-signin-process-'));
  dirs.push(dir);
  const script = path.join(dir, 'fake-codex.py');
  fs.writeFileSync(script, FAKE_CLI);
  return { dir, script, pidFile: path.join(dir, 'pid') };
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function until(check: () => boolean, ms = 10_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!check()) {
    if (Date.now() > deadline) throw new Error('timed out');
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}

describe('spawnSignInProcess', () => {
  it('runs the CLI on a PTY with only the given environment, and kill ends it', async () => {
    const { script, pidFile } = setup();
    process.env.AAC_TEST_SECRET = 'must-not-leak';
    try {
      const child = spawnSignInProcess({
        file: '/usr/bin/python3',
        args: [script, pidFile],
        env: { PATH: process.env.PATH ?? '/usr/bin:/bin', CODEX_HOME: '/staging/x' },
        pty: true,
      });
      let output = '';
      const parser = new SignInOutputParser({
        allowedOrigins: ['https://auth.openai.com'],
        expectsUserCode: true,
      });
      child.onData((chunk) => {
        output += chunk;
        parser.push(chunk);
      });
      let exited: number | null | undefined;
      child.onExit((code) => {
        exited = code;
      });
      await until(() => parser.current === 'ready' && fs.existsSync(pidFile));
      expect(output).toContain('tty');
      expect(output).not.toContain('notty');
      expect(output).toContain('home=/staging/x');
      expect(output).toContain('secret-env=absent');
      expect(parser.verification()).toEqual({
        url: 'https://auth.openai.com/codex/device',
        userCode: 'ABCD-12345',
      });
      const pid = Number(fs.readFileSync(pidFile, 'utf8'));
      expect(alive(pid)).toBe(true);
      child.kill();
      await until(() => exited !== undefined);
      await until(() => !alive(pid));
      expect(exited).not.toBe(0);
    } finally {
      delete process.env.AAC_TEST_SECRET;
    }
  });

  it('passes input to the CLI without echoing it back', async () => {
    const { script, pidFile } = setup();
    const child = spawnSignInProcess({
      file: '/usr/bin/python3',
      args: [script, pidFile, 'read'],
      env: { PATH: process.env.PATH ?? '/usr/bin:/bin' },
      pty: true,
    });
    let output = '';
    child.onData((chunk) => {
      output += chunk;
    });
    let exited: number | null | undefined;
    child.onExit((code) => {
      exited = code;
    });
    await until(() => output.includes('ABCD-12345'));
    expect(child.write('4/0Code\n')).toBe(true);
    await until(() => exited !== undefined);
    expect(exited).toBe(0);
    expect(output).toContain('got=4/0Code');
    expect(output.split('4/0Code').length - 1).toBe(1);
    expect(child.write('late')).toBe(false);
  });

  it('runs without a PTY when asked and reports a missing binary', async () => {
    const { script, pidFile } = setup();
    const child = spawnSignInProcess({
      file: '/usr/bin/python3',
      args: [script, pidFile, 'read'],
      env: { PATH: process.env.PATH ?? '/usr/bin:/bin' },
      pty: false,
    });
    let output = '';
    child.onData((chunk) => {
      output += chunk;
    });
    await until(() => output.includes('ABCD-12345'));
    expect(output.startsWith('notty')).toBe(true);
    child.write('x\n');
    const missing = spawnSignInProcess({
      file: '/nonexistent/codex',
      args: [],
      env: {},
      pty: false,
    });
    const failure = await new Promise((resolve) => missing.onExit((_, reason) => resolve(reason)));
    expect(failure).toBe('missing');
  });
});

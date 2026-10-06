/**
 * The supervised Antigravity sign-in driver (src/antigravity/signin-driver.ts)
 * against a real fake CLI on a real PTY: it presses Enter through the
 * first-run login-method screen, surfaces the one authorization URL (from an
 * OSC 8 hyperlink or from plain wrapped text), feeds the pasted code back with
 * a carriage return, and stops the CLI once the new credential is complete.
 * Spawns child processes, so it is in the slow bucket.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { spawnSync } from 'child_process';
import {
  spawnSignInProcess,
  type SignInProcessHandle,
} from '../../../src/web-server/services/signin-process';
import { SignInOutputParser } from '../../../src/web-server/services/signin-output';
import {
  AGY_DRIVER_PYTHON,
  AGY_SIGNIN_DRIVER_PROGRAM,
} from '../../../src/antigravity/signin-driver';

const ORIGIN = 'https://accounts.google.com';
const FULL_URL =
  'https://accounts.google.com/o/oauth2/auth?access_type=offline' +
  '&client_id=1071.apps.googleusercontent.com&code_challenge=abc123Challenge' +
  '&code_challenge_method=S256&prompt=consent' +
  '&redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback' +
  '&response_type=code&scope=openid+email&state=STATExyz-9';
const LOOKALIKE_URL = FULL_URL.replace('accounts.google.com', 'accounts.google.com.evil.test');

/**
 * The fake CLI's screen from the URL to the code prompt, one PTY write per
 * entry. Plain terminals get the URL hard-wrapped into 120-character chunks;
 * OSC 8 terminals get a hyperlink whose visible text is cut short.
 */
function plainUrlWrites(url: string): string[] {
  const chunks: string[] = [];
  for (let i = 0; i < url.length; i += 120) chunks.push(' ' + url.slice(i, i + 120) + '\x1b[K\r\n');
  return [' Open the URL below in your browser:\r\n', ...chunks];
}
function osc8UrlWrites(url: string): string[] {
  return [
    ' Open the URL below in your browser:\r\n',
    ' \x1b[34;4m\x1b]8;id=xyz;' + url + '\x07' + url.slice(0, 50) + '\x1b[m\x1b]8;;\x07\r\n',
  ];
}
const PROMPT_WRITES = [
  '\r\n After authenticating, copy the code displayed in the browser and paste it below:\r\n',
  ' authorization code...\r\n',
];

const dirs: string[] = [];
afterEach(() => {
  for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

/**
 * The fake CLI. argv[1] is the token path. It models the real 1.2.16 screens:
 * a raw-mode login-method menu, then the authorization URL (OSC 8 or plain
 * wrapped, per FAKE_URL_MODE), then it reads the pasted code, records what it
 * received and its own pid, and writes the credential envelope.
 * FAKE_URL_MODE=nomenu skips the menu entirely (a CLI that remembers the
 * login method), and FAKE_EXIT_EARLY makes it quit before writing a token
 * (the "CLI died" path). Otherwise it hangs so the driver has to stop it,
 * like the real CLI at its prompt.
 */
function fakeCli(tokenPath: string): string {
  return `
import os, sys, time, tty
TOKEN = ${JSON.stringify(tokenPath)}
MODE = os.environ.get("FAKE_URL_MODE", "osc8")
URL_WRITES = {
    "plain": ${JSON.stringify(plainUrlWrites(FULL_URL))},
    "badhost": ${JSON.stringify(plainUrlWrites(LOOKALIKE_URL))},
    "osc8": ${JSON.stringify(osc8UrlWrites(FULL_URL))},
}
PROMPT_WRITES = ${JSON.stringify(PROMPT_WRITES)}
def out(s):
    sys.stdout.write(s); sys.stdout.flush()
tty.setraw(sys.stdin.fileno())
with open(os.environ["FAKE_PID"], "w") as f:
    f.write(str(os.getpid()))
out("\\x1b[?1049h\\x1b[H\\x1b[2J")
out(" Welcome to the Antigravity CLI. You are currently not signed in.\\r\\n\\r\\n")
if MODE != "nomenu":
    out(" Select login method:\\r\\n")
    out(" \\x1b[1m> 1. Google OAuth\\x1b[m\\r\\n")
    out("   2. Use a Google Cloud project\\r\\n\\r\\n")
    out("   up/down Navigate - enter Select\\r\\n")
    menu_key = sys.stdin.read(1)
    with open(os.environ["FAKE_RECEIPT"], "w") as f:
        f.write("menu=" + repr(menu_key) + "\\n")
if os.environ.get("FAKE_EXIT_EARLY"):
    time.sleep(0.2); sys.exit(0)
for piece in URL_WRITES.get(MODE, URL_WRITES["osc8"]):
    out(piece)
    # A pause after each line lets the driver's read end right there, the
    # split that once surfaced a wrapped link without its later lines.
    time.sleep(0.05)
if MODE == "badhost":
    with open(os.environ["FAKE_RECEIPT"], "a") as f:
        f.write("url=" + MODE + "\\n")
for piece in PROMPT_WRITES:
    out(piece)
if MODE == "badhost":
    with open(os.environ["FAKE_RECEIPT"], "a") as f:
        f.write("prompt=1\\n")
code = ""
while True:
    ch = sys.stdin.read(1)
    if not ch or ch in ("\\r", "\\n"):
        break
    code += ch
with open(os.environ["FAKE_CODE"], "w") as f:
    f.write(code)
fd = os.open(TOKEN, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
with os.fdopen(fd, "w") as f:
    f.write('{"auth_method":"consumer","token":{"access_token":"ya29.f","refresh_token":"1//r"}}')
for _ in range(600):
    time.sleep(0.1)
`;
}

interface Setup {
  dir: string;
  token: string;
  receipt: string;
  codeFile: string;
  pidFile: string;
  childArgv: string[];
  env: Record<string, string>;
}

function setup(mode: 'osc8' | 'plain' | 'nomenu' | 'badhost', early = false): Setup {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-driver-'));
  dirs.push(dir);
  const token = path.join(dir, 'antigravity-oauth-token');
  const script = path.join(dir, 'fake-agy.py');
  fs.writeFileSync(script, fakeCli(token));
  const receipt = path.join(dir, 'receipt.txt');
  const codeFile = path.join(dir, 'code.txt');
  const pidFile = path.join(dir, 'pid.txt');
  return {
    dir,
    token,
    receipt,
    codeFile,
    pidFile,
    childArgv: ['/usr/bin/python3', script, token],
    env: {
      PATH: '/usr/bin:/bin',
      FAKE_URL_MODE: mode,
      FAKE_RECEIPT: receipt,
      FAKE_CODE: codeFile,
      FAKE_PID: pidFile,
      ...(early ? { FAKE_EXIT_EARLY: '1' } : {}),
    },
  };
}

interface Run {
  child: SignInProcessHandle;
  url(): string | null;
  exited(): number | null | undefined;
}

function startDriver(s: Setup): Run {
  let exit: number | null | undefined;
  const parser = new SignInOutputParser({
    allowedOrigins: [ORIGIN],
    expectsUserCode: false,
    keepQuery: true,
  });
  const child = spawnSignInProcess({
    file: AGY_DRIVER_PYTHON,
    args: ['-c', AGY_SIGNIN_DRIVER_PROGRAM, s.token, ORIGIN, '--', ...s.childArgv],
    env: s.env,
    pty: false,
  });
  child.onData((chunk) => parser.push(chunk));
  child.onExit((code) => {
    exit = code;
  });
  return {
    child,
    url: () => (parser.current === 'ready' ? (parser.verification()?.url ?? null) : null),
    exited: () => exit,
  };
}

/**
 * Poll a real condition on a real subprocess (PTY output, a file the fake CLI
 * writes, or process exit). Fake timers cannot drive an OS child process, so
 * this waits on the actual signal with a short poll interval, not a guessed
 * fixed sleep.
 */
async function until(check: () => boolean, ms = 20_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!check()) {
    if (Date.now() > deadline) throw new Error('timed out waiting');
    // ES2020 lib has no Promise.withResolvers; executor form is required here.
    await new Promise<void>((resolve) => setTimeout(resolve, 25));
  }
}

/**
 * The driver's buffer is always a prefix of what the CLI wrote, wherever the
 * PTY happens to split the reads. Run the driver's own extract_url (its
 * program up to the PTY fork, which only defines things) over every prefix of
 * a screen, and report each distinct URL it would have printed.
 */
function urlsAtEveryPrefix(screen: string): { seen: string[]; whole: string | null } {
  const fork = AGY_SIGNIN_DRIVER_PROGRAM.indexOf('\npid, master = pty.fork()');
  expect(fork).toBeGreaterThan(0);
  const harness = [
    'import json, sys',
    'job = json.load(sys.stdin)',
    'sys.argv = ["driver", "/nonexistent-token", job["origin"], "--", "/bin/true"]',
    'exec(job["prelude"])',
    'screen = job["screen"].encode()',
    'seen = []',
    'for end in range(len(screen) + 1):',
    '    url = extract_url(screen[:end])',
    '    if url is not None and url.decode() not in seen:',
    '        seen.append(url.decode())',
    'whole = extract_url(screen)',
    'print(json.dumps({"seen": seen, "whole": whole.decode() if whole else None}))',
  ].join('\n');
  const result = spawnSync(AGY_DRIVER_PYTHON, ['-c', harness], {
    input: JSON.stringify({
      origin: ORIGIN,
      prelude: AGY_SIGNIN_DRIVER_PROGRAM.slice(0, fork),
      screen,
    }),
    encoding: 'utf8',
  });
  expect(result.stderr).toBe('');
  expect(result.status).toBe(0);
  return JSON.parse(result.stdout) as { seen: string[]; whole: string | null };
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

describe('Antigravity supervised sign-in driver', () => {
  it('drives the OSC 8 screen: menu Enter, full URL, code with CR, then stops the CLI on the complete token', async () => {
    const s = setup('osc8');
    const run = startDriver(s);
    try {
      await until(() => run.url() !== null);
      // The URL reaches the parser complete (query intact), never truncated.
      expect(run.url()).toBe(FULL_URL);
      // The driver pressed Enter on the highlighted Google OAuth menu entry.
      await until(() => fs.existsSync(s.receipt));
      expect(fs.readFileSync(s.receipt, 'utf8').trim()).toBe("menu='\\r'");
      // Feed the code the way the runner does (code + \n); the CLI receives CR.
      run.child.write('4/0Ab-FakeCode\n');
      await until(() => run.exited() !== undefined, 25_000);
      expect(fs.readFileSync(s.codeFile, 'utf8')).toBe('4/0Ab-FakeCode');
      expect(run.exited()).toBe(0);
      expect(fs.existsSync(s.token)).toBe(true);
    } finally {
      run.child.kill();
    }
  });

  it('rejoins a plain wrapped URL when the terminal has no OSC 8 hyperlinks', async () => {
    const s = setup('plain');
    const run = startDriver(s);
    try {
      await until(() => run.url() !== null);
      expect(run.url()).toBe(FULL_URL);
    } finally {
      run.child.kill();
    }
  });

  it('never surfaces part of a plain wrapped URL, wherever the PTY splits the reads', () => {
    // A read that ends right after a wrapped line used to surface the URL
    // without its later lines (the parser then saw URL[:240]).
    const { seen, whole } = urlsAtEveryPrefix(
      [...plainUrlWrites(FULL_URL), ...PROMPT_WRITES].join('')
    );
    expect(seen).toEqual([FULL_URL]);
    expect(whole).toBe(FULL_URL);
  });

  it('never surfaces part of an OSC 8 URL, wherever the PTY splits the reads', () => {
    const { seen, whole } = urlsAtEveryPrefix(
      [...osc8UrlWrites(FULL_URL), ...PROMPT_WRITES].join('')
    );
    expect(seen).toEqual([FULL_URL]);
    expect(whole).toBe(FULL_URL);
  });

  it('never surfaces any prefix of a plain URL whose host only starts with the allowed origin', () => {
    const { seen, whole } = urlsAtEveryPrefix(
      [...plainUrlWrites(LOOKALIKE_URL), ...PROMPT_WRITES].join('')
    );
    expect(seen).toEqual([]);
    expect(whole).toBe(null);
  });

  it('surfaces the URL when the CLI skips the login-method screen', async () => {
    const s = setup('nomenu');
    const run = startDriver(s);
    try {
      await until(() => run.url() !== null);
      expect(run.url()).toBe(FULL_URL);
      // No menu was on screen, so the driver pressed nothing and waited for no key.
      expect(fs.existsSync(s.receipt)).toBe(false);
    } finally {
      run.child.kill();
    }
  });

  it('never surfaces a plain URL whose host only starts with the allowed origin', async () => {
    const s = setup('badhost');
    const run = startDriver(s);
    try {
      // The fake CLI records prompt=1 only after the lookalike left for the PTY,
      // so once that is on disk the driver has certainly seen those bytes; no
      // wall-clock wait can drive an OS child, and none is needed here.
      await until(
        () => fs.existsSync(s.receipt) && fs.readFileSync(s.receipt, 'utf8').includes('prompt=1')
      );
      expect(run.url()).toBe(null);
    } finally {
      run.child.kill();
    }
  });

  it('exits non-zero when the CLI dies before writing a credential', async () => {
    const s = setup('osc8', true);
    const run = startDriver(s);
    try {
      await until(() => run.exited() !== undefined, 25_000);
      expect(run.exited()).not.toBe(0);
      expect(fs.existsSync(s.token)).toBe(false);
    } finally {
      run.child.kill();
    }
  });

  it('kills the CLI process group when the job is cancelled', async () => {
    const s = setup('osc8');
    const run = startDriver(s);
    try {
      await until(() => run.url() !== null && fs.existsSync(s.pidFile));
      const cliPid = Number(fs.readFileSync(s.pidFile, 'utf8'));
      expect(alive(cliPid)).toBe(true);
      run.child.kill();
      await until(() => run.exited() !== undefined && !alive(cliPid), 25_000);
      // The CLI is gone and never wrote a credential, so nothing is imported.
      expect(alive(cliPid)).toBe(false);
      expect(fs.existsSync(s.token)).toBe(false);
    } finally {
      run.child.kill();
    }
  });
});

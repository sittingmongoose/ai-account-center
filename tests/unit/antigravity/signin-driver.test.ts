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

const dirs: string[] = [];
afterEach(() => {
  for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

/**
 * The fake CLI. argv[1] is the token path. It models the real 1.2.16 screens:
 * a raw-mode login-method menu, then the authorization URL (OSC 8 or plain
 * wrapped, per FAKE_URL_MODE), then it reads the pasted code, records what it
 * received and its own pid, and writes the credential envelope.
 * FAKE_EXIT_EARLY makes it quit before writing a token (the "CLI died" path).
 * Otherwise it hangs so the driver has to stop it, like the real CLI at its
 * prompt.
 */
function fakeCli(tokenPath: string): string {
  return `
import os, sys, time, tty
TOKEN = ${JSON.stringify(tokenPath)}
MODE = os.environ.get("FAKE_URL_MODE", "osc8")
URL = ${JSON.stringify(FULL_URL)}
def out(s):
    sys.stdout.write(s); sys.stdout.flush()
tty.setraw(sys.stdin.fileno())
with open(os.environ["FAKE_PID"], "w") as f:
    f.write(str(os.getpid()))
out("\\x1b[?1049h\\x1b[H\\x1b[2J")
out(" Welcome to the Antigravity CLI. You are currently not signed in.\\r\\n\\r\\n")
out(" Select login method:\\r\\n")
out(" \\x1b[1m> 1. Google OAuth\\x1b[m\\r\\n")
out("   2. Use a Google Cloud project\\r\\n\\r\\n")
out("   up/down Navigate - enter Select\\r\\n")
menu_key = sys.stdin.read(1)
with open(os.environ["FAKE_RECEIPT"], "w") as f:
    f.write("menu=" + repr(menu_key) + "\\n")
if os.environ.get("FAKE_EXIT_EARLY"):
    time.sleep(0.2); sys.exit(0)
if MODE == "plain":
    out(" Open the URL below in your browser:\\r\\n")
    for c in [URL[i:i+120] for i in range(0, len(URL), 120)]:
        out(" " + c + "\\x1b[K\\r\\n")
else:
    out(" Open the URL below in your browser:\\r\\n")
    out(" \\x1b[34;4m\\x1b]8;id=xyz;" + URL + "\\x07" + URL[:50] + "\\x1b[m\\x1b]8;;\\x07\\r\\n")
out("\\r\\n After authenticating, copy the code displayed in the browser and paste it below:\\r\\n")
out(" authorization code...\\r\\n")
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

function setup(mode: 'osc8' | 'plain', early = false): Setup {
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

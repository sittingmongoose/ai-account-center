import { spawn, type ChildProcess } from 'child_process';

/**
 * Starts a sign-in CLI with a fixed argv and no shell
 * (CONTRACT-registry-lifecycle section 6.6). Stdio is never inherited: output
 * arrives in memory through `onData` and input goes only through `write`.
 *
 * With `pty: true` the CLI runs on a pseudo-terminal, for tools that need a
 * TTY. Node has no PTY of its own, so a small fixed Python program (the
 * standard `pty` module) allocates one, turns echo off, relays bytes, and
 * forwards SIGTERM to the CLI's own session. The CLI runs in its own session
 * (pty.fork calls setsid), out of reach of a signal to the relay's group, so
 * the relay itself sends the CLI's group SIGKILL one second after the SIGTERM
 * (before this side's own SIGKILL ends the relay). Either way the child is the
 * leader of a new process group, and `kill` signals the whole group.
 */
export interface SignInCommand {
  file: string;
  args: string[];
  /** The complete environment; nothing else is inherited. */
  env: Record<string, string>;
  pty: boolean;
}

export type SignInSpawnFailure = 'missing' | 'failed' | null;

export interface SignInProcessHandle {
  onData(listener: (chunk: string) => void): void;
  onExit(listener: (code: number | null, failure: SignInSpawnFailure) => void): void;
  /** Write to the CLI's input; false when it is no longer open. */
  write(text: string): boolean;
  /** SIGTERM to the process group, then SIGKILL after a short grace period. */
  kill(): void;
}

export type SignInSpawner = (command: SignInCommand) => SignInProcessHandle;

const KILL_GRACE_MS = 2_000;
/** The relay's own grace before it SIGKILLs the CLI; shorter than KILL_GRACE_MS. */
const RELAY_KILL_GRACE_S = 1;
export const PTY_PYTHON = '/usr/bin/python3';

/** Fixed relay program; its argv after `-c <program>` is the CLI argv. */
export const PTY_RELAY_PROGRAM = [
  'import os, pty, select, signal, sys, termios',
  'argv = sys.argv[1:]',
  'pid, fd = pty.fork()',
  'if pid == 0:',
  '    try:',
  '        attrs = termios.tcgetattr(0)',
  '        attrs[3] &= ~termios.ECHO',
  '        termios.tcsetattr(0, termios.TCSANOW, attrs)',
  '    except Exception:',
  '        pass',
  '    try:',
  '        os.execvp(argv[0], argv)',
  '    except Exception:',
  '        os._exit(127)',
  'try:',
  '    import fcntl, struct',
  "    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))",
  'except Exception:',
  '    pass',
  'def force(signum, frame):',
  '    try:',
  '        os.killpg(pid, signal.SIGKILL)',
  '    except Exception:',
  '        pass',
  'stopping = []',
  'def stop(signum, frame):',
  '    try:',
  '        os.killpg(pid, signal.SIGTERM)',
  '    except Exception:',
  '        pass',
  '    if not stopping:',
  '        stopping.append(True)',
  '        signal.signal(signal.SIGALRM, force)',
  `        signal.setitimer(signal.ITIMER_REAL, ${RELAY_KILL_GRACE_S})`,
  'for name in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):',
  '    signal.signal(name, stop)',
  'source = sys.stdin.fileno()',
  'watch = [fd, source]',
  'while True:',
  '    ready, _, _ = select.select(watch, [], [])',
  '    if fd in ready:',
  '        try:',
  '            data = os.read(fd, 4096)',
  '        except OSError:',
  "            data = b''",
  '        if not data:',
  '            break',
  '        os.write(1, data)',
  '    if source in ready:',
  '        data = os.read(source, 4096)',
  '        if data:',
  '            os.write(fd, data)',
  '        else:',
  '            watch.remove(source)',
  '_, status = os.waitpid(pid, 0)',
  'sys.exit(os.waitstatus_to_exitcode(status))',
].join('\n');

function wrap(child: ChildProcess): SignInProcessHandle {
  const dataListeners: Array<(chunk: string) => void> = [];
  const exitListeners: Array<(code: number | null, failure: SignInSpawnFailure) => void> = [];
  let exited = false;
  let exit: [number | null, SignInSpawnFailure] | null = null;
  const finish = (code: number | null, failure: SignInSpawnFailure) => {
    if (exited) return;
    exited = true;
    exit = [code, failure];
    for (const listener of exitListeners) listener(code, failure);
  };
  const forward = (chunk: string) => {
    for (const listener of dataListeners) listener(chunk);
  };
  child.stdout?.setEncoding('utf8');
  child.stderr?.setEncoding('utf8');
  child.stdout?.on('data', forward);
  child.stderr?.on('data', forward);
  child.stdin?.on('error', () => {
    /* A closed input is reported by write() returning false. */
  });
  child.once('error', (error: NodeJS.ErrnoException) => {
    finish(null, error.code === 'ENOENT' ? 'missing' : 'failed');
  });
  child.once('close', (code) => finish(code, null));
  const signal = (name: NodeJS.Signals) => {
    if (!child.pid) return;
    try {
      process.kill(-child.pid, name);
    } catch {
      try {
        child.kill(name);
      } catch {
        /* Already gone. */
      }
    }
  };
  return {
    onData: (listener) => {
      dataListeners.push(listener);
    },
    onExit: (listener) => {
      if (exit) listener(exit[0], exit[1]);
      else exitListeners.push(listener);
    },
    write: (text) => {
      if (exited || !child.stdin || child.stdin.destroyed || !child.stdin.writable) return false;
      child.stdin.write(text);
      return true;
    },
    kill: () => {
      if (exited) return;
      signal('SIGTERM');
      const timer = setTimeout(() => {
        if (!exited) signal('SIGKILL');
      }, KILL_GRACE_MS);
      timer.unref?.();
    },
  };
}

export const spawnSignInProcess: SignInSpawner = (command) => {
  const file = command.pty ? PTY_PYTHON : command.file;
  const args = command.pty
    ? ['-c', PTY_RELAY_PROGRAM, command.file, ...command.args]
    : command.args;
  const child = spawn(file, args, {
    env: { ...command.env },
    stdio: ['pipe', 'pipe', 'pipe'],
    detached: true,
    windowsHide: true,
    shell: false,
  });
  return wrap(child);
};

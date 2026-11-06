import fs from 'fs';
import path from 'path';

/**
 * Process names the resident runtime's own census (runtime_continuity.census_cli)
 * treats as Antigravity: the native CLI, its helpers and the desktop app.
 */
const ANTIGRAVITY_NAMES = new Set([
  'agy',
  'agentapi',
  'antigravity',
  'language_server',
  'language_server_',
  'webm_encoder',
]);
const MAX_PROC_ENTRIES = 1_000_000;

export interface NativeProcessCensus {
  /** False unless every same-user process was read: nobody can then say nothing runs. */
  complete: boolean;
  /** Same-user Antigravity processes seen; any of them makes an update in_use. */
  running: number;
}

const CMDLINE_BYTES = 65_536;
/** Only these mean the process exited between the listing and the read. */
const GONE = new Set(['ENOENT', 'ESRCH']);

class CensusIncomplete extends Error {}
class ProcessGone extends Error {}

function within(child: string, root: string): boolean {
  const relative = path.relative(root, child);
  return relative === '' || (!relative.startsWith('..') && !path.isAbsolute(relative));
}

function code(error: unknown): string {
  return (error as NodeJS.ErrnoException)?.code ?? '';
}

/** A field, or null once the process is gone; any other failure is incomplete. */
function readField<T>(read: () => T, tolerated: ReadonlySet<string> = GONE): T | null {
  try {
    return read();
  } catch (error) {
    if (GONE.has(code(error))) throw new ProcessGone();
    if (tolerated.has(code(error))) return null;
    throw new CensusIncomplete();
  }
}

/** The roots as named and as resolved, so either spelling of a path matches. */
function spellings(file: string): string[] {
  const resolved = path.resolve(file);
  try {
    const real = fs.realpathSync(resolved);
    return real === resolved ? [resolved] : [resolved, real];
  } catch {
    return [resolved];
  }
}

function readCmdline(file: string): string {
  const fd = fs.openSync(file, fs.constants.O_RDONLY);
  try {
    // Only the first arguments decide; a long command line is read in part, not skipped.
    const buffer = Buffer.alloc(CMDLINE_BYTES);
    let total = 0;
    while (total < buffer.length) {
      const count = fs.readSync(fd, buffer, total, buffer.length - total, null);
      if (count <= 0) break;
      total += count;
    }
    return buffer.subarray(0, total).toString('utf8');
  } finally {
    fs.closeSync(fd);
  }
}

/**
 * A read-only same-user census that needs no resident runtime. It is used
 * only when the runtime is stopped because the installed native build has no
 * switching review, so it holds no managed session to preserve. It counts
 * every process that could be Antigravity: a matching process name, the native
 * binary (also once replaced, " (deleted)"), its helper folder, or anything run
 * from the managed runtime folder (launcher shim, foreground launcher, resident).
 * It errs toward "running": a false match only reports in_use.
 *
 * Complete only when every same-user process was read: a read failure other
 * than the process exiting makes it incomplete, and so does a listing that
 * lacks this process itself. One exception: the kernel denies the exe link of
 * a non-dumpable process (systemd --user, ssh-agent, gpg-agent, sshd-session),
 * so EACCES/EPERM there leaves its name and command line to decide.
 */
export function censusAntigravityProcesses(options: {
  home: string;
  nativeBinary: string;
  /** Owned fixture injection only; production reads /proc. */
  procRoot?: string;
  uid?: number;
  selfPid?: number;
}): NativeProcessCensus {
  const incomplete = { complete: false, running: 0 };
  const procRoot = options.procRoot ?? '/proc';
  const uid = options.uid ?? process.getuid?.();
  const self = options.selfPid ?? process.pid;
  if (typeof uid !== 'number' || !Number.isSafeInteger(uid)) return incomplete;
  const home = path.resolve(options.home);
  const natives = spellings(options.nativeBinary);
  // Resolve the home first, then join each root onto every spelling of it, so a
  // missing root folder under a symlinked home still matches its real path.
  const roots = spellings(home).flatMap((base) =>
    ['.gemini/antigravity-cli', '.local/share/ai-account-center/antigravity-runtime'].flatMap(
      (relative) => spellings(path.join(base, relative))
    )
  );
  let entries: string[];
  try {
    entries = fs.readdirSync(procRoot);
  } catch {
    return incomplete;
  }
  if (entries.length > MAX_PROC_ENTRIES || !entries.includes(String(self))) return incomplete;
  const isAntigravityPath = (value: string) => {
    const file = value.endsWith(' (deleted)') ? value.slice(0, -' (deleted)'.length) : value;
    if (!path.isAbsolute(file)) return false;
    const resolved = path.resolve(file);
    return natives.includes(resolved) || roots.some((root) => within(resolved, root));
  };
  const deniedExe = new Set([...GONE, 'EACCES', 'EPERM']);
  let running = 0;
  try {
    for (const name of entries) {
      if (!/^\d{1,10}$/.test(name)) continue;
      const pid = Number(name);
      if (pid <= 1 || pid === self) continue;
      const directory = path.join(procRoot, name);
      try {
        if (readField(() => fs.statSync(directory).uid) !== uid) continue;
        const comm = readField(() => fs.readFileSync(path.join(directory, 'comm'), 'utf8').trim());
        if (comm !== null && ANTIGRAVITY_NAMES.has(comm)) {
          running += 1;
          continue;
        }
        const exe = readField(() => fs.readlinkSync(path.join(directory, 'exe')), deniedExe);
        if (exe !== null && isAntigravityPath(exe)) {
          running += 1;
          continue;
        }
        const cmdline = readField(() => readCmdline(path.join(directory, 'cmdline')));
        if (
          cmdline !== null &&
          cmdline
            .split('\0')
            .filter(Boolean)
            .slice(0, 4)
            .some((argument) => isAntigravityPath(argument))
        )
          running += 1;
      } catch (error) {
        if (error instanceof ProcessGone) continue;
        throw error;
      }
    }
  } catch {
    return incomplete;
  }
  return { complete: true, running };
}

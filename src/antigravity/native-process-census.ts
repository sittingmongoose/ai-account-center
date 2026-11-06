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
  // A child name may itself start with '..' (`..cache`); only a '..' segment leaves.
  return (
    relative === '' ||
    (relative !== '..' && !relative.startsWith('..' + path.sep) && !path.isAbsolute(relative))
  );
}

/** The kernel marks a removed executable or working folder with this suffix. */
function withoutDeleted(value: string): string {
  return value.endsWith(' (deleted)') ? value.slice(0, -' (deleted)'.length) : value;
}

function code(error: unknown): string {
  return (error as NodeJS.ErrnoException)?.code ?? '';
}

/** A field; an exited process throws ProcessGone, any other failure is incomplete. */
function readField<T>(read: () => T): T {
  return readTolerant(read, new Set()) as T;
}

/** As readField, but the tolerated error codes read as null. */
function readTolerant<T>(read: () => T, tolerated: ReadonlySet<string>): T | null {
  try {
    return read();
  } catch (error) {
    if (GONE.has(code(error))) throw new ProcessGone();
    if (tolerated.has(code(error))) return null;
    throw new CensusIncomplete();
  }
}

/** A missing path component, as opposed to one that could not be resolved. */
function missing(file: string): boolean {
  try {
    // A dangling symlink is not missing: where it pointed is unknown.
    fs.lstatSync(file);
    return false;
  } catch (error) {
    return code(error) === 'ENOENT' || code(error) === 'ENOTDIR';
  }
}

/**
 * A path as named and as resolved, so either spelling matches. A missing tail
 * (a root folder that was removed) is rejoined onto its deepest existing
 * ancestor's real path, so a symlink above it still resolves. Null when the
 * path cannot be resolved for any other reason (a dangling symlink, a folder
 * that refuses access): its real spelling is unknown, so the census is incomplete.
 */
function spellings(file: string): string[] | null {
  const resolved = path.resolve(file);
  let existing = resolved;
  const tail: string[] = [];
  for (;;) {
    try {
      const real = path.join(fs.realpathSync(existing), ...tail);
      return real === resolved ? [resolved] : [resolved, real];
    } catch (error) {
      if ((code(error) !== 'ENOENT' && code(error) !== 'ENOTDIR') || !missing(existing))
        return null;
      const parent = path.dirname(existing);
      if (parent === existing) return [resolved];
      tail.unshift(path.basename(existing));
      existing = parent;
    }
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
 * lacks this process itself, or a root or native path that cannot be resolved.
 * One exception: the kernel denies the exe and cwd links of a non-dumpable
 * process (systemd --user, ssh-agent, gpg-agent, sshd-session), so EACCES/EPERM
 * there leaves its name and the rest of its command line to decide.
 *
 * Command-line arguments are the first four; a relative one that names a path
 * (it contains a slash, such as `./bin/helper.js`) is resolved against the
 * process's working folder. A bare relative word (`helper.js`) is not, so a
 * script started by bare name from inside a root folder matches only by its
 * process name or executable. Two further limits are known: `./bin/x` started
 * from a root by a process that then changes folder (`chdir("/")`) resolves
 * against its new folder, and an absolute argument that reaches a root only
 * through a symlink elsewhere is not resolved. Both still match by name or
 * executable when those are Antigravity's.
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
  // Each root is joined onto every spelling of the home and then resolved itself,
  // so a symlinked home, a symlinked ~/.gemini or ~/.local, and a removed root
  // folder under either all still match the real path a helper runs from.
  const homes = spellings(home);
  const roots = homes?.flatMap((base) =>
    ['.gemini/antigravity-cli', '.local/share/ai-account-center/antigravity-runtime'].map(
      (relative) => spellings(path.join(base, relative))
    )
  );
  if (!natives || !roots || roots.some((root) => root === null)) return incomplete;
  const rootPaths = roots.flat() as string[];
  let entries: string[];
  try {
    entries = fs.readdirSync(procRoot);
  } catch {
    return incomplete;
  }
  if (entries.length > MAX_PROC_ENTRIES || !entries.includes(String(self))) return incomplete;
  const isAntigravityPath = (value: string) => {
    const file = withoutDeleted(value);
    if (!path.isAbsolute(file)) return false;
    const resolved = path.resolve(file);
    return natives.includes(resolved) || rootPaths.some((root) => within(resolved, root));
  };
  const deniedExe = new Set(['EACCES', 'EPERM']);
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
        if (ANTIGRAVITY_NAMES.has(comm)) {
          running += 1;
          continue;
        }
        const exe = readTolerant(() => fs.readlinkSync(path.join(directory, 'exe')), deniedExe);
        if (exe !== null && isAntigravityPath(exe)) {
          running += 1;
          continue;
        }
        const cmdline = readField(() => readCmdline(path.join(directory, 'cmdline')));
        const args = cmdline.split('\0').filter(Boolean).slice(0, 4);
        let cwd: string | null | undefined;
        const resolveArgument = (argument: string): string | null => {
          if (path.isAbsolute(argument)) return argument;
          if (!argument.includes('/')) return null;
          if (cwd === undefined) {
            const link = readTolerant(
              () => fs.readlinkSync(path.join(directory, 'cwd')),
              deniedExe
            );
            // A working folder that was removed reads as "<folder> (deleted)".
            cwd = link === null ? null : withoutDeleted(link);
          }
          return cwd === null || !path.isAbsolute(cwd) ? null : path.resolve(cwd, argument);
        };
        if (
          args.some((argument) => {
            const file = resolveArgument(argument);
            return file !== null && isAntigravityPath(file);
          })
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

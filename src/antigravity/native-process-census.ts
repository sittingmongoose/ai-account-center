import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

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

const DELETED = ' (deleted)';

/**
 * The kernel marks a removed executable or working folder with " (deleted)",
 * but a live folder may really be named that way. Both readings are tried; a
 * false match only reports in_use.
 */
function pathSpellings(value: string): string[] {
  return value.endsWith(DELETED) ? [value, value.slice(0, -DELETED.length)] : [value];
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

/**
 * Filesystems where a lookup can block with no timeout: network ones, every FUSE one (most are), and autofs, whose
 * mount point mounts its share on first touch (a systemd automount like /mnt/Cursor reads as autofs until then).
 */
const NETWORK_FS =
  /^(?:nfs4?|cifs|smb3?|smbfs|ceph|glusterfs|lustre|gpfs|afs|9p|virtiofs|davfs|autofs|beegfs|ocfs2|gfs2|orangefs|pvfs2|vboxsf|fuse.*)$/;
/** How many path lookups one process's arguments may cost the census. */
const LOOKUPS_PER_PROCESS = 4096;

/** Mount points of such filesystems, from a mount table (`/proc/self/mountinfo`); none when it can't be read. */
function networkMounts(file: string): string[] {
  let text: string;
  try {
    text = fs.readFileSync(file, 'utf8');
  } catch {
    return [];
  }
  return text.split('\n').flatMap((line) => {
    const [left, right] = line.split(' - ');
    if (!right || !NETWORK_FS.test(right.split(' ')[0] ?? '')) return [];
    const mount = left.split(' ')[4];
    // the root folder itself is never skipped (a virtiofs / would turn off every lookup)
    if (mount === '/') return [];
    // the table writes a space and other bytes as octal escapes (\040)
    return mount
      ? [
          mount.replace(/\\([0-7]{3})/g, (_, octal: string) =>
            String.fromCharCode(parseInt(octal, 8))
          ),
        ]
      : [];
  });
}

/** lstat results shared by one census's lookups, so repeated parent folders cost one call. */
type StatCache = Map<string, fs.Stats | string>;

function cachedLstat(file: string, cache: StatCache): fs.Stats | string {
  let hit = cache.get(file);
  if (hit === undefined) {
    try {
      hit = fs.lstatSync(file);
    } catch (error) {
      hit = code(error) || 'EUNKNOWN';
    }
    cache.set(file, hit);
  }
  return hit;
}

/**
 * Where a path really leads, resolved one component at a time as the kernel does: `.` and `..` apply to the folder
 * reached so far, after links (`/tmp/l/../h.py` with `/tmp/l -> root/sub` is `root/h.py`). Nothing on or under a mount
 * in `network` is touched: the path from there on is kept as written. A missing tail is kept (`/tmp/link/gone.py`
 * through `/tmp/link -> root` is `root/gone.py`), and so is a dangling link's target. A loop or any other failure is
 * null.
 */
function resolveLocally(file: string, network: readonly string[], cache: StatCache): string | null {
  let pending = file.split(path.sep).filter(Boolean);
  let current: string = path.sep;
  for (let hops = 0; pending.length > 0; ) {
    const [part, ...rest] = pending;
    if (part === '.') {
      pending = rest;
      continue;
    }
    if (part === '..') {
      current = path.dirname(current);
      pending = rest;
      continue;
    }
    const next = path.join(current, part);
    if (network.some((mount) => within(next, mount))) return path.join(next, ...rest);
    const stat = cachedLstat(next, cache);
    if (typeof stat === 'string')
      return stat === 'ENOENT' || stat === 'ENOTDIR' ? path.join(current, ...pending) : null;
    if (!stat.isSymbolicLink()) {
      current = next;
      pending = rest;
      continue;
    }
    hops += 1;
    if (hops > 40) return null;
    let target: string;
    try {
      target = fs.readlinkSync(next);
    } catch {
      return null;
    }
    // a relative target is read from the link's own folder
    if (path.isAbsolute(target)) current = path.sep;
    pending = [...target.split(path.sep).filter(Boolean), ...rest];
  }
  return current;
}

function readCmdline(file: string): { text: string; bytes: number } {
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
    return { text: buffer.subarray(0, total).toString('utf8'), bytes: total };
  } finally {
    fs.closeSync(fd);
  }
}

/**
 * The script paths one interpreter argument may name: the argument itself, an option's value (`--require=x`,
 * `-r/abs/x`), an assignment's value (`ROOT=/x`), each part of a `:` or `,` list (`--path=/a:/b`), and a `file://`
 * URL as its path. A plain flag (`-u`, `--inspect`) names none.
 */
function candidateScripts(argument: string): string[] {
  let value: string;
  const equals = argument.indexOf('=');
  if (argument.startsWith('-')) {
    if (equals > 0) value = argument.slice(equals + 1);
    else if (/^-[A-Za-z][./~]/.test(argument)) value = argument.slice(2);
    else return [];
  } else {
    value =
      equals > 0 && /^[A-Za-z_][A-Za-z0-9_]*$/.test(argument.slice(0, equals))
        ? argument.slice(equals + 1)
        : argument;
  }
  // Each part of a `,` or `:` list, and every `file:` URL anywhere in it as its decoded path, any case of the scheme
  // (`FILE:///x/%2Egemini/h.mjs` is `/x/.gemini/h.mjs`; `/a:file:///b` gives `/a` and `/b`).
  const parts = value
    .split(',')
    .flatMap((item) => [...item.split(':'), ...fileUrlPaths(item)])
    .filter(Boolean);
  return argument.startsWith('-')
    ? parts
    : [argument, ...parts.filter((part) => part !== argument)];
}

/**
 * Every `file:` URL in a list item as a decoded path. Several URLs may share an item (`file:///a:file:///b`), and a URL
 * may be followed by `:more` (`file:///x/root:/b`), so each URL is also read up to each of its first colons.
 */
function fileUrlPaths(item: string): string[] {
  return item
    .split(/(?=file:)/i)
    .filter((segment) => /^file:/i.test(segment))
    .flatMap((segment) => {
      const cuts = [segment];
      for (
        let at = segment.indexOf(':', 5);
        at !== -1 && cuts.length < 16;
        at = segment.indexOf(':', at + 1)
      )
        cuts.push(segment.slice(0, at));
      return cuts.map(filePath);
    });
}

/** A `file:` URL as its path, percent-escapes decoded; a URL that doesn't parse keeps its text after the scheme. */
function filePath(url: string): string {
  try {
    return fileURLToPath(url);
  } catch {
    return url.replace(/^file:(?:\/\/)?/i, '');
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
 * process's working folder. A bare relative word (`helper.js`) is not, except
 * as an interpreter's script (below), so another program started by bare name
 * from inside a root folder matches only by its process name or executable.
 * One further limit is known: `./bin/x` started
 * from a root by a process that then changes folder (`chdir("/")`) resolves
 * against its new folder (it still matches by name or executable when those
 * are Antigravity's). A path that reaches a root through a symlink elsewhere
 * matches by where it really leads (a missing script by the folder that
 * exists, a dangling link by its target), resolved one component at a time and
 * never on or under a network, FUSE or autofs mount, where nothing is looked
 * up; at most 256 lookups per process, past which an interpreter's script is
 * unplaced.
 *
 * An interpreter is known by its process name, argv0 or executable (a
 * rewritten title still runs /usr/bin/python3). For one, every non-option
 * argument and every option or assignment value in the part of the command
 * line read is a candidate script (`python3 helper.py`, `-r/x/hook.js`,
 * `--import=file:///x`, `--path=/a:/b`, `ROOT=/x`), resolved against its
 * working folder; an interpreter whose working folder is inside a root counts
 * whatever its arguments (`python3 -m mod`, a script on stdin), except a shell
 * with no arguments; one whose command line was cut at the read limit leaves
 * idle unproven.
 *
 * A non-dumpable process hides its working folder, so a relative script can't
 * be placed. When such a process is an interpreter (python, node, sh, perl,
 * lua, php and the like, by process name or argv0) with a candidate script, it
 * may be a live helper, so the census is incomplete rather than idle. Other non-dumpable processes (systemd --user, ssh-agent,
 * gpg-agent, sshd-session, sandboxed browser renderers whose
 * `--flag=/path` arguments read like relative paths) are not judged by a
 * script path. An interpreter under a renamed binary is a known limit.
 */
export function censusAntigravityProcesses(options: {
  home: string;
  nativeBinary: string;
  /** Owned fixture injection only; production reads /proc. */
  procRoot?: string;
  /** Owned fixture injection only; production reads /proc/self/mountinfo. */
  mountInfo?: string;
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
  const isAntigravityPath = (value: string) =>
    pathSpellings(value).some((file) => {
      if (!path.isAbsolute(file)) return false;
      const resolved = path.resolve(file);
      return natives.includes(resolved) || rootPaths.some((root) => within(resolved, root));
    });
  // A path as written, or where it really leads: a script reached through a symlink elsewhere (`/tmp/link/h.py` with
  // /tmp/link -> a root) is a root's script. A path that doesn't exist or can't be resolved counts as written only.
  // Lookups never touch a network mount (a hung NFS share would block an lstat with no timeout); a deleted script
  // reached through a symlinked folder still resolves, by the folder that exists.
  const network = networkMounts(options.mountInfo ?? '/proc/self/mountinfo');
  // A process's paths are looked up at most LOOKUPS_PER_PROCESS times (a hostile command line can name thousands);
  // past that an interpreter's script is unplaced.
  let lookups = 0;
  let overLookups = false;
  const stats: StatCache = new Map();
  const resolved = new Map<string, string | null>();
  // `lookup: false` for a bare word: it can only lead into the working folder, which is judged on its own (an
  // interpreter working in a root counts), so it needs no lookup at all and a long inline script costs none.
  const reachesRoot = (value: string, lookup = true): boolean => {
    if (isAntigravityPath(value)) return true;
    if (!lookup) return false;
    let real = resolved.get(value);
    if (real === undefined) {
      if (lookups >= LOOKUPS_PER_PROCESS) {
        overLookups = true;
        return false;
      }
      lookups += 1;
      real = resolveLocally(value, network, stats);
      resolved.set(value, real);
    }
    return real !== null && isAntigravityPath(real);
  };
  const deniedExe = new Set(['EACCES', 'EPERM']);
  // An interpreter running a script by a relative path (`python3 ./bin/helper.py`) is judged by its working folder. A
  // non-dumpable one denies that link, and then the script can't be placed: such a process may be a live helper, so
  // the census can't call the host idle. Only interpreters count here: other non-dumpable processes (systemd --user,
  // ssh-agent, sshd-session, whose rewritten title "sshd-session: user@pts/0" reads like a relative path) are not
  // judged by a script path at all.
  const INTERPRETER =
    /^(?:python[\d.]*|pypy[\d.]*|node|nodejs|bun|deno|sh|bash|dash|zsh|ksh|mksh|fish|perl[\d.]*|ruby[\d.]*|lua[\d.]*|luajit|php[\d.]*|tclsh[\d.]*|Rscript|julia)$/;
  const SHELL = /^(?:sh|bash|dash|zsh|ksh|mksh|fish)$/;
  let unplaced = false;
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
        lookups = 0;
        overLookups = false;
        const read = readField(() => readCmdline(path.join(directory, 'cmdline')));
        const cmdline = read.text;
        const all = cmdline.split('\0').filter(Boolean);
        const args = all.slice(0, 4);
        // An interpreter's script can follow any number of options and may be a bare name (`python3 -I -u helper.py`,
        // `node helper.js`) or an option's value (`node --require=./hook.js app.js`): for one, every non-option
        // argument and every `--opt=value` value in the part of the command line read is a candidate script, resolved
        // against the working folder like a relative path.
        // By process name, argv0 or the executable itself (a rewritten title still runs /usr/bin/python3).
        const named = (word: string) => INTERPRETER.test(word);
        const interpreter =
          named(comm) ||
          named(path.basename(all[0] ?? '')) ||
          (exe !== null && named(path.basename(exe)));
        const shell = SHELL.test(comm) || SHELL.test(path.basename(all[0] ?? ''));
        const operands = interpreter ? [...new Set(all.slice(1).flatMap(candidateScripts))] : [];
        let cwds: string[] | undefined;
        let cwdDenied = false;
        const resolveArgument = (argument: string, bare = false): string[] => {
          if (path.isAbsolute(argument)) return [argument];
          if (!bare && !argument.includes('/')) return [];
          if (cwds === undefined) {
            const link = readTolerant(
              () => fs.readlinkSync(path.join(directory, 'cwd')),
              deniedExe
            );
            // A removed working folder reads as "<folder> (deleted)"; a live one
            // may really carry that name, so both readings are kept.
            cwds = link === null ? [] : pathSpellings(link).filter((cwd) => path.isAbsolute(cwd));
            if (link === null) cwdDenied = true;
          }
          // joined as written, so `..` is applied after links (path.resolve would apply it first)
          return cwds.map((cwd) => `${cwd}${path.sep}${argument}`);
        };
        // An interpreter working inside a root runs Antigravity's code whatever its arguments say (`python3 -m mod`,
        // `python3 -c ...`, a script on stdin): it counts too. Only one with arguments: a bare interpreter name (an
        // empty or reaped command line) is no evidence either way.
        const inRoot = (): boolean => {
          resolveArgument('./x', true);
          return (cwds ?? []).some((cwd) => reachesRoot(cwd));
        };
        if (
          args.some((argument) => resolveArgument(argument).some((file) => reachesRoot(file))) ||
          operands.some((argument) =>
            resolveArgument(argument, true).some((file) =>
              reachesRoot(file, path.isAbsolute(argument) || argument.includes(path.sep))
            )
          ) ||
          // a shell needs arguments (an interactive one parked in a root folder is no helper); another interpreter
          // with none reads its script from stdin
          (interpreter && (all.length > 1 || (all.length === 1 && !shell)) && inRoot())
        ) {
          running += 1;
          continue;
        }
        // Idle stays unproven for an interpreter whose script couldn't be placed: its working folder is hidden while it
        // has a candidate script, or the command line was cut at the read limit with no candidate found.
        const truncated = read.bytes >= CMDLINE_BYTES || overLookups;
        if (interpreter && ((cwdDenied && operands.length > 0) || truncated)) unplaced = true;
      } catch (error) {
        if (error instanceof ProcessGone) continue;
        throw error;
      }
    }
  } catch {
    return incomplete;
  }
  // A counted helper already proves the host busy; an interpreter whose script couldn't be placed leaves idle unproven.
  if (unplaced && running === 0) return incomplete;
  return { complete: true, running };
}

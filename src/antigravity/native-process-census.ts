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

/**
 * Filesystems where a lookup can block with no timeout: network ones, every FUSE one (most are), and autofs, whose
 * mount point mounts its share on first touch (a systemd automount like /mnt/Cursor reads as autofs until then).
 */
const NETWORK_FS =
  /^(?:nfs4?|cifs|smb3?|smbfs|ceph|glusterfs|lustre|gpfs|afs|9p|virtiofs|davfs|autofs|beegfs|ocfs2|gfs2|orangefs|pvfs2|vboxsf|fuse.*)$/;
/** How many path components one process's lookups may walk in all; past it the process's script is unplaced. */
const STEPS_PER_PROCESS = 65_536;
/** A lookup that ran out of the process's step budget. */
const OVER_BUDGET = Symbol('over budget');
/** A lookup that couldn't be finished (a denied folder, an unreadable link): where the path leads is unknown. */
const UNRESOLVED = Symbol('unresolved');
/** lstat failures that mean the kernel couldn't open the path either, so no script runs from it. */
const KERNEL_CANT_OPEN = new Set([
  'ENOENT',
  'ENOTDIR',
  'ENAMETOOLONG',
  'EINVAL',
  'ERR_INVALID_ARG_VALUE',
]);
/** How many path lookups one process's arguments may cost the census. */
const LOOKUPS_PER_PROCESS = 4096;

/** Mount points of such filesystems, from a mount table (`/proc/self/mountinfo`); null when it can't be read. */
function networkMounts(file: string): string[] | null {
  let text: string;
  try {
    text = fs.readFileSync(file, 'utf8');
  } catch {
    return null;
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

/** A file an interpreter runs, by its extension. */
const SCRIPT_FILE =
  /\.(?:[cm]?[jt]sx?|py[cwz]?|pl|pm|rb|php\d?|phar|lua|r|jl|sh|bash|zsh|ksh|fish|tcl|ps1)$/i;

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
 * through `/tmp/link -> root` is `root/gone.py`), and so is a dangling link's target. What the kernel couldn't open
 * either (a missing name, one too long, a NUL byte, a chain past 40 links) is kept as written. Any other failure is
 * UNRESOLVED, a denied folder too, even in a prompt that only mentions one (`node app.js "see /root/x and y"`): a path
 * the owner's own processes can't reach may still be a helper's script under other credentials, and no reading of a
 * command line can tell a prompt from a script whose name has spaces. That prompt holds the update while its process
 * runs, which fails safe.
 */
function resolveLocally(
  file: string,
  network: readonly string[],
  cache: StatCache,
  budget: { steps: number },
  report?: { vanished: boolean }
): string | typeof UNRESOLVED | typeof OVER_BUDGET {
  // a NUL can't be in a path the kernel opens: nothing runs from it
  if (file.includes('\0')) return file;
  // components still to walk, last first, so each step is O(1)
  const stack = file.split(path.sep).filter(Boolean).reverse();
  let current: string = path.sep;
  let hops = 0;
  for (let part = stack.pop(); part !== undefined; part = stack.pop()) {
    // every step counts against the process's budget, so no command line can stall the census
    budget.steps -= 1;
    if (budget.steps < 0) return OVER_BUDGET;
    if (part === '.') continue;
    if (part === '..') {
      current = path.dirname(current);
      continue;
    }
    const next = path.join(current, part);
    const mount = network.find((root) => within(next, root));
    if (mount !== undefined) {
      // Nothing on the mount is touched: the rest is read as written until a `..` leaves the mount, and walking
      // resumes from there with the components not yet read (a resume is no link, so it costs no hop).
      let dir = next;
      let left = false;
      for (let rest = stack.pop(); rest !== undefined; rest = stack.pop()) {
        budget.steps -= 1;
        if (budget.steps < 0) return OVER_BUDGET;
        if (rest === '.') continue;
        if (rest === '..') {
          if (dir === mount) {
            current = path.dirname(mount);
            left = true;
            break;
          }
          dir = path.dirname(dir);
          continue;
        }
        dir = path.join(dir, rest);
      }
      if (!left) return dir;
      continue;
    }
    const stat = cachedLstat(next, cache);
    if (typeof stat === 'string') {
      // missing, not a folder, or a name the kernel can't open either (a long prompt's "and/or" reads as a path):
      // the rest is kept as written
      // A missing path that names a script file (its last part has a script's extension) is a script that ran and was
      // removed since, or whose link was (`python3 h.py`, then `h.py` deleted): reported, so an interpreter's script
      // can't be placed. A prompt that only mentions a missing path (`see /x and/or y`) names no script file.
      if (report && (stat === 'ENOENT' || stat === 'ENOTDIR') && SCRIPT_FILE.test(stack[0] ?? part))
        report.vanished = true;
      return KERNEL_CANT_OPEN.has(stat)
        ? path.join(current, part, ...[...stack].reverse())
        : UNRESOLVED;
    }
    if (!stat.isSymbolicLink()) {
      current = next;
      continue;
    }
    hops += 1;
    // past the kernel's own limit (ELOOP): nothing can be opened through this path
    if (hops > 40) return path.join(next, ...[...stack].reverse());
    let target: string;
    try {
      target = fs.readlinkSync(next);
    } catch {
      return UNRESOLVED;
    }
    // a relative target is read from the link's own folder
    if (path.isAbsolute(target)) current = path.sep;
    stack.push(...target.split(path.sep).filter(Boolean).reverse());
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
function candidateScripts(argument: string, operand = false): string[] {
  let value: string;
  const equals = argument.indexOf('=');
  if (!operand && argument.startsWith('-')) {
    if (equals > 0) value = argument.slice(equals + 1);
    else if (/^-[A-Za-z][^-]/.test(argument)) {
      // a short option's glued value (`php -fh.py`, `ruby -rh.py`, a bare name looked up only if it is a link there),
      // possibly after a cluster of flags (`perl -wI/x/lib`): each place a value could start, up to the first
      // character that is no letter
      const starts: string[] = [];
      for (let at = 2; at < argument.length && at <= 18; at += 1) {
        starts.push(argument.slice(at));
        if (!/[A-Za-z]/.test(argument[at])) break;
      }
      return [...new Set(starts.flatMap((start) => [start, ...listParts(start)]))];
    } else return [];
  } else {
    value =
      equals > 0 && /^[A-Za-z_][A-Za-z0-9_]*$/.test(argument.slice(0, equals))
        ? argument.slice(equals + 1)
        : argument;
  }
  // the whole value too, when splitting it could lose a path that holds a comma or a colon (never a URL's text)
  const whole = /[,:]/.test(value) && !/^[A-Za-z][A-Za-z0-9+.-]*:/.test(value) ? [value] : [];
  const parts = [...whole, ...listParts(value)];
  if (operand || !argument.startsWith('-')) parts.unshift(argument);
  return [...new Set(parts.filter(Boolean))];
}

/**
 * Each part of a `,` or `:` list, and every `file:` URL anywhere in it as its decoded path, any case of the scheme
 * (`FILE:///x/%2Egemini/h.mjs` is `/x/.gemini/h.mjs`; `/a:file:///b` gives `/a` and `/b`). The whole value is kept
 * beside them by the caller, since a path may itself hold a comma or a colon.
 */
function listParts(value: string): string[] {
  return value
    .split(',')
    .flatMap((item) => [...item.split(':'), ...fileUrlPaths(item)])
    .filter(Boolean);
}

/**
 * A script as written in an argument, not a fragment of one: the argument itself, an assignment's value, or an
 * option's whole value (`--require=./h.js`, `-r./h.js`). Only such a script, when missing, reads as vanished.
 */
function writtenScripts(argument: string, operand = false): string[] {
  const equals = argument.indexOf('=');
  if (operand || !argument.startsWith('-'))
    return equals > 0 && /^[A-Za-z_][A-Za-z0-9_]*$/.test(argument.slice(0, equals))
      ? [argument, argument.slice(equals + 1)]
      : [argument];
  if (equals > 0) return [argument.slice(equals + 1)];
  return /^-[A-Za-z][^-]/.test(argument) ? [argument.slice(2)] : [];
}

/** The scripts written whole in a command line (see writtenScripts), with `--` handled as for candidates. */
function commandLineWritten(argv: readonly string[]): Set<string> {
  const end = argv.indexOf('--');
  return new Set(
    argv.flatMap((argument, at) =>
      end !== -1 && at > end
        ? writtenScripts(argument, true)
        : at === end
          ? []
          : writtenScripts(argument)
    )
  );
}

/** Every argument's candidate scripts; after `--` each argument is an operand as written (`python3 -- --x/h.py`). */
function commandLineCandidates(argv: readonly string[]): string[] {
  const end = argv.indexOf('--');
  return [
    ...new Set(
      argv.flatMap((argument, at) =>
        end !== -1 && at > end
          ? candidateScripts(argument, true)
          : at === end
            ? []
            : candidateScripts(argument)
      )
    ),
  ];
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
 * up (a link that leads onto such a mount is read as written from there: a link
 * on the share pointing back into a root is a known limit, since following it
 * would block on a hung share and `~/.local/bin` links into the share are
 * common); a denied folder or an unreadable link leaves an interpreter
 * unplaced, even when only a prompt names the folder (that process holds the
 * update while it runs, which fails safe); so does a script written in an
 * argument that names a script file and is missing (it ran and was removed, or
 * its link was), an interpreter whose working folder is hidden even with no
 * script named (it may read one from stdin), and a process whose name or
 * executable keeps changing while it is read (it is read again twice first, so
 * an exec mid-read is judged by its new image); at most 4096 lookups per
 * process, past which an interpreter's script is unplaced. A folder is listed
 * for links only where it really is, never on a mount.
 *
 * An interpreter is known by its process name, argv0 or executable (a
 * rewritten title still runs /usr/bin/python3). For one, every non-option
 * argument and every option or assignment value in the part of the command
 * line read is a candidate script, and after `--` every argument as written
 * (`python3 helper.py`, `-r/x/hook.js`, `python3 -- --x/h.py`,
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
  // read first: resolving the home, the roots and the native never touches a network or automount either
  // without the mount table no lookup can be kept off a hung share: nothing is proven
  const network = networkMounts(options.mountInfo ?? '/proc/self/mountinfo');
  if (network === null) return incomplete;
  const stats: StatCache = new Map();
  /** A path as written and where it really leads (a missing tail kept); null when that can't be known. */
  const spellings = (file: string): string[] | null => {
    const resolved = path.resolve(file);
    const real = resolveLocally(resolved, network, stats, { steps: STEPS_PER_PROCESS });
    if (typeof real !== 'string') return null;
    return real === resolved ? [resolved] : [resolved, real];
  };
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
  // A process's paths are looked up at most LOOKUPS_PER_PROCESS times (a hostile command line can name thousands);
  // past that an interpreter's script is unplaced.
  let lookups = 0;
  let overLookups = false;
  let unresolvedSeen = false;
  const steps = { steps: STEPS_PER_PROCESS };
  const resolved = new Map<string, { real: string | typeof UNRESOLVED; vanished: boolean }>();
  // A bare word leads into the working folder, which is judged on its own (an interpreter working in a root counts),
  // unless it is a link there: so a bare word is looked up only when the folder holds a link by that name, read once
  // per folder (never on a network mount), and a long inline script costs no lookups.
  // null: the folder can't be listed (search-only, `--x`), though its links can still be followed: look every name up
  const linksIn = new Map<string, Set<string> | null>();
  const isLinkIn = (folder: string, name: string): boolean => {
    let links = linksIn.get(folder);
    if (links === undefined) {
      links = new Set();
      // listed only where it really is, and never on a mount (a removed cwd's "(deleted)" spelling may be a link)
      const real = resolveLocally(folder, network, stats, steps);
      if (typeof real !== 'string' || network.some((root) => within(real, root))) links = null;
      else
        try {
          for (const entry of fs.readdirSync(real, { withFileTypes: true }))
            if (entry.isSymbolicLink()) links.add(entry.name);
        } catch {
          links = null;
        }
      linksIn.set(folder, links);
    }
    return links === null || links.has(name);
  };
  /** `script`: an interpreter's candidate script, which a vanished script file leaves unplaced. */
  const reachesRoot = (value: string, lookup = true, script = false): boolean => {
    if (isAntigravityPath(value)) return true;
    if (!lookup) return false;
    let found = resolved.get(value);
    if (found === undefined) {
      if (lookups >= LOOKUPS_PER_PROCESS) {
        overLookups = true;
        return false;
      }
      lookups += 1;
      const report = { vanished: false };
      const real = resolveLocally(value, network, stats, steps, report);
      if (real === OVER_BUDGET) {
        overLookups = true;
        return false;
      }
      found = { real, vanished: report.vanished };
      resolved.set(value, found);
    }
    // where it leads is unknown (a denied folder, a link loop): an interpreter's script can't be placed
    if (found.real === UNRESOLVED) {
      unresolvedSeen = true;
      return false;
    }
    if (isAntigravityPath(found.real)) return true;
    if (script && found.vanished) unresolvedSeen = true;
    return false;
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
    // a process that execs while it is read is read again, twice at most, before it is called unplaced
    const queue = [...entries];
    const reread = new Map<string, number>();
    for (let index = 0; index < queue.length; index += 1) {
      const name = queue[index];
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
        unresolvedSeen = false;
        steps.steps = STEPS_PER_PROCESS;
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
        const operands = interpreter ? commandLineCandidates(all.slice(1)) : [];
        const written = interpreter ? commandLineWritten(all.slice(1)) : new Set<string>();
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
              reachesRoot(
                file,
                path.isAbsolute(argument) ||
                  argument.includes(path.sep) ||
                  SCRIPT_FILE.test(argument) ||
                  isLinkIn(path.dirname(file), path.basename(file)),
                written.has(argument)
              )
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
        // a process that changed its name or executable while it was read (an exec) is judged by mixed readings
        const commAfter = readField(() =>
          fs.readFileSync(path.join(directory, 'comm'), 'utf8').trim()
        );
        const exeAfter = readTolerant(
          () => fs.readlinkSync(path.join(directory, 'exe')),
          deniedExe
        );
        if (commAfter !== comm || exeAfter !== exe) {
          const times = reread.get(name) ?? 0;
          if (times < 2) {
            reread.set(name, times + 1);
            queue.push(name);
          } else unplaced = true;
        } else if (interpreter && (cwdDenied || truncated || unresolvedSeen)) unplaced = true;
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

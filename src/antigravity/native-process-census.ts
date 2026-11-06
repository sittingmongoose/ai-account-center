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
  /** False when /proc could not be listed: nobody can say nothing runs. */
  complete: boolean;
  /** Same-user Antigravity processes seen; any of them makes an update in_use. */
  running: number;
}

function within(child: string, root: string): boolean {
  const relative = path.relative(root, child);
  return relative === '' || (!relative.startsWith('..') && !path.isAbsolute(relative));
}

function readOptional(read: () => string): string | null {
  try {
    return read();
  } catch {
    // The process exited, or the kernel hides this field: the other signals decide.
    return null;
  }
}

/**
 * A read-only same-user census that needs no resident runtime. It is used
 * only when the runtime is stopped because the installed native build has no
 * switching review, so it holds no managed session to preserve. It counts
 * every process that could be Antigravity: a matching process name, the native
 * binary (also once replaced, " (deleted)"), its helper folder, or anything run
 * from the managed runtime (launcher shim, foreground launcher, resident).
 * It errs toward "running": a false match only reports in_use.
 */
export function censusAntigravityProcesses(options: {
  home: string;
  nativeBinary: string;
  /** Owned fixture injection only; production reads /proc. */
  procRoot?: string;
  uid?: number;
  selfPid?: number;
}): NativeProcessCensus {
  const procRoot = options.procRoot ?? '/proc';
  const uid = options.uid ?? process.getuid?.();
  const self = options.selfPid ?? process.pid;
  if (typeof uid !== 'number' || !Number.isSafeInteger(uid)) return { complete: false, running: 0 };
  const home = path.resolve(options.home);
  const native = path.resolve(options.nativeBinary);
  const nativeReal = readOptional(() => fs.realpathSync(native)) ?? native;
  const helpers = path.join(home, '.gemini/antigravity-cli');
  const runtime = path.join(home, '.local/share/ai-account-center/antigravity-runtime');
  let entries: string[];
  try {
    entries = fs.readdirSync(procRoot);
  } catch {
    return { complete: false, running: 0 };
  }
  if (entries.length > MAX_PROC_ENTRIES) return { complete: false, running: 0 };
  const isAntigravityPath = (value: string) => {
    const file = value.endsWith(' (deleted)') ? value.slice(0, -' (deleted)'.length) : value;
    if (!path.isAbsolute(file)) return false;
    const resolved = path.resolve(file);
    return (
      resolved === native ||
      resolved === nativeReal ||
      within(resolved, helpers) ||
      within(resolved, runtime)
    );
  };
  let running = 0;
  for (const name of entries) {
    if (!/^\d{1,10}$/.test(name)) continue;
    const pid = Number(name);
    if (pid <= 1 || pid === self) continue;
    const directory = path.join(procRoot, name);
    let owner: number;
    try {
      owner = fs.statSync(directory).uid;
    } catch {
      continue; // Exited between the listing and this read.
    }
    if (owner !== uid) continue;
    const comm = readOptional(() => fs.readFileSync(path.join(directory, 'comm'), 'utf8').trim());
    if (comm !== null && ANTIGRAVITY_NAMES.has(comm)) {
      running += 1;
      continue;
    }
    const exe = readOptional(() => fs.readlinkSync(path.join(directory, 'exe')));
    if (exe !== null && isAntigravityPath(exe)) {
      running += 1;
      continue;
    }
    const cmdline = readOptional(() => {
      const raw = fs.readFileSync(path.join(directory, 'cmdline'));
      return raw.length > 65_536 ? '' : raw.toString('utf8');
    });
    if (
      cmdline !== null &&
      cmdline
        .split('\0')
        .filter(Boolean)
        .slice(0, 4)
        .some((argument) => isAntigravityPath(argument))
    )
      running += 1;
  }
  return { complete: true, running };
}

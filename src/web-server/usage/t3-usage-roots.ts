/**
 * T3 Code account homes: the Claude Code config dirs and Codex homes that T3 Code instances use,
 * outside the default roots the collectors read.
 *
 * - Claude: one CLAUDE_CONFIG_DIR per account (`~/.claude-t3/<account>`), transcripts under
 *   `projects/`. Claude Code writes a session only into the config dir it runs with, so these are
 *   new usage; a copy that also sits in a default root still counts once (the request reads them
 *   in keyed mode against every default Claude root, collectExperimentActivity).
 * - Codex: per-account shadow homes (`~/.codex-t3/<account>`, and T3's managed ones under
 *   `~/.t3/userdata/providers/codex/<instance>/shadow`) keep their own login while every other
 *   entry, `sessions/` included, is a symlink back into `~/.codex`. Such a root resolves into the
 *   default root and is left out; a real folder is read, each real file once.
 * - Both: the homes T3's own settings name (`providerInstances.<id>.config.homePath`, and
 *   `shadowHomePath` for Codex), so a home outside these folders is found too.
 *
 * Any folder name counts (T3 lets the user name an instance's home), bounded to T3_MAX_HOMES per
 * base. Symlinks are followed only inside these roots, and only to a target that stays inside the
 * same root (filesUnderT3Root), with a loop guard and real-path dedupe; the experiment walk and the
 * OMP marker scan never follow them.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

export interface T3UsageRoots {
  /** `projects` folders of T3 Claude config dirs. */
  claude: string[];
  /** `sessions` folders of T3 Codex homes and shadow homes. */
  codex: string[];
}

/** Account folders read per base folder (`~/.claude-t3`, `~/.codex-t3`, T3's managed homes). */
export const T3_MAX_HOMES = 64;
const MAX_SETTINGS_BYTES = 1024 * 1024;
const MAX_PATH_LENGTH = 1024;

/** T3 Code's state folder (`<stateDir>` in its sources). */
export function t3StateDir(homeDir: string): string {
  return path.join(homeDir, '.t3', 'userdata');
}

/** Sub-folders of `base` (a symlink to a folder counts), in name order, at most T3_MAX_HOMES. */
function childDirs(base: string): string[] {
  let names: string[];
  try {
    names = fs.readdirSync(base).sort();
  } catch {
    return [];
  }
  const result: string[] = [];
  for (const name of names) {
    if (result.length >= T3_MAX_HOMES) break;
    const candidate = path.join(base, name);
    try {
      if (fs.statSync(candidate).isDirectory()) result.push(candidate);
    } catch {
      /* A dangling link or an unreadable entry is not a home. */
    }
  }
  return result;
}

/** A home path from T3's settings, `~` expanded; null unless it is absolute and plain. */
function settingsPath(value: unknown, homeDir: string): string | null {
  if (typeof value !== 'string') return null;
  const text = value.trim();
  if (!text || text.length > MAX_PATH_LENGTH || text.includes('\0')) return null;
  const expanded =
    text === '~' ? homeDir : /^~[\\/]/.test(text) ? path.join(homeDir, text.slice(2)) : text;
  return path.isAbsolute(expanded) ? path.normalize(expanded) : null;
}

/**
 * The account homes T3's settings name. Only `providerInstances.<id>.driver` and the home paths
 * of its `config` are read; credentials never live in these fields.
 */
export function t3SettingsHomes(
  settingsFile: string,
  homeDir: string
): { claude: string[]; codex: string[] } {
  const homes = { claude: [] as string[], codex: [] as string[] };
  let instances: unknown;
  try {
    if (fs.statSync(settingsFile).size > MAX_SETTINGS_BYTES) return homes;
    instances = (JSON.parse(fs.readFileSync(settingsFile, 'utf8')) as Record<string, unknown>)
      ?.providerInstances;
  } catch {
    return homes;
  }
  if (!instances || typeof instances !== 'object' || Array.isArray(instances)) return homes;
  for (const instance of Object.values(instances as Record<string, unknown>).slice(
    0,
    T3_MAX_HOMES
  )) {
    if (!instance || typeof instance !== 'object') continue;
    const { driver, config } = instance as { driver?: unknown; config?: unknown };
    if (!config || typeof config !== 'object') continue;
    const { homePath, shadowHomePath } = config as Record<string, unknown>;
    if (driver === 'claudeAgent') {
      const home = settingsPath(homePath, homeDir);
      if (home) homes.claude.push(home);
    } else if (driver === 'codex') {
      for (const value of [homePath, shadowHomePath]) {
        const home = settingsPath(value, homeDir);
        if (home) homes.codex.push(home);
      }
    }
  }
  return homes;
}

/** Existing folders, each real folder once, in the order given. */
function existingUnique(candidates: string[]): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const candidate of candidates) {
    try {
      const stats = fs.statSync(candidate);
      if (!stats.isDirectory()) continue;
      const identity = `${stats.dev}:${stats.ino}`;
      const real = fs.realpathSync(candidate);
      if (seen.has(real) || seen.has(identity)) continue;
      seen.add(real);
      seen.add(identity);
      result.push(candidate);
    } catch {
      /* Absent history is not an error. */
    }
  }
  return result;
}

/**
 * The T3 Code usage roots on this host. Cheap: a few directory reads and one small settings file.
 * Roots that resolve into a default root are still listed here; the request builder leaves them
 * out against the roots it already reads.
 */
export function resolveT3UsageRoots(options: { homeDir?: string } = {}): T3UsageRoots {
  const homeDir = options.homeDir ?? os.homedir();
  const state = t3StateDir(homeDir);
  const settings = t3SettingsHomes(path.join(state, 'settings.json'), homeDir);
  const claudeHomes = [...childDirs(path.join(homeDir, '.claude-t3')), ...settings.claude];
  const codexHomes = [
    ...childDirs(path.join(homeDir, '.codex-t3')),
    ...childDirs(path.join(state, 'providers', 'codex')).map((dir) => path.join(dir, 'shadow')),
    ...settings.codex,
  ];
  return {
    claude: existingUnique(claudeHomes.map((home) => path.join(home, 'projects'))),
    codex: existingUnique(codexHomes.map((home) => path.join(home, 'sessions'))),
  };
}

/**
 * The real path of a link found at `link`, or null when its target leaves `root`. The target is
 * first checked as written (no stat), so a link to a network mount is never touched: a stat on a
 * dead mount can block, and no deadline interrupts it. Windows may prefix the target with `\\?\`.
 */
function linkInside(link: string, root: string): string | null {
  try {
    const written = fs
      .readlinkSync(link)
      .replace(/^\\\\\?\\UNC\\/i, '\\\\')
      .replace(/^\\\\\?\\/, '');
    if (!inside(path.resolve(path.dirname(link), written), [root])) return null;
    const real = fs.realpathSync(link);
    return inside(real, [root]) ? real : null;
  } catch {
    return null;
  }
}

function inside(child: string, roots: string[]): boolean {
  return roots.some(
    (root) => child === root || child.startsWith(root.endsWith(path.sep) ? root : root + path.sep)
  );
}

export interface T3WalkLimits {
  deadline: number;
  maxDepth: number;
  maxDirectories: number;
  maxEntries: number;
  maxFiles: number;
}

/**
 * The wanted log files under one T3 root, by real path, each real file once. Unlike the default
 * walk this follows symlinks, because a T3 home may be made of them, but only to a target inside
 * the root's own real path: a link out of the root (into `~/.codex`, `~/PM-Experiments`, a network
 * mount) is skipped without touching its target. A root inside `exclude` (the roots of the same
 * tool the app already reads, so a shadow `sessions` linked to `~/.codex/sessions`) is not read,
 * nor is a folder of `exclude` inside the root. Every folder is entered once by device and inode,
 * so a link loop ends. Bounded like the default walk; `truncated` says a bound stopped it.
 */
export function filesUnderT3Root(
  root: string,
  accept: (name: string) => boolean,
  exclude: string[],
  limits: T3WalkLimits
): { files: string[]; truncated: boolean } {
  const files: string[] = [];
  const seenFiles = new Set<string>();
  const seenDirs = new Set<string>();
  let truncated = false;
  let start: string;
  try {
    start = fs.realpathSync(root);
  } catch {
    return { files, truncated };
  }
  const excluded = exclude.map((entry) => {
    try {
      return fs.realpathSync(entry);
    } catch {
      return path.resolve(entry);
    }
  });
  if (inside(start, excluded)) return { files, truncated };
  const pending = [{ directory: start, depth: 0 }];
  let entries = 0;
  while (pending.length) {
    if (Date.now() >= limits.deadline || seenDirs.size >= limits.maxDirectories) {
      truncated = true;
      break;
    }
    const current = pending.pop() as { directory: string; depth: number };
    let listing: fs.Dirent[];
    try {
      const stats = fs.statSync(current.directory);
      const identity = `${stats.dev}:${stats.ino}`;
      if (seenDirs.has(identity)) continue;
      seenDirs.add(identity);
      listing = fs.readdirSync(current.directory, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of listing) {
      if (++entries > limits.maxEntries || files.length >= limits.maxFiles) {
        return { files, truncated: true };
      }
      let full = path.join(current.directory, entry.name);
      let isDirectory = entry.isDirectory();
      let isFile = entry.isFile();
      if (entry.isSymbolicLink()) {
        const real = linkInside(full, start);
        if (!real) continue;
        try {
          full = real;
          const target = fs.statSync(full);
          isDirectory = target.isDirectory();
          isFile = target.isFile();
        } catch {
          continue;
        }
      }
      if (inside(full, excluded)) continue;
      if (isDirectory) {
        if (current.depth >= limits.maxDepth) truncated = true;
        else pending.push({ directory: full, depth: current.depth + 1 });
      } else if (isFile && accept(entry.name)) {
        try {
          const stats = fs.statSync(full);
          const identity = `${stats.dev}:${stats.ino}`;
          if (seenFiles.has(identity)) continue;
          seenFiles.add(identity);
          files.push(full);
        } catch {
          /* A file that vanished mid-walk is not counted. */
        }
      }
    }
  }
  return { files, truncated };
}

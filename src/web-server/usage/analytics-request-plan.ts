/**
 * The local log roots Analytics reads, found with a few stats and directory reads per root.
 *
 * Several roots can live on a network share (symlinked experiment and OMP folders). A stat on a
 * hard-mounted share that stops answering blocks its thread until the share returns, so this
 * runs on the plan worker thread (analytics-plan-worker.ts), never on the server's event loop.
 */
import * as fs from 'fs';
import * as path from 'path';
import { getCcsDir } from '../../utils/config-manager';
import { getClaudeProjectsDirForAnalytics } from '../../utils/claude-config-path';
import { listAccountInstancePaths } from '../../management/instance-directory';
import { resolveCodexConfigPaths } from '../services/compatible-cli-config-paths';
import { readDashboardPreferences, type UsageLogSource } from '../services/dashboard-preferences';
import type { AccountAnalyticsActivityRequest } from '../services/account-analytics-activity';
import { resolveOmpSessionRoots } from './omp-native-usage-collector';
import { resolveMuseSessionsDir } from './muse-native-usage-collector';
import { resolveZcodeDbPath } from './zcode-native-usage-collector';
import { antigravityUsagePresent } from './antigravity-native-usage-collector';
import {
  emptyRootSet,
  readExperimentRoots,
  type ExperimentRootSet,
  type ExperimentRootsView,
} from './experiment-usage-roots';
import { resolveT3UsageRoots, type T3UsageRoots } from './t3-usage-roots';

export interface ActivityPlanScope {
  minDate: number;
  cacheDir: string;
}

/** The roots a few stats find, plus what the later part needs to avoid reading a root twice. */
export interface ReadyActivityPlan {
  requests: AccountAnalyticsActivityRequest[];
  /** `<kind>:<real path>` of every built-in root read, for the later part's dedupe. */
  scanned: string[];
  /** The experiment-root walk's progress, so the server can start its next slice. */
  experimentView: ExperimentRootsView | null;
}

async function isDirectory(directory: string): Promise<boolean> {
  try {
    return (await fs.promises.stat(directory)).isDirectory();
  } catch {
    return false;
  }
}

/**
 * The roots a few stats find (Claude Code, Codex, Muse Code, zcode, Antigravity, the experiment
 * roots found so far and T3 Code homes), so their collectors start at once instead of waiting for
 * the slowest discovery step. Together with the later part they also say which tools are installed.
 */
export function readyActivityRequests(activity: ActivityPlanScope): ReadyActivityPlan {
  const ccsDir = getCcsDir();
  const claudeRoots = [getClaudeProjectsDirForAnalytics()];
  try {
    for (const instance of listAccountInstancePaths(path.join(ccsDir, 'instances')))
      claudeRoots.push(path.join(instance, 'projects'));
  } catch {
    /* No configured instance history. */
  }
  const unique = new Set<string>();
  const seen = (kind: string, resolved: string): boolean => {
    const key = `${kind}:${resolved}`;
    if (unique.has(key)) return true;
    unique.add(key);
    return false;
  };
  const requests: AccountAnalyticsActivityRequest[] = [];
  for (const directory of claudeRoots) {
    try {
      if (!fs.statSync(directory).isDirectory()) continue;
      const real = fs.realpathSync(directory);
      if (seen('claude', real)) continue;
      requests.push({
        provider: 'claude',
        request: { kind: 'claude', projectsDir: real, activity },
      });
    } catch {
      /* Absent history is not an error or measured zero. */
    }
  }
  const codexHome = resolveCodexConfigPaths().baseDir;
  if (fs.existsSync(path.join(codexHome, 'sessions'))) {
    try {
      seen('codex', fs.realpathSync(path.join(codexHome, 'sessions')));
    } catch {
      /* Realpath failure still scans under the configured home. */
    }
    requests.push({
      provider: 'codex',
      request: { kind: 'codex', codexHome, cacheDir: path.join(ccsDir, 'cache'), activity },
    });
  }
  try {
    const sessionsDir = resolveMuseSessionsDir();
    if (fs.statSync(sessionsDir).isDirectory()) {
      try {
        seen('muse', fs.realpathSync(sessionsDir));
      } catch {
        /* Realpath failure still scans under the resolved directory. */
      }
      requests.push({ provider: 'muse', request: { kind: 'muse', sessionsDir, activity } });
    }
  } catch {
    /* Absent history is not an error or measured zero. */
  }
  try {
    const dbPath = resolveZcodeDbPath();
    if (fs.statSync(dbPath).isFile()) {
      try {
        seen('zcode', fs.realpathSync(dbPath));
      } catch {
        /* Realpath failure still scans the resolved database. */
      }
      requests.push({ provider: 'zcode', request: { kind: 'zcode', dbPath, activity } });
    }
  } catch {
    /* Absent history is not an error or measured zero. */
  }
  // Antigravity (and T3 Code's Antigravity instances): the helper finds and reads the databases; a few
  // stats here only say whether any of its roots exists.
  try {
    if (antigravityUsagePresent())
      requests.push({ provider: 'antigravity', request: { kind: 'antigravity', activity } });
  } catch {
    /* Absent history is not an error or measured zero. */
  }
  // Experiment roots: what the background walk has found so far (a small cached file), read now so
  // these collectors start with the rest; the walk itself advances one slice in the background.
  // Saved extra sources count as read roots too: experiment roots inside them are left to them,
  // and Claude experiment copies are also checked against saved Claude sources.
  const readNow = new Set(unique);
  try {
    for (const source of readDashboardPreferences(ccsDir).usageLogSources) {
      if (!source || source.host !== 'ubuntu') continue;
      try {
        if (source.tool === 'claude-code' && fs.statSync(source.path).isDirectory())
          readNow.add(`claude:${fs.realpathSync(source.path)}`);
        else if (source.tool === 'codex' && fs.existsSync(path.join(source.path, 'sessions')))
          readNow.add(`codex:${fs.realpathSync(path.join(source.path, 'sessions'))}`);
        else if (source.tool === 'muse' && fs.statSync(source.path).isDirectory())
          readNow.add(`muse:${fs.realpathSync(source.path)}`);
        else if (source.tool === 'zcode' && fs.statSync(source.path).isFile())
          readNow.add(`zcode:${fs.realpathSync(source.path)}`);
      } catch {
        /* An unreadable saved source is skipped here as it is below. */
      }
    }
  } catch {
    /* Without prefs only the built-in roots count as read. */
  }
  let experimentView: ExperimentRootsView | null = null;
  try {
    experimentView = readExperimentRoots(activity.cacheDir);
  } catch {
    /* Without experiment roots the default and T3 roots still scan. */
  }
  // T3 Code account homes (a few directory reads): counted with the experiment roots, against the
  // same default roots, so a transcript or shadow-home link already read counts once.
  let t3: T3UsageRoots = { claude: [], codex: [] };
  try {
    t3 = resolveT3UsageRoots();
  } catch {
    /* Without T3 homes the other roots still scan. */
  }
  try {
    requests.push(
      ...experimentActivityRequests(
        experimentView?.roots ?? emptyRootSet(),
        {
          projectsDir: claudeRoots[0],
          codexHome,
          sessionsDir: resolveMuseSessionsDir(),
          dbPath: resolveZcodeDbPath(),
        },
        activity,
        path.join(ccsDir, 'cache'),
        readNow,
        t3,
        experimentView
      )
    );
  } catch {
    /* Without experiment roots the default roots still scan. */
  }
  return { requests, scanned: [...unique], experimentView };
}

/**
 * The OMP roots (the marker scan awaits every directory read and is bounded at 30 s) and the saved
 * extras, which must know every built-in root so the same logs are never read twice. Never rejects.
 */
export async function laterActivityRequests(
  activity: ActivityPlanScope,
  scanned: string[]
): Promise<AccountAnalyticsActivityRequest[]> {
  const ccsDir = getCcsDir();
  const unique = new Set(scanned);
  const discovered: AccountAnalyticsActivityRequest[] = [];
  try {
    const ompRoots: string[] = [];
    const discovery = { truncated: false };
    for (const root of await resolveOmpSessionRoots({
      cacheDir: path.join(ccsDir, 'cache'),
      discovery,
    }))
      if (await isDirectory(root)) ompRoots.push(root);
    if (ompRoots.length || discovery.truncated) {
      for (const root of ompRoots) {
        try {
          unique.add(`omp:${fs.realpathSync(root)}`);
        } catch {
          /* Realpath failure still scans under the resolved root. */
        }
      }
      discovered.push({
        provider: 'omp',
        request: {
          kind: 'omp',
          roots: ompRoots,
          activity: { ...activity, discoveryTruncated: discovery.truncated },
        },
      });
    }
  } catch {
    /* Absent history is not an error or measured zero. */
  }
  try {
    const prefs = readDashboardPreferences(ccsDir);
    discovered.push(...extraActivityRequests(prefs.usageLogSources, activity, ccsDir, unique));
  } catch {
    /* Without prefs the built-in roots still scan. */
  }
  return discovered;
}

/**
 * Requests for the experiment roots found so far, one per tool, each reading only its experiment
 * roots and deduplicating against the tool's default roots (collectExperimentActivity). `scanned`
 * holds `<kind>:<real path>` for every root the other requests read (built-in, instances, saved
 * sources). An experiment root that no longer exists, or that resolves to (or inside) a root of
 * its tool already read, is left out; every Claude root read is the Claude request's reference.
 * T3 Code homes (`t3`, t3-usage-roots.ts) ride in the same Claude and Codex requests as
 * `homeRoots`, under the same rule: a Codex shadow home whose `sessions` links into `~/.codex`
 * resolves inside the default root and is left out, so it is never read twice.
 */
export function experimentActivityRequests(
  found: ExperimentRootSet,
  defaults: { projectsDir: string; codexHome: string; sessionsDir: string; dbPath: string },
  activity: { minDate: number; cacheDir: string },
  cacheDir: string,
  scanned: Set<string>,
  t3: T3UsageRoots = { claude: [], codex: [] },
  discovery: ExperimentRootsView | null = null
): AccountAnalyticsActivityRequest[] {
  const readRoots = (kind: string): string[] =>
    [...scanned]
      .filter((key) => key.startsWith(`${kind}:`))
      .map((key) => key.slice(kind.length + 1));
  const pick = (kind: string, list: string[], wantFile = false): string[] => {
    const read = readRoots(kind);
    // The same folder reached another way (a bind mount) is a root already read, too.
    const readIdentities = new Set<string>();
    for (const root of read) {
      try {
        const stats = fs.statSync(root);
        readIdentities.add(`${stats.dev}:${stats.ino}`);
      } catch {
        /* A read root that is gone has no identity. */
      }
    }
    const kept = new Set<string>();
    for (const candidate of list) {
      try {
        const stats = fs.statSync(candidate);
        if (wantFile ? !stats.isFile() : !stats.isDirectory()) continue;
        if (readIdentities.has(`${stats.dev}:${stats.ino}`)) continue;
        const real = fs.realpathSync(candidate);
        const inside = read.some(
          (root) =>
            real === root || real.startsWith(root.endsWith(path.sep) ? root : root + path.sep)
        );
        if (!inside) kept.add(real);
      } catch {
        /* A root that is gone is skipped. */
      }
    }
    return [...kept];
  };
  const requests: AccountAnalyticsActivityRequest[] = [];
  const activityFor = (kind: keyof ExperimentRootSet) => ({
    ...activity,
    discoveryTruncated:
      !!discovery &&
      (discovery.truncated ||
        discovery.capped.includes(kind) ||
        (discovery.scanning && !discovery.completedAt)),
  });
  const claude = pick('claude', found.claude);
  const claudeHomes = pick('claude', t3.claude).filter((root) => !claude.includes(root));
  let projectsDir = defaults.projectsDir;
  try {
    projectsDir = fs.realpathSync(projectsDir);
  } catch {
    /* A missing default folder still names the request's cache. */
  }
  if (claude.length || claudeHomes.length || activityFor('claude').discoveryTruncated)
    requests.push({
      provider: 'claude',
      request: {
        kind: 'claude',
        projectsDir,
        experimentRoots: claude,
        ...(claudeHomes.length ? { homeRoots: claudeHomes } : {}),
        referenceRoots: [...new Set([projectsDir, ...readRoots('claude')])],
        activity: activityFor('claude'),
      },
    });
  const codex = pick('codex', found.codex);
  const codexHomes = pick('codex', t3.codex).filter((root) => !codex.includes(root));
  if (codex.length || codexHomes.length || activityFor('codex').discoveryTruncated)
    requests.push({
      provider: 'codex',
      request: {
        kind: 'codex',
        codexHome: defaults.codexHome,
        cacheDir,
        experimentRoots: codex,
        ...(codexHomes.length ? { homeRoots: codexHomes } : {}),
        activity: activityFor('codex'),
      },
    });
  const muse = pick('muse', found.muse);
  if (muse.length || activityFor('muse').discoveryTruncated)
    requests.push({
      provider: 'muse',
      request: {
        kind: 'muse',
        sessionsDir: defaults.sessionsDir,
        experimentRoots: muse,
        activity: activityFor('muse'),
      },
    });
  const zcode = pick('zcode', found.zcode, true);
  if (zcode.length || activityFor('zcode').discoveryTruncated)
    requests.push({
      provider: 'zcode',
      request: {
        kind: 'zcode',
        dbPath: defaults.dbPath,
        experimentDbs: zcode,
        activity: activityFor('zcode'),
      },
    });
  return requests;
}

/**
 * Saved extra usage-log locations for this host (Ubuntu) as worker requests.
 * The built-in roots stay the defaults; each extra adds one request. An extra
 * that resolves to a root already scanned (built-in or an earlier extra) is
 * skipped, so the same logs are never counted twice; two generic JSONL
 * sources may share a root only with different field mappings. Unreadable or
 * unmapped extras are skipped, never a failed collection.
 */
export function extraActivityRequests(
  sources: UsageLogSource[],
  activity: { minDate: number; cacheDir: string },
  ccsDir: string,
  scanned: Set<string>
): AccountAnalyticsActivityRequest[] {
  const requests: AccountAnalyticsActivityRequest[] = [];
  let excludeRoots: string[] = [];
  const claim = (kind: string, resolved: string, mapping = ''): boolean => {
    const key = `${kind}:${resolved}${mapping}`;
    excludeRoots = [];
    const inside = (child: string, parent: string) =>
      child === parent || child.startsWith(parent.endsWith(path.sep) ? parent : parent + path.sep);
    for (const existing of scanned) {
      const colon = existing.indexOf(':');
      const tool = existing.slice(0, colon);
      const [root, priorMapping = ''] = existing.slice(colon + 1).split('\n');
      if (tool !== kind && kind !== 'jsonl' && tool !== 'jsonl') continue;
      if (tool === 'jsonl' && kind === 'jsonl' && `\n${priorMapping}` !== mapping) continue;
      if (inside(resolved, root)) return false;
      if (inside(root, resolved)) excludeRoots.push(root);
    }
    scanned.add(key);
    return true;
  };
  for (const source of sources) {
    if (!source || source.host !== 'ubuntu') continue;
    try {
      if (source.tool === 'claude-code') {
        if (!fs.statSync(source.path).isDirectory()) continue;
        const real = fs.realpathSync(source.path);
        if (!claim('claude', real)) continue;
        requests.push({
          provider: 'claude',
          request: { kind: 'claude', projectsDir: real, activity: { ...activity, excludeRoots } },
        });
      } else if (source.tool === 'codex') {
        if (!fs.existsSync(path.join(source.path, 'sessions'))) continue;
        const real = fs.realpathSync(path.join(source.path, 'sessions'));
        if (!claim('codex', real)) continue;
        requests.push({
          provider: 'codex',
          request: {
            kind: 'codex',
            codexHome: source.path,
            cacheDir: path.join(ccsDir, 'cache'),
            activity: { ...activity, excludeRoots },
          },
        });
      } else if (source.tool === 'omp') {
        if (!fs.statSync(source.path).isDirectory()) continue;
        const real = fs.realpathSync(source.path);
        if (!claim('omp', real)) continue;
        requests.push({
          provider: 'omp',
          request: { kind: 'omp', roots: [real], activity: { ...activity, excludeRoots } },
        });
      } else if (source.tool === 'muse') {
        if (!fs.statSync(source.path).isDirectory()) continue;
        const real = fs.realpathSync(source.path);
        if (!claim('muse', real)) continue;
        requests.push({
          provider: 'muse',
          request: { kind: 'muse', sessionsDir: real, activity: { ...activity, excludeRoots } },
        });
      } else if (source.tool === 'zcode') {
        if (!fs.statSync(source.path).isFile()) continue;
        const real = fs.realpathSync(source.path);
        if (!claim('zcode', real)) continue;
        requests.push({ provider: 'zcode', request: { kind: 'zcode', dbPath: real, activity } });
      } else if (source.tool === 'jsonl') {
        const fieldMapping = source.fieldMapping;
        if (!fieldMapping || typeof fieldMapping.timestamp !== 'string') continue;
        if (!fs.statSync(source.path).isDirectory()) continue;
        const real = fs.realpathSync(source.path);
        const mapping = {
          timestamp: fieldMapping.timestamp,
          ...(typeof fieldMapping.model === 'string' ? { model: fieldMapping.model } : {}),
          ...(typeof fieldMapping.inputTokens === 'string'
            ? { inputTokens: fieldMapping.inputTokens }
            : {}),
          ...(typeof fieldMapping.outputTokens === 'string'
            ? { outputTokens: fieldMapping.outputTokens }
            : {}),
          ...(typeof fieldMapping.cost === 'string' ? { cost: fieldMapping.cost } : {}),
        };
        if (!claim('jsonl', real, `\n${JSON.stringify(mapping)}`)) continue;
        requests.push({
          provider: 'jsonl',
          request: {
            kind: 'jsonl',
            roots: [real],
            mapping,
            activity: { ...activity, excludeRoots },
          },
        });
      }
    } catch {
      /* An unreadable extra is skipped, never a failed collection. */
    }
  }
  return requests;
}

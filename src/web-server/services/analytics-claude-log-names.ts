/** Metadata-only inventory for SSH mirror selection; never opens transcript contents. */
import * as fs from 'fs';
import * as path from 'path';
import { createHash } from 'crypto';
import { getCcsDir } from '../../utils/config-manager';
import { getClaudeProjectsDirForAnalytics } from '../../utils/claude-config-path';
import { listAccountInstancePaths } from '../../management/instance-directory';
import { EXPERIMENT_CLAUDE_ROOT_DEPTH, readExperimentRoots } from '../usage/experiment-usage-roots';
import { filesUnder } from '../usage/account-activity-collector';
import { filesUnderT3Root, resolveT3UsageRoots } from '../usage/t3-usage-roots';
import { readDashboardPreferences } from './dashboard-preferences';

export interface ClaudeLogNames {
  originals: Set<string>;
  complete: boolean;
}

export function localClaudeLogNames(cacheDir: string, minDateMs: number): ClaudeLogNames {
  const roots = [getClaudeProjectsDirForAnalytics()];
  let t3: string[];
  let experiments: string[];
  try {
    roots.push(
      ...listAccountInstancePaths(path.join(getCcsDir(), 'instances')).map((root) =>
        path.join(root, 'projects')
      )
    );
  } catch {
    /* No account instance history. */
  }
  try {
    roots.push(
      ...readDashboardPreferences()
        .usageLogSources.filter(
          (source) => source.host === 'ubuntu' && source.tool === 'claude-code'
        )
        .map((source) => source.path)
    );
    t3 = resolveT3UsageRoots().claude;
    experiments = readExperimentRoots(cacheDir).roots.claude;
  } catch {
    return { originals: new Set(), complete: false };
  }
  const originals = new Set<string>();
  let complete = true;
  const deadline = Date.now() + 2000;
  // Use each collector's own walk and scope: an unscanned symlink or a deeper
  // experiment file must never suppress a mirror that actually holds usage.
  const addNames = (files: string[]): void => {
    for (const file of files) {
      if (file.split(path.sep).some((part) => /^ssh-[0-9a-f-]{36}$/i.test(part))) continue;
      try {
        if (fs.statSync(file).mtimeMs < minDateMs - 86_400_000) continue;
        originals.add(createHash('sha256').update(path.basename(file)).digest('hex'));
        if (originals.size >= 20_000) {
          complete = false;
          return;
        }
      } catch {
        complete = false;
      }
    }
  };
  for (const root of new Set([...roots, ...experiments])) {
    if (originals.size >= 20_000) break;
    if (!fs.existsSync(root)) continue;
    const issues = { failed: 0 };
    addNames(
      filesUnder(
        root,
        'claude',
        issues,
        deadline,
        undefined,
        roots.includes(root) ? undefined : EXPERIMENT_CLAUDE_ROOT_DEPTH
      )
    );
    if (issues.failed) complete = false;
  }
  for (const root of t3) {
    if (originals.size >= 20_000) break;
    const walked = filesUnderT3Root(root, (name) => name.endsWith('.jsonl'), roots, {
      deadline,
      maxDepth: 64,
      maxDirectories: 10_000,
      maxEntries: 100_000,
      maxFiles: 20_000,
    });
    addNames(walked.files);
    if (walked.truncated) complete = false;
  }
  return { originals, complete };
}

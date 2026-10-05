import * as path from 'path';
import { getCcsDir, getCcsHome } from './config-manager';

/**
 * Resolve the canonical default Claude config directory.
 * Ignores CLAUDE_CONFIG_DIR so CCS can keep a stable source of truth
 * for shared plugin/channel state while still honoring test/dev home overrides.
 */
export function getDefaultClaudeConfigDir(): string {
  return path.join(getCcsHome(), '.claude');
}

/**
 * Resolve Claude config directory with test/dev overrides.
 * Precedence:
 * 1. CLAUDE_CONFIG_DIR (explicit override)
 * 2. CCS_HOME compatibility path (<CCS_HOME>/.claude)
 * 3. ~/.claude (default)
 */
export function getClaudeConfigDir(): string {
  if (process.env.CLAUDE_CONFIG_DIR) {
    return path.resolve(process.env.CLAUDE_CONFIG_DIR);
  }

  return getDefaultClaudeConfigDir();
}

/** Resolve user-scope Claude JSON config path (~/.claude.json). */
export function getClaudeUserConfigPath(): string {
  return path.join(getCcsHome(), '.claude.json');
}

/** Resolve Claude settings.json path. */
export function getClaudeSettingsPath(): string {
  return path.join(getClaudeConfigDir(), 'settings.json');
}

/**
 * The Claude `projects` directory the usage readers scan: the active Claude config dir, unless
 * that dir is one of CCS's own account instances, in which case the canonical default (instance
 * directories are scanned separately, so scanning one as the default too would count it twice).
 * Home's usage aggregator and Analytics both resolve it here, so the two read the same Claude
 * logs and honour the same CLAUDE_CONFIG_DIR override.
 */
export function getClaudeProjectsDirForAnalytics(): string {
  const active = getClaudeConfigDir();
  const instances = path.join(getCcsDir(), 'instances');
  const relative = path.relative(instances, active);
  const insideInstances =
    relative.length > 0 && !relative.startsWith('..') && !path.isAbsolute(relative);
  return path.join(insideInstances ? getDefaultClaudeConfigDir() : active, 'projects');
}

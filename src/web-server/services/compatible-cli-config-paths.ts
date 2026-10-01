/** Read-only native history paths retained from the CCS-compatible storage schema. */
import * as os from 'os';
import * as path from 'path';
import { expandPath } from '../../utils/helpers';

export interface CodexConfigPaths {
  configPath: string;
  configDisplayPath: string;
  baseDir: string;
  baseDirDisplay: string;
}

export interface DroidConfigPaths {
  settingsPath: string;
  settingsDisplayPath: string;
  legacyConfigPath: string;
  legacyConfigDisplayPath: string;
}

interface ConfigPathOptions {
  platform?: NodeJS.Platform;
  env?: NodeJS.ProcessEnv;
  homeDir?: string;
}

export function resolveCodexConfigPaths(options: ConfigPathOptions = {}): CodexConfigPaths {
  const env = options.env ?? process.env;
  const homeDir = options.homeDir ?? os.homedir();
  const baseDir = path.resolve(
    env.CODEX_HOME ? expandPath(env.CODEX_HOME) : path.join(homeDir, '.codex')
  );
  const baseDirDisplay = env.CODEX_HOME ? '$CODEX_HOME' : '~/.codex';

  return {
    baseDir,
    baseDirDisplay,
    configPath: path.join(baseDir, 'config.toml'),
    configDisplayPath: `${baseDirDisplay}/config.toml`,
  };
}

export function resolveDroidConfigPaths(options: ConfigPathOptions = {}): DroidConfigPaths {
  const env = options.env ?? process.env;
  const homeDir = options.homeDir ?? os.homedir();

  const byokBase = env.CCS_HOME || homeDir;
  const settingsPath = path.join(byokBase, '.factory', 'settings.json');
  const legacyConfigPath = path.join(byokBase, '.factory', 'config.json');

  return {
    settingsPath,
    settingsDisplayPath: '~/.factory/settings.json',
    legacyConfigPath,
    legacyConfigDisplayPath: '~/.factory/config.json',
  };
}

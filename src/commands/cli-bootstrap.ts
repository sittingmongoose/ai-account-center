import * as fs from 'fs';
import { detectCloudSyncPath, setGlobalConfigDir } from '../utils/config-manager';

export interface CliBootstrapResult {
  args: string[];
  exitNow: boolean;
}

/** Preserve the existing private configuration location without running legacy setup. */
export async function prepareCliArguments(rawArgs: string[]): Promise<CliBootstrapResult> {
  const args = [...rawArgs];
  const configDirIndices = args
    .map((arg, index) => (arg === '--config-dir' || arg.startsWith('--config-dir=') ? index : -1))
    .filter((index) => index !== -1);
  if (configDirIndices.length > 1) {
    console.error('[X] --config-dir may be provided only once');
    process.exitCode = 1;
    return { args, exitNow: true };
  }

  let configDir = process.env.CCS_DIR || process.env.CCS_HOME;
  const index = configDirIndices[0];
  if (index !== undefined) {
    const token = args[index];
    const inline = token.startsWith('--config-dir=');
    configDir = inline ? token.slice('--config-dir='.length) : args[index + 1];
    if (!configDir || configDir.startsWith('-')) {
      console.error('[X] --config-dir requires a path argument');
      process.exitCode = 1;
      return { args, exitNow: true };
    }
    try {
      if (!fs.statSync(configDir).isDirectory()) {
        console.error(`[X] Not a directory: ${configDir}`);
        process.exitCode = 1;
        return { args, exitNow: true };
      }
    } catch {
      console.error(`[X] Config directory not found: ${configDir}`);
      console.error('[i] Create the directory first, then copy your config files into it.');
      process.exitCode = 1;
      return { args, exitNow: true };
    }
    setGlobalConfigDir(configDir);
    args.splice(index, inline ? 1 : 2);
  }

  if (configDir) {
    const cloudService = detectCloudSyncPath(configDir);
    if (cloudService) {
      console.error(`[!] Account configuration directory is under ${cloudService}.`);
      console.error('    Saved account credentials may be synced to cloud.');
      console.error('    Use CCS_DIR=/path/outside/cloud ai-account-center ...');
    }
  }
  return { args, exitNow: false };
}

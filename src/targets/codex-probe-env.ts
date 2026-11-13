/** Shell and environment isolation used only by the Codex binary probe. */
import * as path from 'path';
import type { SpawnOptions } from 'child_process';
const DEFAULT_WINDOWS_CMD_SHELL = 'C:\\Windows\\System32\\cmd.exe';

export function stripAnthropicEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const result: NodeJS.ProcessEnv = {};
  for (const key of Object.keys(env)) {
    if (!key.startsWith('ANTHROPIC_')) {
      result[key] = env[key];
    }
  }
  return result;
}

export function stripBrowserEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const result: NodeJS.ProcessEnv = {};
  for (const key of Object.keys(env)) {
    if (!key.toUpperCase().startsWith('CCS_BROWSER_')) {
      result[key] = env[key];
    }
  }
  return result;
}

export function stripCodexSessionEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const sessionKeys = new Set(['CODEX_CI', 'CODEX_MANAGED_BY_BUN', 'CODEX_THREAD_ID']);
  const result: NodeJS.ProcessEnv = {};
  for (const key of Object.keys(env)) {
    const upperKey = key.toUpperCase();
    if (sessionKeys.has(upperKey)) {
      continue;
    }
    result[key] = env[key];
  }
  return result;
}

export function escapeShellArg(arg: string): string {
  const isWindows = process.platform === 'win32';

  if (isWindows) {
    // cmd.exe: Use double quotes, escape inner double quotes by doubling them
    // cmd.exe interprets "" as escaped double quote inside quoted string
    // Strip newlines/tabs that can break cmd.exe parsing
    return (
      '"' +
      String(arg)
        .replace(/[\r\n\t]/g, ' ') // Replace newlines/tabs with space
        .replace(/%/g, '%%') // Escape percent signs
        .replace(/\^/g, '^^') // Escape carets
        .replace(/!/g, '^^!') // Escape exclamation marks (delayed expansion)
        .replace(/"/g, '""') + // Escape quotes
      '"'
    );
  } else {
    // Unix/macOS: Double quotes with escaped inner quotes
    return '"' + String(arg).replace(/"/g, '\\"') + '"';
  }
}

export function getWindowsEscapedCommandShell(): SpawnOptions['shell'] {
  if (process.platform !== 'win32') {
    return true;
  }

  const systemRoot = [process.env.SystemRoot, process.env.SYSTEMROOT, process.env.windir].find(
    (candidate): candidate is string => Boolean(candidate && path.win32.isAbsolute(candidate))
  );
  const trustedSystemCmd = systemRoot
    ? path.win32.normalize(path.win32.join(systemRoot, 'System32', 'cmd.exe'))
    : DEFAULT_WINDOWS_CMD_SHELL;
  const trustedSystemCmdLower = trustedSystemCmd.toLowerCase();

  for (const candidate of [process.env.ComSpec, process.env.COMSPEC]) {
    if (!candidate || !path.win32.isAbsolute(candidate)) {
      continue;
    }

    const normalizedCandidate = path.win32.normalize(candidate);
    if (normalizedCandidate.toLowerCase() === trustedSystemCmdLower) {
      return normalizedCandidate;
    }
  }

  return trustedSystemCmd;
}

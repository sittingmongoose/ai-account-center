import * as fs from 'fs';
import * as path from 'path';
import { ConfigError } from '../errors/error-types';
import { resolveCanonicalPath, symlinkPointsTo } from './codex-plugin-paths';
import { createLogger } from '../services/logging';

const logger = createLogger('codex-auth:resources');
const FALLBACK_SAFE_SYMLINK_ERRORS = new Set(['EPERM', 'EACCES', 'ENOSYS']);

export function ensureSharedPluginCache(profileDir: string, sharedCodexHome: string): void {
  const targetPath = path.join(sharedCodexHome, 'plugins', 'cache');
  const pluginsPath = path.join(profileDir, 'plugins');
  const linkPath = path.join(pluginsPath, 'cache');

  ensureProfileLocalPluginsDirectory(pluginsPath);
  fs.mkdirSync(targetPath, { recursive: true, mode: 0o700 });

  if (resolveCanonicalPath(pluginsPath) === resolveCanonicalPath(path.dirname(targetPath))) {
    throw new ConfigError(
      'Refusing plugin cache repair: profile plugins directory resolves to the shared plugins directory.'
    );
  }

  const existingStat = lstatIfExists(linkPath);
  if (isExpectedCacheLink(linkPath, targetPath, existingStat)) {
    return;
  }

  let backupPath = existingStat === null ? null : createPluginCacheBackupPath(pluginsPath);
  if (backupPath !== null) {
    try {
      fs.renameSync(linkPath, backupPath);
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code !== 'ENOENT') {
        throw err;
      }
      backupPath = null;
    }
  }

  try {
    fs.symlinkSync(targetPath, linkPath, 'dir');
  } catch (err) {
    if (isExpectedCacheLink(linkPath, targetPath)) {
      removePluginCacheBackup(backupPath);
      return;
    }

    const pathAfterFailure = lstatIfExists(linkPath);
    if (backupPath !== null && pathAfterFailure === null) {
      if (restorePluginCacheWithoutOverwrite(linkPath, backupPath)) {
        backupPath = null;
      }
    }

    const code = (err as NodeJS.ErrnoException | null)?.code;
    if (!code || !FALLBACK_SAFE_SYMLINK_ERRORS.has(code)) {
      throw err;
    }

    if (isExpectedCacheLink(linkPath, targetPath)) {
      removePluginCacheBackup(backupPath);
      return;
    }
    const fallbackPathStat = lstatIfExists(linkPath);
    if (backupPath !== null && fallbackPathStat?.isDirectory()) {
      mergeMissingResourceTree(backupPath, linkPath);
      removePluginCacheBackup(backupPath);
    }

    ensureLocalPluginCacheDirectory(linkPath);
    mergeMissingResourceTree(targetPath, linkPath);
    process.stderr.write(
      `[!] codex-auth: symlink unavailable; using profile-local plugin cache at ${linkPath}. ` +
        `Copied missing shared entries; plugin cache updates won't propagate automatically.\n`
    );
    logger.warn(
      'codex-auth.plugin-cache-copy-fallback',
      'Copied shared plugin cache after symlink failure',
      {
        link: linkPath,
        target: targetPath,
        error: err instanceof Error ? err.message : String(err),
      }
    );
    return;
  }

  removePluginCacheBackup(backupPath);

  logger.stage(
    'dispatch',
    'codex.plugin-cache.symlink.created',
    'Created shared plugin cache symlink',
    {
      link: linkPath,
      target: targetPath,
    }
  );
}

function ensureProfileLocalPluginsDirectory(pluginsPath: string): void {
  const pluginsStat = lstatIfExists(pluginsPath);
  if (pluginsStat === null) {
    fs.mkdirSync(pluginsPath, { recursive: true, mode: 0o700 });
    return;
  }
  if (!pluginsStat.isDirectory()) {
    throw new ConfigError(
      'Refusing plugin cache repair: profile plugins path is not a local directory.'
    );
  }
}

function isExpectedCacheLink(
  linkPath: string,
  targetPath: string,
  stat: fs.Stats | null = lstatIfExists(linkPath)
): boolean {
  if (!stat?.isSymbolicLink()) {
    return false;
  }
  return symlinkPointsTo(linkPath, targetPath);
}

function lstatIfExists(resourcePath: string): fs.Stats | null {
  try {
    return fs.lstatSync(resourcePath);
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === 'ENOENT') {
      return null;
    }
    throw err;
  }
}

function createPluginCacheBackupPath(pluginsPath: string): string {
  return path.join(
    pluginsPath,
    `.cache.ccs-backup-${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}`
  );
}

function removePluginCacheBackup(backupPath: string | null): void {
  if (backupPath !== null) {
    fs.rmSync(backupPath, { recursive: true, force: true });
  }
}

function ensureLocalPluginCacheDirectory(linkPath: string): void {
  let existingStat = lstatIfExists(linkPath);
  if (existingStat?.isDirectory()) {
    return;
  }
  if (existingStat !== null) {
    throw new ConfigError(
      'Refusing plugin cache fallback: profile cache path is not a local directory.'
    );
  }

  try {
    fs.mkdirSync(linkPath, { recursive: false, mode: 0o700 });
    return;
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code !== 'EEXIST') {
      throw err;
    }
  }

  existingStat = lstatIfExists(linkPath);
  if (!existingStat?.isDirectory()) {
    throw new ConfigError(
      'Refusing plugin cache fallback: profile cache path is not a local directory.'
    );
  }
}

function restorePluginCacheWithoutOverwrite(linkPath: string, backupPath: string): boolean {
  const backupStat = lstatIfExists(backupPath);
  if (!backupStat?.isDirectory()) {
    return false;
  }

  let linkStat = lstatIfExists(linkPath);
  if (linkStat === null) {
    try {
      fs.mkdirSync(linkPath, { recursive: false, mode: 0o700 });
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code !== 'EEXIST') {
        throw err;
      }
    }
    linkStat = lstatIfExists(linkPath);
  }

  if (!linkStat?.isDirectory()) {
    return false;
  }

  mergeMissingResourceTree(backupPath, linkPath);
  removePluginCacheBackup(backupPath);
  return true;
}

function mergeMissingResourceTree(sourceDir: string, targetDir: string): void {
  for (const entry of fs.readdirSync(sourceDir, { withFileTypes: true })) {
    const sourcePath = path.join(sourceDir, entry.name);
    const targetPath = path.join(targetDir, entry.name);
    const targetStat = lstatIfExists(targetPath);

    if (targetStat === null) {
      fs.cpSync(sourcePath, targetPath, {
        recursive: true,
        force: false,
        errorOnExist: false,
        preserveTimestamps: true,
      });
      continue;
    }

    if (entry.isDirectory() && targetStat.isDirectory()) {
      mergeMissingResourceTree(sourcePath, targetPath);
    }
  }
}

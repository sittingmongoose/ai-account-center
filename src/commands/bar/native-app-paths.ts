import { spawnSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { ConfigError } from '../../errors/error-types';

export const PRODUCT_NAME = 'AI Account Center';
export const MAC_APP_NAME = 'AI Account Center.app';
export const MAC_LEGACY_APP_NAME = 'CCS Bar.app';
export const MAC_BUNDLE_ID = 'party.sittingmongoose.ccs.accounts-bar';
export const MAC_EXECUTABLE = 'CCSBar';

export interface NativeCommandResult {
  status: number | null;
  stdout: string;
  stderr: string;
}

export type NativeCommandRunner = (command: string, args: string[]) => NativeCommandResult;

export function runNativeCommand(command: string, args: string[]): NativeCommandResult {
  const result = spawnSync(command, args, {
    encoding: 'utf8',
    shell: false,
    timeout: 5000,
    maxBuffer: 1024 * 1024,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  if (result.error) throw new ConfigError(`Unable to run ${path.basename(command)} safely.`);
  return { status: result.status, stdout: result.stdout ?? '', stderr: result.stderr ?? '' };
}

export interface OwnedMacApp {
  path: string;
  version: string | null;
}
export interface NativeAppOptions {
  runner?: NativeCommandRunner;
  readPlist?: (file: string) => Record<string, unknown>;
}

export function getMacAppPaths(home = os.homedir(), appsDirOverride?: string) {
  const appsDir = path.resolve(appsDirOverride ?? path.join(home, 'Applications'));
  return {
    appsDir,
    canonical: path.join(appsDir, MAC_APP_NAME),
    legacy: path.join(appsDir, MAC_LEGACY_APP_NAME),
  };
}

function lstatIfPresent(file: string): fs.Stats | null {
  try {
    return fs.lstatSync(file);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw error;
  }
}

function requireOwnedRegular(file: string, directory = false): fs.Stats {
  const stat = lstatIfPresent(file);
  if (
    !stat ||
    stat.isSymbolicLink() ||
    (directory ? !stat.isDirectory() : !stat.isFile()) ||
    (typeof process.getuid === 'function' && stat.uid !== process.getuid())
  ) {
    throw new ConfigError(`Refusing an unowned or unexpected native app path: ${file}`);
  }
  return stat;
}

function decodeXmlText(value: string): string {
  return value.replace(/&([^;]+);/g, (_match, entity: string) => {
    const named: Record<string, string> = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'" };
    if (Object.prototype.hasOwnProperty.call(named, entity)) return named[entity];
    const code = /^#x[0-9a-f]+$/i.test(entity)
      ? Number.parseInt(entity.slice(2), 16)
      : /^#\d+$/.test(entity)
        ? Number.parseInt(entity.slice(1), 10)
        : NaN;
    if (!Number.isSafeInteger(code) || code < 0 || code > 0x10ffff) {
      throw new ConfigError('Invalid native app plist text.');
    }
    return String.fromCodePoint(code);
  });
}

/** Non-expanding XML reader; it never resolves a DTD or external entity. */
function parseXmlPlist(source: string): Record<string, unknown> {
  const xml = source
    .replace(/<\?xml[^?]*\?>/g, '')
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/<!DOCTYPE[^>[\]]*>/g, '')
    .trim();
  let offset = 0;
  const fail = (): never => {
    throw new ConfigError('Invalid native app plist structure.');
  };
  const space = () => {
    while (/\s/.test(xml[offset] ?? '') && offset < xml.length) offset++;
  };
  const take = (tag: string): boolean => {
    space();
    if (!xml.startsWith(tag, offset)) return false;
    offset += tag.length;
    return true;
  };
  const text = (tag: string): string => {
    if (take(`<${tag}/>`)) return '';
    if (!take(`<${tag}>`)) return fail();
    const end = xml.indexOf(`</${tag}>`, offset);
    if (end < 0) return fail();
    const value = xml.slice(offset, end);
    if (value.includes('<')) return fail();
    offset = end + tag.length + 3;
    return decodeXmlText(value);
  };
  const value = (depth: number): unknown => {
    if (depth > 32) return fail();
    if (take('<dict/>')) return {};
    if (take('<dict>')) {
      const result: Record<string, unknown> = Object.create(null);
      while (!take('</dict>')) {
        const key = text('key');
        if (Object.prototype.hasOwnProperty.call(result, key)) return fail();
        result[key] = value(depth + 1);
      }
      return result;
    }
    if (take('<array/>')) return [];
    if (take('<array>')) {
      const result: unknown[] = [];
      while (!take('</array>')) result.push(value(depth + 1));
      return result;
    }
    if (take('<true/>') || take('<true />')) return true;
    if (take('<false/>') || take('<false />')) return false;
    space();
    for (const tag of ['string', 'date', 'data']) {
      if (xml.startsWith(`<${tag}>`, offset) || xml.startsWith(`<${tag}/>`, offset))
        return text(tag);
    }
    for (const tag of ['integer', 'real']) {
      if (xml.startsWith(`<${tag}>`, offset)) {
        const number = Number(text(tag));
        if (!Number.isFinite(number)) return fail();
        return number;
      }
    }
    return fail();
  };
  const opening = /^<plist(?:\s+[^>]*)?>/.exec(xml);
  if (!opening) return fail();
  offset = opening[0].length;
  const result = value(0);
  if (!take('</plist>')) return fail();
  space();
  if (offset !== xml.length || !result || typeof result !== 'object' || Array.isArray(result))
    return fail();
  return result as Record<string, unknown>;
}

export function readMacPlist(
  file: string,
  runner: NativeCommandRunner = runNativeCommand
): Record<string, unknown> {
  const stat = requireOwnedRegular(file);
  if (stat.size > 1024 * 1024) throw new ConfigError('Native app plist is too large.');
  const data = fs.readFileSync(file);
  if (data.subarray(0, 8).toString('ascii') !== 'bplist00')
    return parseXmlPlist(data.toString('utf8'));
  const result = runner('/usr/bin/plutil', ['-convert', 'json', '-o', '-', file]);
  if (result.status !== 0) throw new ConfigError('Unable to read native app binary plist.');
  try {
    const decoded: unknown = JSON.parse(result.stdout);
    if (decoded && typeof decoded === 'object' && !Array.isArray(decoded))
      return decoded as Record<string, unknown>;
  } catch {
    /* Malformed conversion cannot establish app ownership. */
  }
  throw new ConfigError('Invalid native app binary plist.');
}

export function inspectOwnedMacApp(
  appPath: string,
  options: NativeAppOptions = {}
): OwnedMacApp | null {
  if (lstatIfPresent(appPath) === null) return null;
  requireOwnedRegular(appPath, true);
  const contents = path.join(appPath, 'Contents');
  requireOwnedRegular(contents, true);
  requireOwnedRegular(path.join(contents, 'MacOS'), true);
  const infoFile = path.join(contents, 'Info.plist');
  requireOwnedRegular(infoFile);
  const info = options.readPlist?.(infoFile) ?? readMacPlist(infoFile, options.runner);
  if (info.CFBundleIdentifier !== MAC_BUNDLE_ID || info.CFBundleExecutable !== MAC_EXECUTABLE) {
    throw new ConfigError(`Refusing a foreign native app bundle: ${appPath}`);
  }
  const executable = requireOwnedRegular(path.join(contents, 'MacOS', MAC_EXECUTABLE));
  if ((executable.mode & 0o111) === 0)
    throw new ConfigError(`Native app executable is not executable: ${appPath}`);
  const version = [info.CFBundleShortVersionString, info.CFBundleVersion].find(
    (value): value is string =>
      typeof value === 'string' && value.trim() !== '' && value.length <= 128
  );
  return { path: appPath, version: version?.trim() ?? null };
}

/** Preflight both names; an occupied foreign target never silently falls back. */
export function resolveOwnedMacApp(
  appsDir: string,
  options: NativeAppOptions = {}
): OwnedMacApp | null {
  const paths = getMacAppPaths(undefined, appsDir);
  if (lstatIfPresent(paths.appsDir)) requireOwnedRegular(paths.appsDir, true);
  const canonical = inspectOwnedMacApp(paths.canonical, options);
  const legacyStat = lstatIfPresent(paths.legacy);
  let legacy: OwnedMacApp | null = null;
  if (legacyStat?.isSymbolicLink()) {
    if (
      !canonical ||
      (typeof process.getuid === 'function' && legacyStat.uid !== process.getuid()) ||
      fs.readlinkSync(paths.legacy) !== MAC_APP_NAME ||
      fs.realpathSync(paths.legacy) !== fs.realpathSync(paths.canonical)
    ) {
      throw new ConfigError(`Refusing a foreign native app alias: ${paths.legacy}`);
    }
  } else if (legacyStat) legacy = inspectOwnedMacApp(paths.legacy, options);
  return canonical ?? legacy;
}

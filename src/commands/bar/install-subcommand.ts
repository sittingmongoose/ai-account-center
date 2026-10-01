/**
 * Build the owned native app shipped with this CLI and run its audited installer.
 * No floating release, archive download, account mutation, or dashboard login.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { spawn } from 'child_process';
import { getCcsDir } from '../../config/config-loader-facade';
import { BinaryError } from '../../errors/error-types';
import { hasAnyFlag } from '../arg-extractor';
import { inspectOwnedMacApp } from './native-app-paths';

const APP_NAME = 'AI Account Center.app';
const MAC_REQUIRED_FILES = [
  'Package.swift',
  'VERSION',
  'LICENSE',
  'Resources/Info.plist',
  'Scripts/package_app.sh',
  'Scripts/install_user.sh',
  'Scripts/install_bundle.py',
] as const;
const MAC_SOURCE_PATHS = ['Package.swift', 'VERSION', 'LICENSE', 'Sources', 'Scripts', 'Resources'];

export interface InstallCommandOptions {
  cwd: string;
}

export interface InstallDeps {
  /** Native builds run only on their supported operating system. */
  getPlatform: () => NodeJS.Platform;
  /** The installed CLI package root, independent of the user's current directory. */
  getPackageRoot: () => string;
  /** Injectable process boundary; never invoke shell command strings. */
  runCommand: (file: string, args: string[], options: InstallCommandOptions) => Promise<void>;
  /** Version metadata only; saved connection material is never read or rewritten. */
  getCcsDir: () => string;
  /** Used to read the installed app version; native installer owns its destination. */
  getAppsDir: () => string;
  readAppBundleVersion: (appPath: string) => string | null;
  /** Stdin-TTY-only preference for launching after installation. */
  promptLaunch: () => Promise<boolean>;
}

function defaultPackageRoot(): string {
  // Both src/commands/bar and dist/commands/bar have the same depth.
  return path.resolve(__dirname, '../../..');
}

function defaultRunCommand(
  file: string,
  args: string[],
  options: InstallCommandOptions
): Promise<void> {
  return new Promise((resolve, reject) => {
    const child = spawn(file, args, { cwd: options.cwd, stdio: 'inherit', shell: false });
    child.once('error', () =>
      reject(new BinaryError(`Could not run ${path.basename(file)}.`, file))
    );
    child.once('close', (code, signal) => {
      if (code === 0) resolve();
      else
        reject(
          new BinaryError(`${path.basename(file)} failed (${signal ?? code ?? 'unknown'}).`, file)
        );
    });
  });
}

function defaultReadAppBundleVersion(appPath: string): string | null {
  try {
    return inspectOwnedMacApp(appPath)?.version ?? null;
  } catch {
    return null;
  }
}

async function defaultPromptLaunch(): Promise<boolean> {
  if (!process.stdin.isTTY) return false;
  const readline = await import('readline');
  const reader = readline.createInterface({ input: process.stdin, output: process.stdout });
  return new Promise((resolve) => {
    reader.question('Launch AI Account Center now? [Y/n] ', (answer) => {
      reader.close();
      const value = answer.trim().toLowerCase();
      resolve(value === '' || value === 'y' || value === 'yes');
    });
  });
}

function validateSourceEntry(entry: string): void {
  const stat = fs.lstatSync(entry);
  if (stat.isSymbolicLink() || (!stat.isDirectory() && !stat.isFile())) {
    throw new BinaryError(
      'Packaged native source contains a symlink or unsupported file. Reinstall this CLI.'
    );
  }
  if (stat.isDirectory()) {
    for (const child of fs.readdirSync(entry)) validateSourceEntry(path.join(entry, child));
  }
}

function validatePackagedMacSource(source: string): void {
  if (!fs.existsSync(source) || !fs.lstatSync(source).isDirectory()) {
    throw new BinaryError('Packaged macOS app source is missing. Reinstall this CLI.', source);
  }
  for (const required of MAC_REQUIRED_FILES) {
    const file = path.join(source, required);
    if (!fs.existsSync(file) || !fs.lstatSync(file).isFile()) {
      throw new BinaryError(
        `Packaged macOS app source is incomplete: ${required}. Reinstall this CLI.`,
        file
      );
    }
  }
  if (
    !fs.existsSync(path.join(source, 'Sources')) ||
    !fs.lstatSync(path.join(source, 'Sources')).isDirectory()
  ) {
    throw new BinaryError(
      'Packaged macOS app source is incomplete: Sources. Reinstall this CLI.',
      source
    );
  }
  for (const entry of MAC_SOURCE_PATHS) validateSourceEntry(path.join(source, entry));
}

function pinInstalledVersion(ccsDir: string, version: string | null): void {
  if (version === null) {
    console.log('[!] The installed app version could not be read.');
    return;
  }
  try {
    const versionPath = path.join(ccsDir, 'bar', '.version');
    const directory = path.dirname(versionPath);
    fs.mkdirSync(directory, { recursive: true });
    if (!fs.lstatSync(directory).isDirectory()) throw new BinaryError('Unsafe version directory.');
    try {
      if (!fs.lstatSync(versionPath).isFile()) throw new BinaryError('Unsafe version metadata.');
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
    const temporary = path.join(
      directory,
      `.version-${process.pid}-${Math.random().toString(16).slice(2)}.tmp`
    );
    try {
      fs.writeFileSync(temporary, version, { flag: 'wx', mode: 0o600 });
      fs.renameSync(temporary, versionPath);
    } finally {
      fs.rmSync(temporary, { force: true });
    }
  } catch {
    console.log('[!] The app was installed, but its version metadata could not be saved.');
  }
}

export async function handleBarInstall(
  args: string[],
  deps: Partial<InstallDeps> = {}
): Promise<void> {
  const unknown = args.find((arg) => !['--launch', '--no-launch', '--await-quit'].includes(arg));
  const duplicate = args.find((arg, index) => args.indexOf(arg) !== index);
  if (unknown || duplicate) {
    console.error(
      `[X] ${unknown ? `Unknown option: ${unknown}` : `Duplicate option: ${duplicate}`}`
    );
    process.exitCode = 1;
    return;
  }
  const forceLaunch = hasAnyFlag(args, ['--launch']);
  const noLaunch = hasAnyFlag(args, ['--no-launch']);
  if (forceLaunch && noLaunch) {
    console.error('[X] Use either --launch or --no-launch.');
    process.exitCode = 1;
    return;
  }
  const platform = (deps.getPlatform ?? (() => process.platform))();
  if (platform !== 'darwin') {
    console.error('[X] This native bar installer requires macOS 14+ and Swift Command Line Tools.');
    if (platform === 'win32') {
      console.error(
        '[i] Use the packaged windows-bar/scripts/Build.ps1 and Install.ps1 for the Windows app.'
      );
    }
    process.exitCode = 1;
    return;
  }
  if (hasAnyFlag(args, ['--await-quit'])) {
    console.error(
      '[X] --await-quit is obsolete. The owned installer safely stops only this app after verifying the new build.'
    );
    process.exitCode = 1;
    return;
  }

  const root = (deps.getPackageRoot ?? defaultPackageRoot)();
  const source = path.join(root, 'macos-bar');
  const runCommand = deps.runCommand ?? defaultRunCommand;
  let workspace: string | undefined;
  try {
    validatePackagedMacSource(source);
    // Build outside the npm package; global installations may be read-only.
    // Copy only source/resources, excluding dist, build output, and local state.
    workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'ai-account-center-native-build-'));
    const buildSource = path.join(workspace, 'macos-bar');
    fs.mkdirSync(buildSource);
    for (const entry of MAC_SOURCE_PATHS) {
      fs.cpSync(path.join(source, entry), path.join(buildSource, entry), { recursive: true });
    }
    console.log('[i] Building AI Account Center from the native source packaged with this CLI...');
    await runCommand('/bin/bash', [path.join(buildSource, 'Scripts', 'package_app.sh')], {
      cwd: buildSource,
    });
    const shouldLaunch =
      forceLaunch || (!noLaunch && (await (deps.promptLaunch ?? defaultPromptLaunch)()));
    const installArgs = [path.join(buildSource, 'Scripts', 'install_user.sh')];
    if (shouldLaunch) installArgs.push('--launch');
    // Native installer owns signature validation, process matching, backups,
    // rollback, startup preferences, and the legacy CCS Bar.app alias.
    await runCommand('/bin/bash', installArgs, { cwd: buildSource });
    const appPath = path.join(
      (deps.getAppsDir ?? (() => path.join(os.homedir(), 'Applications')))(),
      APP_NAME
    );
    const installedVersion = (deps.readAppBundleVersion ?? defaultReadAppBundleVersion)(appPath);
    pinInstalledVersion((deps.getCcsDir ?? getCcsDir)(), installedVersion);
    console.log(
      `[OK] AI Account Center${installedVersion ? ` v${installedVersion}` : ''} installed.`
    );
    if (!shouldLaunch) console.log('[i] Run `ai-account-center bar` to launch later.');
  } catch (error) {
    const message = error instanceof Error ? error.message : 'Native installation failed.';
    console.error(`[X] ${message}`);
    console.error(
      '[i] The native installer preserves connection settings and backs up existing owned apps.'
    );
    process.exitCode = 1;
  } finally {
    if (workspace !== undefined) fs.rmSync(workspace, { recursive: true, force: true });
  }
}

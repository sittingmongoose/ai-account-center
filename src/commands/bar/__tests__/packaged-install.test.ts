import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  handleBarInstall,
  type InstallDeps,
  type InstallCommandOptions,
} from '../install-subcommand';

interface CommandCall {
  file: string;
  args: string[];
  options: InstallCommandOptions;
}

let temporary: string;
let packageRoot: string;
let source: string;
let ccsDir: string;
let appsDir: string;
let calls: CommandCall[];
let output: string[];
let prompts: number;
let versionReads: string[];
let originalLog: typeof console.log;
let originalError: typeof console.error;
let originalExitCode: typeof process.exitCode;

function write(file: string, value: string): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, value);
}

function deps(overrides: Partial<InstallDeps> = {}): InstallDeps {
  return {
    getPlatform: () => 'darwin',
    getPackageRoot: () => packageRoot,
    runCommand: async (file, args, options) => {
      calls.push({ file, args: [...args], options: { ...options } });
    },
    getCcsDir: () => ccsDir,
    getAppsDir: () => appsDir,
    readAppBundleVersion: (appPath) => {
      versionReads.push(appPath);
      return '0.4.0';
    },
    promptLaunch: async () => {
      prompts += 1;
      return false;
    },
    ...overrides,
  };
}

describe('packaged installer fixtures', () => {
  beforeEach(() => {
    temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'packaged-bar-install-test-'));
    // Shell metacharacters in source paths must stay inert argument data.
    packageRoot = path.join(temporary, 'owned package $() `literal`');
    source = path.join(packageRoot, 'macos-bar');
    ccsDir = path.join(temporary, 'private-ccs');
    appsDir = path.join(temporary, 'Applications');
    for (const [file, contents] of Object.entries({
      'Package.swift': '// offline fixture',
      VERSION: '0.4.0',
      LICENSE: 'MIT fixture',
      'Sources/CCSBarApp/App.swift': '// fixture app',
      'Resources/Info.plist': '<plist>fixture</plist>',
      'Resources/icon.png': 'fixture icon',
      'Scripts/package_app.sh': '# fixture build',
      'Scripts/install_user.sh': '# fixture installer',
      'Scripts/install_bundle.py': '# fixture guarded installer',
    })) {
      write(path.join(source, file), contents);
    }
    write(path.join(source, 'dist/do-not-copy.txt'), 'old build output');
    write(path.join(source, '.build/do-not-copy.txt'), 'old Swift output');
    write(path.join(source, '.ccs/do-not-copy.txt'), 'fixture private state');
    write(path.join(ccsDir, 'bar/accounts-connection.json'), 'offline connection fixture');
    write(path.join(ccsDir, 'codex-auto-switch.json'), '{"enabled":true}');
    calls = [];
    output = [];
    prompts = 0;
    versionReads = [];
    originalLog = console.log;
    originalError = console.error;
    originalExitCode = process.exitCode;
    console.log = (...args: unknown[]) => output.push(args.map(String).join(' '));
    console.error = (...args: unknown[]) => output.push(args.map(String).join(' '));
    process.exitCode = 0;
  });

  afterEach(() => {
    console.log = originalLog;
    console.error = originalError;
    process.exitCode = originalExitCode;
    fs.rmSync(temporary, { recursive: true, force: true });
  });

  describe('owned packaged native installation', () => {
    it('builds a private source copy and invokes only the guarded owned Mac installer', async () => {
      const originalConnection = fs.readFileSync(path.join(ccsDir, 'bar/accounts-connection.json'));
      const originalAutomatic = fs.readFileSync(path.join(ccsDir, 'codex-auto-switch.json'));
      let buildSource: string | undefined;
      await handleBarInstall(
        ['--no-launch'],
        deps({
          runCommand: async (file, args, options) => {
            calls.push({ file, args: [...args], options: { ...options } });
            buildSource = options.cwd;
            expect(file).toBe('/bin/bash');
            expect(options.cwd).not.toBe(source);
            expect(path.basename(options.cwd)).toBe('macos-bar');
            expect(args[0]).toBe(
              path.join(
                options.cwd,
                'Scripts',
                calls.length === 1 ? 'package_app.sh' : 'install_user.sh'
              )
            );
            expect(
              fs.readFileSync(path.join(options.cwd, 'Sources/CCSBarApp/App.swift'), 'utf8')
            ).toBe('// fixture app');
            expect(fs.readFileSync(path.join(options.cwd, 'Resources/icon.png'), 'utf8')).toBe(
              'fixture icon'
            );
            expect(fs.existsSync(path.join(options.cwd, 'dist'))).toBe(false);
            expect(fs.existsSync(path.join(options.cwd, '.build'))).toBe(false);
            expect(fs.existsSync(path.join(options.cwd, '.ccs'))).toBe(false);
          },
        })
      );
      expect(calls).toHaveLength(2);
      expect(calls[1].args).toHaveLength(1);
      expect(prompts).toBe(0);
      expect(versionReads).toEqual([path.join(appsDir, 'AI Account Center.app')]);
      expect(fs.readFileSync(path.join(ccsDir, 'bar/.version'), 'utf8')).toBe('0.4.0');
      expect(fs.readFileSync(path.join(ccsDir, 'bar/accounts-connection.json'))).toEqual(
        originalConnection
      );
      expect(fs.readFileSync(path.join(ccsDir, 'codex-auto-switch.json'))).toEqual(
        originalAutomatic
      );
      expect(fs.existsSync(path.join(ccsDir, 'bar/launch.json'))).toBe(false);
      expect(fs.readFileSync(path.join(source, 'dist/do-not-copy.txt'), 'utf8')).toBe(
        'old build output'
      );
      expect(fs.existsSync(path.dirname(buildSource!))).toBe(false);
      expect(process.exitCode).toBe(0);
    });

    it('never follows a version-pin alias into saved connection data', async () => {
      const connection = path.join(ccsDir, 'bar/accounts-connection.json');
      const before = fs.readFileSync(connection);
      const pin = path.join(ccsDir, 'bar/.version');
      fs.symlinkSync(connection, pin);
      await handleBarInstall(['--no-launch'], deps());
      expect(calls).toHaveLength(2);
      expect(fs.readFileSync(connection)).toEqual(before);
      expect(fs.lstatSync(pin).isSymbolicLink()).toBe(true);
      expect(output.some((line) => line.includes('version metadata could not be saved'))).toBe(
        true
      );
      expect(process.exitCode).toBe(0);
    });

    it('passes explicit --launch only to the native installer and bypasses the prompt', async () => {
      await handleBarInstall(['--launch'], deps());
      expect(calls).toHaveLength(2);
      expect(calls[0].args).toHaveLength(1);
      expect(calls[1].args.slice(1)).toEqual(['--launch']);
      expect(prompts).toBe(0);
    });

    it('uses a confirmed launch preference after a successful build', async () => {
      await handleBarInstall(
        [],
        deps({
          promptLaunch: async () => {
            prompts += 1;
            expect(calls).toHaveLength(1);
            return true;
          },
        })
      );
      expect(prompts).toBe(1);
      expect(calls[1].args.slice(1)).toEqual(['--launch']);
    });

    it('keeps declined or noninteractive launch disabled and shows launch guidance', async () => {
      await handleBarInstall([], deps());
      expect(prompts).toBe(1);
      expect(calls[1].args).toHaveLength(1);
      expect(
        output.some((line) => line.includes('ai-account-center bar') && line.includes('later'))
      ).toBe(true);
    });

    for (const platform of ['linux', 'win32'] as const) {
      it(`rejects ${platform} before package reads, processes, or launch prompts`, async () => {
        await handleBarInstall(
          [],
          deps({
            getPlatform: () => platform,
            getPackageRoot: () => {
              throw new Error('Unexpected package read');
            },
            runCommand: async () => {
              throw new Error('Unexpected process');
            },
            promptLaunch: async () => {
              throw new Error('Unexpected prompt');
            },
          })
        );
        expect(process.exitCode).toBe(1);
        expect(calls).toEqual([]);
        expect(versionReads).toEqual([]);
        if (platform === 'win32')
          expect(output.some((line) => line.includes('windows-bar/scripts/Build.ps1'))).toBe(true);
      });
    }

    for (const args of [
      ['--launch', '--no-launch'],
      ['--await-quit'],
      ['--obsolete-option'],
      ['--launch', '--launch'],
    ]) {
      it(`rejects unsupported or contradictory flags ${args.join(' ')} without actions`, async () => {
        await handleBarInstall(args, deps());
        expect(process.exitCode).toBe(1);
        expect(calls).toEqual([]);
        expect(prompts).toBe(0);
        expect(versionReads).toEqual([]);
      });
    }

    for (const missing of [
      'VERSION',
      'Resources/Info.plist',
      'Scripts/install_bundle.py',
      'Sources',
    ]) {
      it(`rejects incomplete packaged source (${missing}) before invoking a build`, async () => {
        fs.rmSync(path.join(source, missing), { recursive: true, force: true });
        await handleBarInstall([], deps());
        expect(process.exitCode).toBe(1);
        expect(calls).toEqual([]);
        expect(prompts).toBe(0);
      });
    }

    it('rejects a bundled installer symlink instead of executing an external script', async () => {
      const installer = path.join(source, 'Scripts/install_user.sh');
      fs.unlinkSync(installer);
      const outside = path.join(temporary, 'foreign-installer.sh');
      write(outside, '# must never execute');
      fs.symlinkSync(outside, installer);
      await handleBarInstall([], deps());
      expect(process.exitCode).toBe(1);
      expect(calls).toEqual([]);
      expect(fs.readFileSync(outside, 'utf8')).toBe('# must never execute');
    });

    it('rejects nested source symlink escapes and an aliased source root', async () => {
      const outside = path.join(temporary, 'foreign-source.swift');
      write(outside, '// unrelated');
      fs.symlinkSync(outside, path.join(source, 'Sources/CCSBarApp/foreign.swift'));
      await handleBarInstall([], deps());
      expect(process.exitCode).toBe(1);
      expect(calls).toEqual([]);
      fs.unlinkSync(path.join(source, 'Sources/CCSBarApp/foreign.swift'));
      const realSource = path.join(packageRoot, 'real-native-source');
      fs.renameSync(source, realSource);
      fs.symlinkSync(realSource, source, 'dir');
      await handleBarInstall([], deps());
      expect(calls).toEqual([]);
      expect(fs.readFileSync(outside, 'utf8')).toBe('// unrelated');
    });

    it('does not install, prompt, pin a version, or alter existing app files after build failure', async () => {
      write(path.join(appsDir, 'AI Account Center.app/old-marker'), 'existing owned app');
      write(path.join(ccsDir, 'bar/.version'), 'previous-version');
      await handleBarInstall(
        [],
        deps({
          runCommand: async (file, args, options) => {
            calls.push({ file, args, options });
            throw new Error('Mock Swift build failure');
          },
        })
      );
      expect(calls).toHaveLength(1);
      expect(prompts).toBe(0);
      expect(versionReads).toEqual([]);
      expect(fs.readFileSync(path.join(ccsDir, 'bar/.version'), 'utf8')).toBe('previous-version');
      expect(fs.readFileSync(path.join(appsDir, 'AI Account Center.app/old-marker'), 'utf8')).toBe(
        'existing owned app'
      );
      expect(fs.existsSync(path.dirname(calls[0].options.cwd))).toBe(false);
      expect(process.exitCode).toBe(1);
    });

    it('delegates rollback to the native installer and saves no success metadata when it fails', async () => {
      write(path.join(ccsDir, 'bar/.version'), 'previous-version');
      await handleBarInstall(
        ['--launch'],
        deps({
          runCommand: async (file, args, options) => {
            calls.push({ file, args, options });
            if (calls.length === 2) throw new Error('Mock owned installer rejected foreign app');
          },
        })
      );
      expect(calls).toHaveLength(2);
      expect(versionReads).toEqual([]);
      expect(fs.readFileSync(path.join(ccsDir, 'bar/.version'), 'utf8')).toBe('previous-version');
      expect(fs.existsSync(path.dirname(calls[1].options.cwd))).toBe(false);
      expect(output.some((line) => line.startsWith('[OK]'))).toBe(false);
      expect(process.exitCode).toBe(1);
    });

    it('reads version only from the installed canonical app with the owned bundle identity', async () => {
      const info = path.join(appsDir, 'AI Account Center.app/Contents/Info.plist');
      write(
        info,
        '<plist><dict><key>CFBundleIdentifier</key><string>party.sittingmongoose.ccs.accounts-bar</string>' +
          '<key>CFBundleExecutable</key><string>CCSBar</string>' +
          '<key>CFBundleShortVersionString</key><string>0.5.1</string></dict></plist>'
      );
      const executable = path.join(appsDir, 'AI Account Center.app/Contents/MacOS/CCSBar');
      write(executable, 'offline native executable fixture');
      fs.chmodSync(executable, 0o700);
      const provided = deps();
      delete (provided as Partial<InstallDeps>).readAppBundleVersion;
      await handleBarInstall(['--no-launch'], provided);
      expect(fs.readFileSync(path.join(ccsDir, 'bar/.version'), 'utf8')).toBe('0.5.1');
      fs.rmSync(path.join(ccsDir, 'bar/.version'));
      write(
        info,
        '<plist><dict><key>CFBundleIdentifier</key><string>example.foreign</string>' +
          '<key>CFBundleShortVersionString</key><string>9.9.9</string></dict></plist>'
      );
      await handleBarInstall(['--no-launch'], provided);
      expect(fs.existsSync(path.join(ccsDir, 'bar/.version'))).toBe(false);
      expect(output.some((line) => line.includes('version could not be read'))).toBe(true);
    });
  });
});

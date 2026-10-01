import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  getMacAppPaths,
  inspectOwnedMacApp,
  MAC_APP_NAME,
  MAC_BUNDLE_ID,
  MAC_EXECUTABLE,
  MAC_LEGACY_APP_NAME,
  PRODUCT_NAME,
  readMacPlist,
  resolveOwnedMacApp,
} from '../native-app-paths';
import { handleBarUninstall } from '../uninstall-subcommand';
import type { UninstallDeps } from '../uninstall-subcommand';
import type { NativeCommandRunner } from '../native-app-paths';

let home: string;
let appsDir: string;
let ccsDir: string;
let pin: string;
let agent: string;
let backups: string;
let output: string[];
let originalLog: typeof console.log;
let originalError: typeof console.error;
let originalExitCode: number | undefined;

function xml(document: string): string {
  return `<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict>${document}</dict></plist>`;
}

function bundle(name = MAC_APP_NAME, identity = MAC_BUNDLE_ID, version = '2.1.0'): string {
  const app = path.join(appsDir, name);
  const contents = path.join(app, 'Contents');
  fs.mkdirSync(path.join(contents, 'MacOS'), { recursive: true });
  fs.writeFileSync(
    path.join(contents, 'Info.plist'),
    xml(
      `<key>CFBundleIdentifier</key><string>${identity}</string>` +
        `<key>CFBundleExecutable</key><string>${MAC_EXECUTABLE}</string>` +
        `<key>CFBundleShortVersionString</key><string>${version}</string>` +
        '<key>NSAppTransportSecurity</key><dict><key>NSAllowsLocalNetworking</key><true/></dict>'
    )
  );
  fs.writeFileSync(path.join(contents, 'MacOS', MAC_EXECUTABLE), 'fixture executable only\n', {
    mode: 0o755,
  });
  return app;
}

function writeAgent(
  executable = path.join(appsDir, MAC_APP_NAME, 'Contents', 'MacOS', MAC_EXECUTABLE),
  label = MAC_BUNDLE_ID
): void {
  fs.mkdirSync(path.dirname(agent), { recursive: true });
  fs.writeFileSync(
    agent,
    xml(
      `<key>Label</key><string>${label}</string>` +
        `<key>ProgramArguments</key><array><string>${executable}</string></array><key>RunAtLoad</key><true/>`
    )
  );
}

const noNativeCommands: NativeCommandRunner = () => {
  throw new Error('Unexpected native command in isolated fixture');
};

function uninstallDeps(overrides: Partial<UninstallDeps> = {}): Partial<UninstallDeps> {
  return {
    getCcsDir: () => ccsDir,
    getAppsDir: () => appsDir,
    home,
    platform: 'darwin',
    runner: noNativeCommands,
    listAppProcesses: () => [],
    readProcessIdentity: () => 'fixture-birth',
    disableLaunchAgent: () => false,
    restoreLaunchAgent: () => {},
    ...overrides,
  };
}

describe('native app ownership fixtures', () => {
  beforeEach(() => {
    home = fs.mkdtempSync(path.join(os.tmpdir(), 'account-center-native-paths-'));
    appsDir = path.join(home, 'Applications');
    ccsDir = path.join(home, '.ccs');
    pin = path.join(ccsDir, 'bar', '.version');
    agent = path.join(home, 'Library', 'LaunchAgents', `${MAC_BUNDLE_ID}.plist`);
    backups = path.join(home, 'Library', 'Application Support', 'CCS Bar', 'Backups');
    fs.mkdirSync(appsDir);
    fs.mkdirSync(path.dirname(pin), { recursive: true });
    fs.writeFileSync(pin, '2.1.0\n');
    output = [];
    originalLog = console.log;
    originalError = console.error;
    originalExitCode = process.exitCode;
    console.log = (...args) => {
      output.push(args.map(String).join(' '));
    };
    console.error = (...args) => {
      output.push(args.map(String).join(' '));
    };
    process.exitCode = 0;
  });

  afterEach(() => {
    console.log = originalLog;
    console.error = originalError;
    process.exitCode = originalExitCode;
    fs.rmSync(home, { recursive: true, force: true });
  });

  describe('owned Mac native app paths', () => {
    it('retains compatibility identifiers while preferring the branded owned bundle', () => {
      expect(PRODUCT_NAME).toBe('AI Account Center');
      expect(MAC_EXECUTABLE).toBe('CCSBar');
      expect(MAC_BUNDLE_ID).toBe('party.sittingmongoose.ccs.accounts-bar');
      const canonical = bundle();
      bundle(MAC_LEGACY_APP_NAME, MAC_BUNDLE_ID, '1.9.0');
      expect(getMacAppPaths(home)).toEqual({
        appsDir,
        canonical,
        legacy: path.join(appsDir, MAC_LEGACY_APP_NAME),
      });
      expect(resolveOwnedMacApp(appsDir)).toEqual({ path: canonical, version: '2.1.0' });
    });

    it('supports appsDir overrides and an owned real legacy fallback', () => {
      const legacy = bundle(MAC_LEGACY_APP_NAME);
      expect(getMacAppPaths('/unused', appsDir).appsDir).toBe(appsDir);
      expect(resolveOwnedMacApp(appsDir)).toEqual({ path: legacy, version: '2.1.0' });
    });

    it('accepts only the exact relative legacy alias of the owned canonical bundle', () => {
      const canonical = bundle();
      fs.symlinkSync(MAC_APP_NAME, path.join(appsDir, MAC_LEGACY_APP_NAME));
      expect(resolveOwnedMacApp(appsDir)?.path).toBe(canonical);
      fs.unlinkSync(path.join(appsDir, MAC_LEGACY_APP_NAME));
      fs.symlinkSync(`./${MAC_APP_NAME}`, path.join(appsDir, MAC_LEGACY_APP_NAME));
      expect(() => resolveOwnedMacApp(appsDir)).toThrow('foreign native app alias');
    });

    it('rejects foreign canonical and legacy bundles without falling back', () => {
      const canonical = bundle(MAC_APP_NAME, 'example.foreign');
      const legacy = bundle(MAC_LEGACY_APP_NAME);
      expect(() => resolveOwnedMacApp(appsDir)).toThrow('foreign native app bundle');
      fs.rmSync(canonical, { recursive: true });
      bundle();
      fs.rmSync(legacy, { recursive: true });
      bundle(MAC_LEGACY_APP_NAME, 'example.foreign');
      expect(() => resolveOwnedMacApp(appsDir)).toThrow('foreign native app bundle');
    });

    it('rejects canonical symlinks, dangling legacy aliases, and executable symlinks', () => {
      const other = bundle('Other.app');
      const canonical = path.join(appsDir, MAC_APP_NAME);
      fs.symlinkSync(other, canonical);
      expect(() => resolveOwnedMacApp(appsDir)).toThrow('unexpected native app path');
      fs.unlinkSync(canonical);
      const legacy = path.join(appsDir, MAC_LEGACY_APP_NAME);
      fs.symlinkSync(MAC_APP_NAME, legacy);
      expect(() => resolveOwnedMacApp(appsDir)).toThrow('foreign native app alias');
      fs.unlinkSync(legacy);
      bundle();
      const executable = path.join(canonical, 'Contents', 'MacOS', MAC_EXECUTABLE);
      fs.unlinkSync(executable);
      fs.symlinkSync(path.join(other, 'Contents', 'MacOS', MAC_EXECUTABLE), executable);
      expect(() => inspectOwnedMacApp(canonical)).toThrow('unexpected native app path');
    });

    it('requires an executable regular file and reads only top-level identity keys', () => {
      const app = bundle();
      const executable = path.join(app, 'Contents', 'MacOS', MAC_EXECUTABLE);
      fs.chmodSync(executable, 0o600);
      expect(() => inspectOwnedMacApp(app)).toThrow('not executable');
      fs.chmodSync(executable, 0o755);
      fs.writeFileSync(
        path.join(app, 'Contents', 'Info.plist'),
        xml(
          `<key>Nested</key><dict><key>CFBundleIdentifier</key><string>${MAC_BUNDLE_ID}</string></dict>` +
            `<key>CFBundleExecutable</key><string>${MAC_EXECUTABLE}</string>`
        )
      );
      expect(() => inspectOwnedMacApp(app)).toThrow('foreign native app bundle');
    });

    it('rejects duplicate plist keys and external entity expansions', () => {
      const app = bundle();
      const info = path.join(app, 'Contents', 'Info.plist');
      fs.writeFileSync(
        info,
        xml(
          '<key>CFBundleIdentifier</key><string>a</string><key>CFBundleIdentifier</key><string>b</string>'
        )
      );
      expect(() => readMacPlist(info)).toThrow('plist structure');
      fs.writeFileSync(info, xml('<key>CFBundleIdentifier</key><string>&external;</string>'));
      expect(() => readMacPlist(info)).toThrow('plist text');
    });

    it('reads binary plists through an injected argument-array runner', () => {
      const app = bundle();
      const info = path.join(app, 'Contents', 'Info.plist');
      fs.writeFileSync(info, 'bplist00fixture');
      const calls: [string, string[]][] = [];
      const runner: NativeCommandRunner = (command, args) => {
        calls.push([command, args]);
        return {
          status: 0,
          stdout: JSON.stringify({
            CFBundleIdentifier: MAC_BUNDLE_ID,
            CFBundleExecutable: MAC_EXECUTABLE,
            CFBundleVersion: '17',
          }),
          stderr: '',
        };
      };
      expect(resolveOwnedMacApp(appsDir, { runner })).toEqual({ path: app, version: '17' });
      expect(calls).toEqual([['/usr/bin/plutil', ['-convert', 'json', '-o', '-', info]]]);
    });

    it('returns null for absent bundles and preserves unknown versions as unknown', () => {
      expect(resolveOwnedMacApp(appsDir)).toBeNull();
      const app = bundle();
      const info = path.join(app, 'Contents', 'Info.plist');
      fs.writeFileSync(
        info,
        xml(
          `<key>CFBundleIdentifier</key><string>${MAC_BUNDLE_ID}</string><key>CFBundleExecutable</key><string>${MAC_EXECUTABLE}</string>`
        )
      );
      expect(resolveOwnedMacApp(appsDir)).toEqual({ path: app, version: null });
    });
  });

  describe('reversible owned native app uninstall', () => {
    it('backs up canonical app, verified alias, matching agent and pin while preserving account state', async () => {
      const app = bundle();
      const legacy = path.join(appsDir, MAC_LEGACY_APP_NAME);
      fs.symlinkSync(MAC_APP_NAME, legacy);
      writeAgent();
      const config = path.join(ccsDir, 'bar', 'accounts-connection.json');
      const auth = path.join(ccsDir, 'fixture-auth.json');
      fs.writeFileSync(config, 'synthetic connection fixture');
      fs.writeFileSync(auth, 'synthetic auth fixture');
      await handleBarUninstall([], uninstallDeps());
      expect(process.exitCode).toBe(0);
      expect(fs.existsSync(app)).toBe(false);
      expect(fs.existsSync(legacy)).toBe(false);
      expect(fs.existsSync(agent)).toBe(false);
      expect(fs.existsSync(pin)).toBe(false);
      const saved = path.join(backups, fs.readdirSync(backups)[0]);
      expect(
        fs.existsSync(path.join(saved, MAC_APP_NAME, 'Contents', 'MacOS', MAC_EXECUTABLE))
      ).toBe(true);
      expect(fs.readlinkSync(path.join(saved, MAC_LEGACY_APP_NAME))).toBe(MAC_APP_NAME);
      expect(fs.readFileSync(path.join(saved, 'version-pin'), 'utf8')).toBe('2.1.0\n');
      expect(fs.readFileSync(config, 'utf8')).toBe('synthetic connection fixture');
      expect(fs.readFileSync(auth, 'utf8')).toBe('synthetic auth fixture');
      expect(fs.statSync(saved).mode & 0o777).toBe(0o700);
    });

    it('preserves legacy DI names with real owned temporary fixtures', async () => {
      const legacy = bundle(MAC_LEGACY_APP_NAME);
      await handleBarUninstall([], uninstallDeps({ appName: MAC_LEGACY_APP_NAME }));
      expect(process.exitCode).toBe(0);
      expect(fs.existsSync(legacy)).toBe(false);
      expect(fs.existsSync(pin)).toBe(false);
    });

    it('preflights every occupied app path before moving the app or clearing the pin', async () => {
      const app = bundle();
      bundle(MAC_LEGACY_APP_NAME, 'example.foreign');
      let moved = false;
      await handleBarUninstall(
        [],
        uninstallDeps({
          movePath: () => {
            moved = true;
          },
        })
      );
      expect(process.exitCode).toBe(1);
      expect(moved).toBe(false);
      expect(fs.existsSync(app)).toBe(true);
      expect(fs.readFileSync(pin, 'utf8')).toBe('2.1.0\n');
      expect(fs.existsSync(backups)).toBe(false);
    });

    it('refuses a foreign alias, launch agent, or symlinked version pin without any changes', async () => {
      const app = bundle();
      const legacy = path.join(appsDir, MAC_LEGACY_APP_NAME);
      fs.symlinkSync('../Elsewhere.app', legacy);
      await handleBarUninstall([], uninstallDeps());
      expect(process.exitCode).toBe(1);
      expect(fs.existsSync(app)).toBe(true);
      fs.unlinkSync(legacy);
      writeAgent('/foreign/program');
      process.exitCode = 0;
      await handleBarUninstall([], uninstallDeps());
      expect(process.exitCode).toBe(1);
      expect(fs.existsSync(app)).toBe(true);
      expect(fs.existsSync(agent)).toBe(true);
      fs.unlinkSync(agent);
      const victim = path.join(home, 'foreign-pin');
      fs.writeFileSync(victim, 'keep');
      fs.unlinkSync(pin);
      fs.symlinkSync(victim, pin);
      process.exitCode = 0;
      await handleBarUninstall([], uninstallDeps());
      expect(process.exitCode).toBe(1);
      expect(fs.readFileSync(victim, 'utf8')).toBe('keep');
      expect(fs.lstatSync(pin).isSymbolicLink()).toBe(true);
    });

    it('refuses running or unverifiable app processes without signaling or moving anything', async () => {
      const app = bundle();
      writeAgent();
      let disabled = false;
      let moved = false;
      for (const identity of ['fixture-birth', null]) {
        process.exitCode = 0;
        await handleBarUninstall(
          [],
          uninstallDeps({
            listAppProcesses: () => [
              { pid: 123, executable: path.join(app, 'Contents', 'MacOS', MAC_EXECUTABLE) },
            ],
            readProcessIdentity: () => identity,
            disableLaunchAgent: () => {
              disabled = true;
              return true;
            },
            movePath: () => {
              moved = true;
            },
          })
        );
        expect(process.exitCode).toBe(1);
      }
      expect(disabled).toBe(false);
      expect(moved).toBe(false);
      expect(fs.existsSync(app)).toBe(true);
      expect(fs.existsSync(pin)).toBe(true);
    });

    it('rolls back an interrupted move and restores the previously loaded matching agent', async () => {
      const app = bundle();
      const legacy = path.join(appsDir, MAC_LEGACY_APP_NAME);
      fs.symlinkSync(MAC_APP_NAME, legacy);
      writeAgent();
      let count = 0;
      const restored: string[] = [];
      await handleBarUninstall(
        [],
        uninstallDeps({
          disableLaunchAgent: () => true,
          restoreLaunchAgent: (value) => {
            restored.push(value);
          },
          movePath: (source, destination) => {
            if (++count === 3) throw new Error('fixture move failure');
            fs.renameSync(source, destination);
          },
        })
      );
      expect(process.exitCode).toBe(1);
      expect(fs.existsSync(app)).toBe(true);
      expect(fs.readlinkSync(legacy)).toBe(MAC_APP_NAME);
      expect(fs.existsSync(agent)).toBe(true);
      expect(fs.existsSync(pin)).toBe(true);
      expect(restored).toEqual([agent]);
      expect(fs.readdirSync(backups)).toEqual([]);
    });

    it('rechecks ownership after process preflight before moving any bundle', async () => {
      const app = bundle();
      let checks = 0;
      await handleBarUninstall(
        [],
        uninstallDeps({
          listAppProcesses: () => {
            if (++checks === 2)
              fs.writeFileSync(
                path.join(app, 'Contents', 'Info.plist'),
                xml('<key>CFBundleIdentifier</key><string>foreign</string>')
              );
            return [];
          },
        })
      );
      expect(process.exitCode).toBe(1);
      expect(fs.existsSync(app)).toBe(true);
      expect(fs.existsSync(pin)).toBe(true);
      expect(fs.readdirSync(backups)).toEqual([]);
    });

    it('preserves the original backup when a replacement prevents rollback', async () => {
      const app = bundle();
      fs.symlinkSync(MAC_APP_NAME, path.join(appsDir, MAC_LEGACY_APP_NAME));
      let moves = 0;
      await handleBarUninstall(
        [],
        uninstallDeps({
          movePath: (source, destination) => {
            if (++moves === 2) {
              fs.mkdirSync(app);
              fs.writeFileSync(path.join(app, 'foreign-marker'), 'preserve replacement');
              throw new Error('fixture replacement during failed move');
            }
            fs.renameSync(source, destination);
          },
        })
      );
      expect(process.exitCode).toBe(1);
      expect(fs.readFileSync(path.join(app, 'foreign-marker'), 'utf8')).toBe(
        'preserve replacement'
      );
      const saved = path.join(backups, fs.readdirSync(backups)[0]);
      expect(inspectOwnedMacApp(path.join(saved, MAC_APP_NAME))?.version).toBe('2.1.0');
      expect(fs.existsSync(pin)).toBe(true);
      expect(output.join('\n')).toContain('rollback needs recovery');
    });

    it('does not boot out a loaded launch agent whose actual program is foreign', async () => {
      bundle();
      writeAgent();
      const commands: string[][] = [];
      const deps = uninstallDeps({
        runner: (_command, args) => {
          commands.push(args);
          return { status: 0, stdout: 'program = /foreign/program\n', stderr: '' };
        },
      });
      delete deps.disableLaunchAgent;
      await handleBarUninstall([], deps);
      expect(process.exitCode).toBe(1);
      expect(commands.map((args) => args[0])).toEqual(['print']);
      expect(fs.existsSync(agent)).toBe(true);
      expect(fs.existsSync(pin)).toBe(true);
    });

    it('refuses a launch agent with a live PID instead of sending an unverified stop', async () => {
      const app = bundle();
      writeAgent();
      const commands: string[][] = [];
      const deps = uninstallDeps({
        runner: (_command, args) => {
          commands.push(args);
          return {
            status: 0,
            stdout: `program = ${path.join(app, 'Contents', 'MacOS', MAC_EXECUTABLE)}\npid = 123\n`,
            stderr: '',
          };
        },
      });
      delete deps.disableLaunchAgent;
      await handleBarUninstall([], deps);
      expect(process.exitCode).toBe(1);
      expect(commands.map((args) => args[0])).toEqual(['print']);
      expect(fs.existsSync(app)).toBe(true);
      expect(fs.existsSync(agent)).toBe(true);
      expect(fs.existsSync(pin)).toBe(true);
    });

    it('gates unsupported production platforms before filesystem or runner access', async () => {
      let accessed = false;
      await handleBarUninstall([], {
        platform: 'win32',
        getAppsDir: () => {
          accessed = true;
          return appsDir;
        },
        runner: () => {
          accessed = true;
          throw new Error('must not run');
        },
      });
      expect(process.exitCode).toBe(1);
      expect(accessed).toBe(false);
      expect(output.join('\n')).toContain('macOS only');
    });

    it('is a no-op when no owned app, agent, or version pin exists', async () => {
      fs.unlinkSync(pin);
      await handleBarUninstall([], uninstallDeps());
      expect(process.exitCode).toBe(0);
      expect(output.join('\n')).toContain('nothing to remove');
      expect(fs.existsSync(backups)).toBe(false);
    });
  });
});

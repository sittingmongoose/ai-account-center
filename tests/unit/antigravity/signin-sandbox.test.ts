/**
 * The isolated Antigravity sign-in home: staging layout and modes, the exact
 * bubblewrap argv and environment, the preflight and the credential watch.
 * Temporary folders only; nothing here starts the CLI.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  APPARMOR_QUERY_LEAF,
  BWRAP,
  DBUS_RUN_SESSION,
  PRIVATE_SESSION_BUS_CONFIG,
  antigravitySignInPreflight,
  maskedCredentialState,
  prepareSignInStaging,
  removeSignInStaging,
  sandboxArgs,
  sandboxEnvironment,
  sweepSignInStaging,
} from '../../../src/antigravity/signin-sandbox';

let root: string;
let ccsDir: string;
let home: string;

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-sandbox-'));
  ccsDir = path.join(root, '.ccs');
  home = path.join(root, 'home');
  fs.mkdirSync(ccsDir, { mode: 0o700 });
  fs.mkdirSync(home, { mode: 0o700 });
});

afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

const mode = (target: string) => fs.statSync(target).mode & 0o777;

function installAgy(fileMode = 0o755): string {
  const binary = path.join(home, '.local', 'bin', 'agy');
  fs.mkdirSync(path.dirname(binary), { recursive: true });
  fs.writeFileSync(binary, '#!/bin/sh\nexit 0\n');
  fs.chmodSync(binary, fileMode);
  return binary;
}

describe('sign-in staging', () => {
  it('creates owned private folders, browser stubs and the fixed bus config', () => {
    const staging = prepareSignInStaging(ccsDir);
    expect(path.dirname(staging.directory)).toBe(path.join(ccsDir, 'antigravity-signin'));
    expect(path.basename(staging.directory)).toMatch(/^\.staging-[a-f0-9]{16}$/);
    for (const folder of [
      path.join(ccsDir, 'antigravity-signin'),
      staging.directory,
      staging.home,
      staging.mask,
      path.join(staging.mask, 'antigravity-cli'),
      path.join(staging.mask, 'antigravity-cli', 'log'),
      path.join(staging.mask, 'AAC-signin'),
      staging.stubs,
    ])
      expect([folder, mode(folder)]).toEqual([folder, 0o700]);
    expect(staging.mask).toBe(path.join(staging.home, '.gemini'));
    expect(staging.token).toBe(
      path.join(staging.mask, 'antigravity-cli', 'antigravity-oauth-token')
    );
    expect(fs.readdirSync(staging.stubs).sort()).toEqual([
      'open',
      'sensible-browser',
      'www-browser',
      'x-www-browser',
      'xdg-open',
    ]);
    for (const stub of fs.readdirSync(staging.stubs)) {
      expect(fs.readFileSync(path.join(staging.stubs, stub), 'utf8')).toBe('#!/bin/sh\nexit 0\n');
      expect(mode(path.join(staging.stubs, stub))).toBe(0o700);
    }
    expect(fs.readFileSync(staging.busConfig, 'utf8')).toBe(PRIVATE_SESSION_BUS_CONFIG);
    expect(mode(staging.busConfig)).toBe(0o600);
    expect(PRIVATE_SESSION_BUS_CONFIG).not.toContain('servicedir');
    expect(fs.existsSync(staging.token)).toBe(false);
    removeSignInStaging(staging);
    expect(fs.existsSync(staging.directory)).toBe(false);
  });

  it('builds the reviewed sandbox argv: read-only root, the mask over the real .gemini, no CLI arguments', () => {
    const staging = prepareSignInStaging(ccsDir);
    const gemini = path.join(home, '.gemini');
    const args = sandboxArgs(staging, {
      realHome: home,
      nativeBinary: '/fixture/agy',
      apparmorLeaf: APPARMOR_QUERY_LEAF,
    });
    expect(args).toEqual([
      '--die-with-parent',
      '--unshare-user',
      '--unshare-pid',
      '--unshare-ipc',
      '--unshare-uts',
      '--ro-bind',
      '/',
      '/',
      '--bind',
      staging.mask,
      gemini,
      '--bind',
      APPARMOR_QUERY_LEAF,
      APPARMOR_QUERY_LEAF,
      '--tmpfs',
      '/tmp',
      '--tmpfs',
      path.join(gemini, 'antigravity-cli', 'log'),
      '--proc',
      '/proc',
      '--dev',
      '/dev',
      '--chdir',
      path.join(gemini, 'AAC-signin'),
      DBUS_RUN_SESSION,
      '--config-file',
      staging.busConfig,
      '/fixture/agy',
    ]);
    const withoutLeaf = sandboxArgs(staging, {
      realHome: home,
      nativeBinary: '/fixture/agy',
      apparmorLeaf: null,
    });
    expect(withoutLeaf).not.toContain(APPARMOR_QUERY_LEAF);
    expect(withoutLeaf.filter((arg) => arg === '--bind').length).toBe(1);
  });

  it('passes only the allowlisted environment, the real HOME and browser stubs', () => {
    const staging = prepareSignInStaging(ccsDir);
    const env = sandboxEnvironment(staging, {
      realHome: home,
      source: {
        HOME: '/elsewhere',
        PATH: '/attacker/bin:/usr/bin',
        TERM: 'xterm-kitty',
        SSH_CONNECTION: '192.168.50.20 50000 192.168.50.179 22',
        SSH_TTY: '/dev/pts/3',
        GEMINI_API_KEY: 'secret-key',
        GOOGLE_API_KEY: 'secret-key',
        DBUS_SESSION_BUS_ADDRESS: 'unix:path=/run/user/1000/bus',
        DISPLAY: ':0',
        WAYLAND_DISPLAY: 'wayland-0',
        SSH_AUTH_SOCK: '/tmp/agent',
        LANG: 'en_US.UTF-8\nINJECTED=1',
        LC_ALL: 'C.UTF-8',
      },
    });
    expect(env).toEqual({
      HOME: home,
      PATH: `${staging.stubs}:/usr/bin:/bin`,
      BROWSER: path.join(staging.stubs, 'xdg-open'),
      TERM: 'xterm-kitty',
      SSH_CONNECTION: '192.168.50.20 50000 192.168.50.179 22',
      SSH_TTY: '/dev/pts/3',
      LC_ALL: 'C.UTF-8',
    });
    expect(sandboxEnvironment(staging, { realHome: home, source: {} }).TERM).toBe('xterm-256color');
  });

  it('sweeps only staging folders older than an hour', () => {
    const old = prepareSignInStaging(ccsDir);
    const recent = prepareSignInStaging(ccsDir);
    const other = path.join(ccsDir, 'antigravity-signin', 'keep-me');
    fs.mkdirSync(other);
    const aged = new Date(Date.now() - 2 * 60 * 60_000);
    fs.utimesSync(old.directory, aged, aged);
    fs.utimesSync(other, aged, aged);
    expect(sweepSignInStaging(ccsDir)).toBe(1);
    expect(fs.existsSync(old.directory)).toBe(false);
    expect(fs.existsSync(recent.directory)).toBe(true);
    expect(fs.existsSync(other)).toBe(true);
    expect(sweepSignInStaging(path.join(root, 'missing'))).toBe(0);
  });
});

describe('the new credential in the mask', () => {
  it('is absent, partial while incomplete or unsafe, and complete only as a consumer login', () => {
    const staging = prepareSignInStaging(ccsDir);
    expect(maskedCredentialState(staging)).toBe('absent');
    fs.writeFileSync(staging.token, '{"auth_method":"cons', { mode: 0o600 });
    expect(maskedCredentialState(staging)).toBe('partial');
    fs.writeFileSync(
      staging.token,
      JSON.stringify({ auth_method: 'consumer', token: { access_token: 'a' } })
    );
    expect(maskedCredentialState(staging)).toBe('partial');
    fs.writeFileSync(
      staging.token,
      JSON.stringify({ auth_method: 'api-key', token: { access_token: 'a', refresh_token: 'r' } })
    );
    expect(maskedCredentialState(staging)).toBe('partial');
    fs.writeFileSync(
      staging.token,
      JSON.stringify({ auth_method: 'consumer', token: { access_token: 'a', refresh_token: 'r' } })
    );
    expect(maskedCredentialState(staging)).toBe('complete');
    fs.rmSync(staging.token);
    fs.symlinkSync('/etc/hostname', staging.token);
    expect(maskedCredentialState(staging)).toBe('partial');
  });
});

describe('isolation preflight', () => {
  it('passes with the CLI, both tools and a working namespace probe', () => {
    const binary = installAgy();
    let probes = 0;
    const result = antigravitySignInPreflight(home, {
      platform: 'linux',
      exists: () => true,
      probe: () => {
        probes += 1;
        return 0;
      },
    });
    expect(result).toMatchObject({ ok: true, nativeBinary: binary });
    expect(probes).toBe(1);
  });

  it('names tool_missing for a missing or unsafe CLI and preflight_failed for the sandbox', () => {
    const ok = { platform: 'linux' as const, exists: () => true, probe: () => 0 };
    expect(antigravitySignInPreflight(home, ok)).toMatchObject({
      ok: false,
      reason: 'tool_missing',
    });
    installAgy(0o775);
    expect(antigravitySignInPreflight(home, ok)).toMatchObject({
      ok: false,
      reason: 'tool_missing',
      detail: 'The Antigravity CLI is writable by other users.',
    });
    installAgy(0o644);
    expect(antigravitySignInPreflight(home, ok)).toMatchObject({ reason: 'tool_missing' });
    installAgy(0o755);
    expect(
      antigravitySignInPreflight(home, { ...ok, uid: (process.getuid?.() ?? 0) + 1 })
    ).toMatchObject({ reason: 'tool_missing' });
    expect(
      antigravitySignInPreflight(home, { ...ok, exists: (file) => file !== BWRAP })
    ).toMatchObject({ ok: false, reason: 'preflight_failed' });
    expect(antigravitySignInPreflight(home, { ...ok, probe: () => 1 })).toMatchObject({
      ok: false,
      reason: 'preflight_failed',
    });
    expect(antigravitySignInPreflight(home, { ...ok, platform: 'darwin' })).toMatchObject({
      ok: false,
      reason: 'preflight_failed',
    });
  });
});

/**
 * The helper sync sends one flat archive: the source helpers plus the
 * generated Codex runtime. Fixture folders only: nothing here reaches ssh or
 * a real ~/.ccs.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import { spawnSync } from 'child_process';
import { createHash } from 'crypto';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { inflateSync } from 'zlib';
import {
  HELPER_FILES,
  helperPublishScript,
  helperExtract,
  helperHashQuery,
  helperArchiveArgs,
  localHelperChecksums,
} from '../../../src/web-server/services/app-update-hosts';

const RUNTIME = 'app_update_codex_runtime.cjs';
const directories: string[] = [];
afterEach(() => {
  for (const value of directories.splice(0)) fs.rmSync(value, { recursive: true, force: true });
});

function temporary(prefix: string): string {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  directories.push(directory);
  return directory;
}

/** A package root with every helper; the runtime is left out when built is false. */
function fixtureRoot(built = true): string {
  const root = temporary('aac-helper-sync-');
  const source = path.join(root, 'scripts', 'app-updates');
  const generated = path.join(root, 'dist', 'app-updates');
  fs.mkdirSync(source, { recursive: true });
  fs.mkdirSync(generated, { recursive: true });
  for (const name of HELPER_FILES) {
    if (name === RUNTIME && !built) continue;
    fs.writeFileSync(path.join(name === RUNTIME ? generated : source, name), `fixture ${name}\n`);
  }
  return root;
}

describe('remote helper sync', () => {
  it('ships the source helpers and the Codex runtime', () => {
    expect(HELPER_FILES).toHaveLength(13);
    expect(HELPER_FILES).toContain(RUNTIME);
    const source = path.resolve(__dirname, '../../../scripts/app-updates');
    for (const name of HELPER_FILES.filter((value) => value !== RUNTIME))
      expect(fs.existsSync(path.join(source, name))).toBe(true);
    // Generated code never lands in the source folder.
    expect(fs.existsSync(path.join(source, RUNTIME))).toBe(false);
  });

  it('archives every helper flat from both folders and extracts them flat', () => {
    const root = fixtureRoot();
    const args = helperArchiveArgs(root);
    expect(args.filter((value) => value === '-C')).toHaveLength(2);
    const archive = spawnSync('tar', args);
    expect(archive.status).toBe(0);
    const listing = spawnSync('tar', ['-t', '-f', '-'], {
      input: archive.stdout,
      encoding: 'utf8',
    });
    expect(listing.status).toBe(0);
    const names = listing.stdout.split(/\r?\n/).filter(Boolean);
    expect([...names].sort()).toEqual([...HELPER_FILES].sort());

    const home = temporary('aac-helper-home-');
    const script = helperPublishScript(localHelperChecksums(root)).replace(
      'pathlib.Path.home()',
      `pathlib.Path(${JSON.stringify(home)})`
    );
    const extracted = spawnSync(
      process.platform === 'win32' ? 'python' : 'python3',
      ['-c', script],
      { input: archive.stdout }
    );
    expect(extracted.status).toBe(0);
    const state = path.join(home, '.ccs', 'app-updates');
    const generation = fs
      .readFileSync(path.join(state, 'app_updates.py'), 'utf8')
      .split(/\r?\n/)[0]
      .split(' ')
      .at(-1)!;
    const target = path.join(state, generation);
    expect(fs.readdirSync(target).sort()).toEqual([...HELPER_FILES].sort());
    for (const name of HELPER_FILES)
      expect(fs.readFileSync(path.join(target, name), 'utf8')).toBe(`fixture ${name}\n`);
  });

  it('publishes a complete Windows manifest below the cmd.exe command limit', () => {
    const root = fixtureRoot();
    const hashes = localHelperChecksums(root);
    const command = helperExtract('windows', hashes);
    expect(command.length).toBeLessThan(8191);
    const powershell = Buffer.from(command.split(' ').at(-1)!, 'base64').toString('utf16le');
    const match = /^& python\.exe -c '([^']+)' ([A-Za-z0-9+/=]+); exit \$LASTEXITCODE$/.exec(
      powershell
    );
    expect(match).not.toBeNull();
    const [, bootstrap, encoded] = match!;
    expect(inflateSync(Buffer.from(encoded, 'base64')).toString()).toBe(
      helperPublishScript(hashes)
    );

    // Exercise the transmitted bootstrap with a binary archive in an isolated
    // home. Quoting and compression must not consume or alter the tar on stdin.
    const home = temporary('aac-windows-helper-home-');
    const setup = `import pathlib; pathlib.Path.home = classmethod(lambda cls: pathlib.Path(${JSON.stringify(home)})); `;
    const archive = spawnSync('tar', helperArchiveArgs(root));
    expect(archive.status).toBe(0);
    const run = spawnSync(
      process.platform === 'win32' ? 'python' : 'python3',
      ['-c', setup + bootstrap, encoded],
      { input: archive.stdout }
    );
    expect(run.status).toBe(0);
    const state = path.join(home, '.ccs', 'app-updates');
    const generation = fs
      .readFileSync(path.join(state, 'app_updates.py'), 'utf8')
      .split(/\r?\n/)[0]
      .split(' ')
      .at(-1)!;
    for (const name of HELPER_FILES)
      expect(fs.readFileSync(path.join(state, generation, name), 'utf8')).toBe(`fixture ${name}\n`);
  });

  it('keeps the previous launcher and state on an incomplete or corrupt archive', () => {
    const root = fixtureRoot();
    const home = temporary('aac-helper-publish-');
    const state = path.join(home, '.ccs', 'app-updates');
    fs.mkdirSync(state, { recursive: true });
    fs.writeFileSync(path.join(state, 'app_updates.py'), 'previous launcher');
    fs.writeFileSync(path.join(state, 'dashboard-update.lock'), 'live lock');
    fs.writeFileSync(path.join(state, 'pending.json'), 'restart marker');
    const hashes = localHelperChecksums(root);
    fs.writeFileSync(path.join(root, 'scripts/app-updates/app_update_common.py'), 'corrupt');
    const archive = spawnSync('tar', helperArchiveArgs(root));
    const script = helperPublishScript(hashes).replace(
      'pathlib.Path.home()',
      `pathlib.Path(${JSON.stringify(home)})`
    );
    const run = (input: Buffer) =>
      spawnSync(process.platform === 'win32' ? 'python' : 'python3', ['-c', script], { input });
    expect(run(archive.stdout).status).not.toBe(0);
    expect(run(archive.stdout.subarray(0, 512)).status).not.toBe(0);
    expect(fs.readFileSync(path.join(state, 'app_updates.py'), 'utf8')).toBe('previous launcher');
    expect(fs.readFileSync(path.join(state, 'dashboard-update.lock'), 'utf8')).toBe('live lock');
    expect(fs.readFileSync(path.join(state, 'pending.json'), 'utf8')).toBe('restart marker');
    expect(fs.readdirSync(state).sort()).toEqual([
      'app_updates.py',
      'dashboard-update.lock',
      'pending.json',
    ]);
  });

  it('republishes an old Windows generation even when all helper bytes match', () => {
    const root = fixtureRoot();
    const home = temporary('aac-helper-access-migration-');
    const state = path.join(home, '.ccs', 'app-updates');
    const generation = 'helpers-' + '1'.repeat(32);
    fs.mkdirSync(path.join(state, generation), { recursive: true });
    for (const name of HELPER_FILES)
      fs.writeFileSync(path.join(state, generation, name), `fixture ${name}\n`);
    const powershell = Buffer.from(
      helperHashQuery('windows').split(' ').at(-1)!,
      'base64'
    ).toString('utf16le');
    const encoded = /^& python\.exe -c '[^']+' ([A-Za-z0-9+/=]+); exit \$LASTEXITCODE$/.exec(
      powershell
    )![1];
    const script = inflateSync(Buffer.from(encoded, 'base64'))
      .toString()
      .replace('pathlib.Path.home()', `pathlib.Path(${JSON.stringify(home)})`);
    const query = () =>
      spawnSync(process.platform === 'win32' ? 'python' : 'python3', ['-c', script], {
        encoding: 'utf8',
      });
    fs.writeFileSync(
      path.join(state, 'app_updates.py'),
      `# aac-helpers: ${generation}\nimport pathlib\n`
    );
    const old = query();
    expect(old.status).toBe(0);
    expect(old.stdout).toBe('');
    fs.writeFileSync(
      path.join(state, 'app_updates.py'),
      `# aac-helpers: ${generation}\n# aac-helpers-format: 2\n`
    );
    const current = query();
    expect(current.status).toBe(0);
    expect(current.stdout.trim().split(/\r?\n/)).toHaveLength(HELPER_FILES.length);
    for (const [name, hash] of Object.entries(localHelperChecksums(root)))
      expect(current.stdout).toContain(`${hash}  ${name}`);
  });

  it('leaves the previous generation selected when Windows identity or ACL setup fails', () => {
    const root = fixtureRoot();
    const archive = spawnSync('tar', helperArchiveArgs(root));
    expect(archive.status).toBe(0);
    for (const failure of ['identity', 'acl']) {
      const home = temporary('aac-helper-access-failure-');
      const state = path.join(home, '.ccs', 'app-updates');
      fs.mkdirSync(state, { recursive: true });
      fs.writeFileSync(path.join(state, 'app_updates.py'), 'previous launcher');
      const script = helperPublishScript(localHelperChecksums(root))
        .replace('pathlib.Path.home()', `pathlib.Path(${JSON.stringify(home)})`)
        .replace("if os.name == 'nt':", 'if True:');
      const setup = `import subprocess\nsubprocess.check_output = lambda *a, **k: ${failure === 'identity' ? "'not-a-sid'" : "'S-1-5-21-1-2-3-1001'"}\ndef fail(*a, **k):\n raise OSError('fixture ACL failure')\nsubprocess.run = fail\n`;
      const run = spawnSync(
        process.platform === 'win32' ? 'python' : 'python3',
        ['-c', setup + script],
        { input: archive.stdout }
      );
      expect(run.status).not.toBe(0);
      expect(fs.readdirSync(state)).toEqual(['app_updates.py']);
      expect(fs.readFileSync(path.join(state, 'app_updates.py'), 'utf8')).toBe('previous launcher');
    }
  });

  it.skipIf(process.platform !== 'win32')(
    'grants the Windows user inheritable access to the published helpers',
    () => {
      const root = fixtureRoot();
      const home = temporary('aac-helper-user-acl-');
      const archive = spawnSync('tar', helperArchiveArgs(root));
      const script = helperPublishScript(localHelperChecksums(root)).replace(
        'pathlib.Path.home()',
        `pathlib.Path(${JSON.stringify(home)})`
      );
      const run = spawnSync('python', ['-c', script], { input: archive.stdout });
      expect(run.status).toBe(0);
      const state = path.join(home, '.ccs', 'app-updates');
      const generation = fs
        .readFileSync(path.join(state, 'app_updates.py'), 'utf8')
        .split(/\r?\n/)[0]
        .split(' ')
        .at(-1)!;
      const targets = [
        state,
        path.join(state, generation),
        path.join(state, generation, 'app_updates.py'),
      ]
        .map((value) => `'${value.replace(/'/g, "''")}'`)
        .join(',');
      const inspect = `$ErrorActionPreference='Stop'; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; @(@(${targets}) | ForEach-Object { $item=Get-Item -LiteralPath $_; $acl=Get-Acl -LiteralPath $_; @($acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier]) | Where-Object { $_.IdentityReference -eq $sid -and $_.AccessControlType -eq 'Allow' -and ($_.FileSystemRights -band 2032127) -eq 2032127 -and (($item.PSIsContainer -and ($_.InheritanceFlags -band 3) -eq 3) -or (-not $item.PSIsContainer -and $_.IsInherited)) }).Count }) | ConvertTo-Json -Compress`;
      const acl = spawnSync(
        'powershell.exe',
        [
          '-NoProfile',
          '-NonInteractive',
          '-EncodedCommand',
          Buffer.from(inspect, 'utf16le').toString('base64'),
        ],
        { encoding: 'utf8' }
      );
      expect(acl.status).toBe(0);
      const granted = JSON.parse(acl.stdout.trim());
      expect(granted).toHaveLength(3);
      expect(granted.every((count: number) => count > 0)).toBe(true);
    }
  );

  it('hashes the runtime from the build output beside the source helpers', () => {
    const root = fixtureRoot();
    const values = localHelperChecksums(root);
    expect(Object.keys(values).sort()).toEqual([...HELPER_FILES].sort());
    for (const name of HELPER_FILES)
      expect(values[name]).toBe(createHash('sha256').update(`fixture ${name}\n`).digest('hex'));
  });

  it('fails loudly when the runtime is not built instead of sending a partial set', () => {
    const root = fixtureRoot(false);
    const message = 'The Codex update runtime is not built; run bun run build:server.';
    expect(() => localHelperChecksums(root)).toThrow(message);
    expect(() => helperArchiveArgs(root)).toThrow(message);
  });

  it('fails when a source helper is missing', () => {
    const root = fixtureRoot();
    fs.rmSync(path.join(root, 'scripts', 'app-updates', 'app_update_codex.cjs'));
    expect(() => localHelperChecksums(root)).toThrow('app_update_codex.cjs is missing');
  });
});

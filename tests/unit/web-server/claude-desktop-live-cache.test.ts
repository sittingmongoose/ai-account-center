import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  readClaudeDesktopLiveSnapshot,
  writeClaudeDesktopLiveSnapshot,
} from '../../../src/web-server/services/claude-desktop-live-cache';
import type { ClaudeDesktopLiveUsage } from '../../../src/web-server/services/claude-desktop-live-service';

const manifest = 'a'.repeat(64);
const sample: ClaudeDesktopLiveUsage = {
  profileId: 'gmail',
  email: 'fixture@example.com',
  platform: 'windows',
  source: 'Claude Desktop live quota on Windows',
  plan: 'max',
  fetchedAt: '2026-10-01T10:00:00Z',
  windows: [
    {
      key: 'seven_day',
      label: 'Weekly usage',
      usedPercent: 125.5,
      remainingPercent: 0,
      resetAt: '2026-10-02T03:00:00Z',
      windowMinutes: 10080,
      used: null,
      limit: null,
      unit: null,
    },
  ],
};

describe('protected persisted Claude quota snapshots', () => {
  let root: string;
  let directory: string;
  let file: string;

  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-claude-quota-cache-'));
    directory = path.join(root, 'claude-desktop-live-cache');
    file = path.join(directory, 'gmail.json');
  });

  afterEach(() => {
    mock.restore();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('atomically retains only public fields and original amounts/timestamps in private storage', async () => {
    const unsafe = {
      ...sample,
      accessToken: 'PRIVATE_TOKEN_SENTINEL',
      windows: [{ ...sample.windows[0], cookieHeader: 'PRIVATE_COOKIE_SENTINEL' }],
    };
    await writeClaudeDesktopLiveSnapshot(root, 'gmail', manifest, unsafe);
    const contents = fs.readFileSync(file, 'utf8');
    expect(contents).not.toContain('PRIVATE_');
    expect(await readClaudeDesktopLiveSnapshot(root, 'gmail', manifest)).toEqual(sample);
    expect(fs.statSync(directory).mode & 0o777).toBe(0o700);
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    expect(fs.readdirSync(directory)).toEqual(['gmail.json']);
  });

  it('rejects a changed manifest or an unknown profile without accessing another account', async () => {
    await writeClaudeDesktopLiveSnapshot(root, 'gmail', manifest, sample);
    expect(await readClaudeDesktopLiveSnapshot(root, 'gmail', 'b'.repeat(64))).toBeNull();
    expect(await readClaudeDesktopLiveSnapshot(root, '../gmail', manifest)).toBeNull();
    await writeClaudeDesktopLiveSnapshot(root, '../gmail', manifest, sample);
    expect(fs.readdirSync(directory)).toEqual(['gmail.json']);
  });

  it('persists only whitelisted optional-group availability and original cached window time', async () => {
    const value = {
      ...sample,
      optionalExtras: {
        resetCredits: 'unavailable' as const,
        prepaidBalance: 'ok' as const,
        token: 'PRIVATE_TOKEN_SENTINEL',
      },
      windows: [
        {
          ...sample.windows[0],
          key: 'prepaid_balance',
          status: 'cached' as const,
          sampledAt: '2026-10-01T09:00:00Z',
          cookie: 'PRIVATE_COOKIE_SENTINEL',
        },
      ],
    };
    await writeClaudeDesktopLiveSnapshot(root, 'gmail', manifest, value);
    const contents = fs.readFileSync(file, 'utf8');
    expect(contents).not.toContain('PRIVATE_');
    expect(await readClaudeDesktopLiveSnapshot(root, 'gmail', manifest)).toMatchObject({
      fetchedAt: sample.fetchedAt,
      optionalExtras: { resetCredits: 'unavailable', prepaidBalance: 'ok' },
      windows: [{ status: 'cached', sampledAt: '2026-10-01T09:00:00Z' }],
    });
  });

  it('rejects malformed or oversized files before unbounded decoding', async () => {
    await writeClaudeDesktopLiveSnapshot(root, 'gmail', manifest, sample);
    fs.writeFileSync(file, '{broken');
    expect(await readClaudeDesktopLiveSnapshot(root, 'gmail', manifest)).toBeNull();
    fs.writeFileSync(file, 'x'.repeat(64 * 1024 + 1));
    expect(await readClaudeDesktopLiveSnapshot(root, 'gmail', manifest)).toBeNull();
  });

  it('refuses symlinks for both read and replacement, preserving the linked target', async () => {
    fs.mkdirSync(directory, { mode: 0o700 });
    const target = path.join(root, 'unrelated.json');
    fs.writeFileSync(target, 'UNRELATED_CONTENT');
    fs.symlinkSync(target, file);
    expect(await readClaudeDesktopLiveSnapshot(root, 'gmail', manifest)).toBeNull();
    await writeClaudeDesktopLiveSnapshot(root, 'gmail', manifest, sample);
    expect(fs.readFileSync(target, 'utf8')).toBe('UNRELATED_CONTENT');
    expect(fs.lstatSync(file).isSymbolicLink()).toBe(true);
  });

  it('refuses symlinked directories and hard-linked or nonprivate files', async () => {
    const target = path.join(root, 'unrelated-directory');
    fs.mkdirSync(target, { mode: 0o700 });
    fs.symlinkSync(target, directory);
    await writeClaudeDesktopLiveSnapshot(root, 'gmail', manifest, sample);
    expect(fs.readdirSync(target)).toEqual([]);
    fs.unlinkSync(directory);
    await writeClaudeDesktopLiveSnapshot(root, 'gmail', manifest, sample);
    fs.linkSync(file, path.join(root, 'alias.json'));
    expect(await readClaudeDesktopLiveSnapshot(root, 'gmail', manifest)).toBeNull();
    fs.unlinkSync(path.join(root, 'alias.json'));
    fs.chmodSync(file, 0o644);
    expect(await readClaudeDesktopLiveSnapshot(root, 'gmail', manifest)).toBeNull();
    fs.chmodSync(file, 0o600);
    fs.chmodSync(directory, 0o755);
    expect(await readClaudeDesktopLiveSnapshot(root, 'gmail', manifest)).toBeNull();
  });

  it('refuses storage owned by a different user and isolates CCS directories', async () => {
    await writeClaudeDesktopLiveSnapshot(root, 'gmail', manifest, sample);
    expect(
      await readClaudeDesktopLiveSnapshot(path.join(root, 'other'), 'gmail', manifest)
    ).toBeNull();
    if (process.getuid) {
      spyOn(process, 'getuid').mockReturnValue(process.getuid() + 1);
      expect(await readClaudeDesktopLiveSnapshot(root, 'gmail', manifest)).toBeNull();
    }
  });
});

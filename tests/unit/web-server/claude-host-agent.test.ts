/**
 * M4a/M4b agent adapters (lane F): the purge permit's binding, the claude-host reply
 * validation through the real TS fallback transport with a fake process runner, and the
 * labelled fallback when no agent artifact exists. No real host, app or provider.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import fs from 'fs';
import path from 'path';
import {
  claudeHostPurgePermit,
  runClaudeHostAgent,
} from '../../../src/web-server/services/agent-verbs';
import { SshClaudeHostTransport } from '../../../src/web-server/services/claude-host-transport';
import { launchApp } from '../../../src/utils/app-launcher';
import type { ClaudeHostLauncher } from '../../../src/web-server/services/claude-account-stores';
import type { runManagedCommand } from '../../../src/utils/managed-process';

const savedVerbs = process.env.AAC_AGENT_VERBS;
const savedTransport = process.env.AAC_AGENT_TRANSPORT;
afterEach(() => {
  if (savedVerbs === undefined) delete process.env.AAC_AGENT_VERBS;
  else process.env.AAC_AGENT_VERBS = savedVerbs;
  if (savedTransport === undefined) delete process.env.AAC_AGENT_TRANSPORT;
  else process.env.AAC_AGENT_TRANSPORT = savedTransport;
});

const launcher: ClaudeHostLauncher = {
  launcherName: 'Claude (party)',
  launcherPath: 'C:\\Users\\x\\Desktop\\Claude (party).lnk',
  startMenuPath:
    'C:\\Users\\x\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Claude (party).lnk',
  profilePath: 'C:\\Users\\x\\AppData\\Roaming\\Claude-party',
  sshHost: 'windows-host',
};

describe('claudeHostPurgePermit', () => {
  it('binds host, trash folder and the 120-second expiry', () => {
    const now = 1_700_000_000_000;
    const token = claudeHostPurgePermit('windows', 'party-20261002T080000Z', now);
    expect(token).toMatch(/^1700000120000\.[a-f0-9]{64}$/);
    expect(claudeHostPurgePermit('windows', 'party-20261002T080000Z', now)).toBe(token);
    expect(claudeHostPurgePermit('mac', 'party-20261002T080000Z', now)).not.toBe(token);
    expect(claudeHostPurgePermit('windows', 'other-20261002T080000Z', now)).not.toBe(token);
    expect(claudeHostPurgePermit('windows', 'party-20261002T080000Z', now + 1)).not.toBe(token);
  });
});

describe('runClaudeHostAgent reply validation', () => {
  // A fake artifact and process runner drive the real TS fallback transport end to end.
  function fakeAgent(value: unknown) {
    const dir = fs.mkdtempSync(path.join(process.cwd(), '.tmp-agent-test-'));
    const file = path.join(dir, 'fake-agent.bin');
    fs.writeFileSync(file, 'fixture agent image');
    const artifact = {
      target: 'x86_64-pc-windows-msvc',
      version: '0.1.0',
      sha256: createHash('sha256').update(fs.readFileSync(file)).digest('hex'),
      file: path.relative(process.cwd(), file),
    };
    const calls: Array<{ args: string[]; input?: string }> = [];
    const run: typeof runManagedCommand = async (_file, args, opts) => {
      calls.push({ args, input: opts.input });
      const ok = (stdout: string) => ({ code: 0, stdout, timedOut: false, missing: false, failed: false });
      if (args[0] === '--version') {
        return ok(
          JSON.stringify({ name: 'aac-agent', version: '0.1.0', protocol: 1, target: artifact.target })
        );
      }
      return ok(JSON.stringify({ protocol: 1, id: 'claude-host', type: 'result', value }) + '\n');
    };
    return { artifact, run, calls, cleanup: () => fs.rmSync(dir, { recursive: true, force: true }) };
  }

  it('accepts only the reference answer shape for each operation', async () => {
    const cases: Array<[Record<string, unknown>, unknown, boolean]> = [
      [{ op: 'session', launcher }, { signedIn: true }, true],
      [{ op: 'session', launcher }, { signedIn: 'yes' }, false],
      [{ op: 'state', launcher }, { running: false }, true],
      [{ op: 'state', launcher }, { running: 0 }, false],
      [{ op: 'trash', profileId: 'party', trashName: 'party-20261002T080000Z', launcher }, { result: 'moved' }, true],
      [{ op: 'trash', profileId: 'party', trashName: 'party-20261002T080000Z', launcher }, { result: 'copied' }, false],
      [{ op: 'purge', trashName: 'party-20261002T080000Z', token: 'x' }, { ok: true }, true],
      [{ op: 'purge', trashName: 'party-20261002T080000Z', token: 'x' }, { ok: false }, false],
      [{ op: 'create', profileId: 'party' }, { launcherName: 'Claude (party)', profilePath: 'C:\\p' }, true],
      [{ op: 'create', profileId: 'party' }, { launcherName: 'Claude (party)' }, false],
      [{ op: 'open', profileId: 'party' }, { ok: true }, true],
    ];
    for (const [args, value, accepted] of cases) {
      const fake = fakeAgent(value);
      try {
        const result = await runClaudeHostAgent(
          { platform: 'windows' },
          args,
          { mode: 'on', transport: 'off', artifacts: [fake.artifact], run: fake.run, deadlineMs: 5000, maxBytes: 65536 }
        );
        if (accepted) {
          expect(result).toEqual(value as Record<string, unknown>);
          // The envelope binds the host family and carries the request on stdin.
          const input = JSON.parse(fake.calls[1].input ?? '{}');
          expect(input.verb).toBe('claude-host');
          expect(input.args.host).toBe('windows');
          expect(input.args.op).toBe(args.op);
        } else {
          expect(result).toBeNull();
        }
      } finally {
        fake.cleanup();
      }
    }
  });

  it('delegates when the agent reports a failure or a fallback', async () => {
    for (const value of [{ fallback: 'python' }]) {
      const fake = fakeAgent(value);
      try {
        const result = await runClaudeHostAgent(
          { platform: 'windows' },
          { op: 'session', launcher },
          { mode: 'on', transport: 'off', artifacts: [fake.artifact], run: fake.run, deadlineMs: 5000, maxBytes: 65536 }
        );
        expect(result).toBeNull();
      } finally {
        fake.cleanup();
      }
    }
  });
});

describe('labelled fallback with no artifact', () => {
  it('keeps the ssh bootstrap answering when the verbs are on but no agent exists', async () => {
    process.env.AAC_AGENT_VERBS = 'on';
    process.env.AAC_AGENT_TRANSPORT = 'off';
    const calls: Array<{ command: string; input: string }> = [];
    const transport = new SshClaudeHostTransport(async (_host, command, input) => {
      calls.push({ command, input });
      return '{"running":false}';
    });
    expect(await transport.appState('windows', { launcher })).toBe('stopped');
    expect(calls).toHaveLength(1);
    expect(calls[0].command).toMatch(/^powershell\.exe -NoProfile -NonInteractive -EncodedCommand /);
    // Purge without a permit skips the agent; with one it still falls back (no artifact).
    await transport.purge('windows', { sshHost: 'windows-host', trashName: 'party-20261002T080000Z' });
    await transport.purge('windows', {
      sshHost: 'windows-host',
      trashName: 'party-20261002T080000Z',
      token: claudeHostPurgePermit('windows', 'party-20261002T080000Z'),
    });
    expect(calls).toHaveLength(3);
  });

  it('launchApp falls through to the reference launcher and keeps its error', async () => {
    process.env.AAC_AGENT_VERBS = 'on';
    process.env.AAC_AGENT_TRANSPORT = 'off';
    const dir = fs.mkdtempSync(path.join(process.cwd(), '.tmp-launch-test-'));
    try {
      await expect(
        launchApp({ app: 'app', exe: path.join(dir, 'missing-exe'), args: [] })
      ).rejects.toThrow();
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});

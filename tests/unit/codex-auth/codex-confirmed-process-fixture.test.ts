import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { spawn, type ChildProcess } from 'child_process';
import {
  createCodexActivationRuntime,
  CodexActivationRuntimeError,
  type CodexProcessSnapshot,
} from '../../../src/codex-auth/codex-activation-runtime';

describe('confirmed switch with disposable real process families', () => {
  it('actually stops only the approved family and reopens a fresh process preserving cwd/env without replaying its prompt', async () => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-confirmed-process-'));
    const codexHome = path.join(directory, '.codex');
    fs.mkdirSync(codexHome);
    // The runtime recognises Codex by the program's file name, so the fixture family needs a
    // real program called `codex`. Copy the JavaScript runtime running this test (it runs the
    // fixture script just as well). Never a fixed system path: GitHub's Ubuntu runners have
    // no /usr/bin/node (their Node lives under /usr/local and the tool cache).
    const executable = path.join(directory, 'codex');
    fs.copyFileSync(process.execPath, executable);
    fs.chmodSync(executable, 0o700);
    const script = path.join(directory, 'fixture.js');
    fs.writeFileSync(
      script,
      `
      const fs = require('fs'), cp = require('child_process');
      const fresh = process.argv.includes('--fresh');
      let child;
      if (!fresh && !process.argv.includes('--child')) {
        child = cp.spawn(process.execPath, [${JSON.stringify(script)}, '--child'], {stdio:'ignore'});
      }
      fs.writeFileSync(${JSON.stringify(path.join(directory, 'process-'))} + process.pid + '.json', JSON.stringify({
        pid:process.pid, child:child?.pid, fresh,
        promptReplayed:process.argv.includes('private-fixture-prompt'),
        environmentPreserved:process.env.CCS_PRIVATE_FIXTURE === 'private-fixture-value',
        cwdPreserved:process.cwd() === ${JSON.stringify(directory)}
      }));
      process.on('SIGTERM', () => { if(child) child.kill('SIGTERM'); process.exit(0); });
      setInterval(() => {}, 1000);
    `
    );
    const children: ChildProcess[] = [];
    let observedChild: number | undefined;
    const fixtureEnv = {
      ...process.env,
      HOME: directory,
      CODEX_HOME: codexHome,
      CCS_PRIVATE_FIXTURE: 'private-fixture-value',
    };
    function launch(args: string[]): ChildProcess {
      const child = spawn(executable, [script, ...args], {
        cwd: directory,
        env: fixtureEnv,
        stdio: 'ignore',
      });
      children.push(child);
      return child;
    }
    function snapshot(pid: number): CodexProcessSnapshot | undefined {
      try {
        const root = `/proc/${pid}`;
        const raw = fs.readFileSync(`${root}/stat`, 'utf8');
        const fields = raw.slice(raw.lastIndexOf(')') + 2).split(' ');
        const env: NodeJS.ProcessEnv = {};
        for (const item of fs.readFileSync(`${root}/environ`, 'utf8').split('\0')) {
          const separator = item.indexOf('=');
          if (separator > 0) env[item.slice(0, separator)] = item.slice(separator + 1);
        }
        return {
          pid,
          ppid: Number(fields[1]),
          state: fields[0],
          startTime: fields[19],
          env,
          exe: fs.readlinkSync(`${root}/exe`),
          cwd: fs.readlinkSync(`${root}/cwd`),
          args: fs.readFileSync(`${root}/cmdline`, 'utf8').split('\0').filter(Boolean),
        };
      } catch {
        return undefined;
      }
    }
    async function ready(pid: number): Promise<void> {
      for (let attempt = 0; attempt < 100; attempt++) {
        if (fs.existsSync(path.join(directory, `process-${pid}.json`))) return;
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      throw new Error('Fixture did not start');
    }
    const original = launch(['private-fixture-prompt']);
    const unrelated = spawn('/bin/sleep', ['60'], { stdio: 'ignore' });
    children.push(unrelated);
    let clock = 0;
    const signalled: number[] = [];
    try {
      await ready(original.pid!);
      observedChild = JSON.parse(
        fs.readFileSync(path.join(directory, `process-${original.pid}.json`), 'utf8')
      ).child;
      await ready(observedChild!);
      const scan = async (): Promise<CodexProcessSnapshot[]> => {
        const pids = [
          ...children.map((entry) => entry.pid!),
          ...(observedChild ? [observedChild] : []),
        ];
        return pids.map(snapshot).filter((entry): entry is CodexProcessSnapshot => Boolean(entry));
      };
      const runtime = createCodexActivationRuntime(codexHome, {
        scan,
        assertIdle: async () => {},
        acquireStartupLock: async () => async () => {},
        acquireNativeStartupLock: async () => {},
        releaseNativeStartupLock: async () => {},
        prepareCli: async () => {},
        signal: async (target, signal) => {
          const live = snapshot(target.pid);
          if (live?.startTime !== target.startTime) throw new Error('Fixture PID changed');
          signalled.push(target.pid);
          process.kill(target.pid, signal);
        },
        launchCli: async () => {
          const fresh = launch(['--fresh']);
          await ready(fresh.pid!);
        },
        sleep: async (milliseconds) => {
          clock += milliseconds;
          await new Promise((resolve) => setTimeout(resolve, 2));
        },
        now: () => clock,
        removeControlSocket: () => {},
      });
      let plan;
      try {
        await runtime.stop();
      } catch (error) {
        expect(error).toBeInstanceOf(CodexActivationRuntimeError);
        plan = (error as CodexActivationRuntimeError).stopPlan;
      }
      expect(plan).toBeDefined();
      expect(signalled).toEqual([]);
      expect(snapshot(original.pid!)).toBeDefined();
      await runtime.stop(plan);
      expect(snapshot(original.pid!)).toBeUndefined();
      expect(snapshot(observedChild!)).toBeUndefined();
      expect(snapshot(unrelated.pid!)).toBeDefined();
      await runtime.start();
      const fresh = children.at(-1)!;
      const proof = JSON.parse(
        fs.readFileSync(path.join(directory, `process-${fresh.pid}.json`), 'utf8')
      );
      expect(proof.fresh).toBe(true);
      expect(proof.promptReplayed).toBe(false);
      expect(proof.environmentPreserved).toBe(true);
      expect(proof.cwdPreserved).toBe(true);
      expect(signalled).not.toContain(unrelated.pid!);
    } finally {
      for (const child of children) if (child.exitCode === null) child.kill('SIGTERM');
      if (observedChild && snapshot(observedChild)) {
        try {
          process.kill(observedChild, 'SIGTERM');
        } catch {
          /* Already exited. */
        }
      }
      await new Promise((resolve) => setTimeout(resolve, 30));
      fs.rmSync(directory, { recursive: true, force: true });
    }
  });
});

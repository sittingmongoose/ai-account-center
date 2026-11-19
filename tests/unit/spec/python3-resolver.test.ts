import { describe, expect, it } from 'bun:test';
import { resolvePython3, requirePython3 } from '../../spec/python3.mjs';

const candidates = [
  { file: 'python3', args: [] },
  { file: 'python', args: [] },
  { file: 'py', args: ['-3'] },
];

/** A spawnSync stub answering each candidate from `answers` (default: a working Python 3). */
function probeWith(answers = new Map()) {
  const calls = [];
  const spawnSync = (file, args) => {
    calls.push([file, ...args]);
    const answer = answers.get(file) ?? { status: 0, stdout: 'Python 3.13.7\n' };
    return { status: answer.status, stdout: answer.stdout ?? '', stderr: answer.stderr ?? '' };
  };
  return { spawnSync, calls };
}

/** The spec gate must find a Python 3 on Windows too: python3, python, then py -3 (tool-issue 51). */
describe('resolvePython3', () => {
  it('prefers python3 when it answers, so POSIX hosts keep their interpreter', () => {
    const probe = probeWith();
    expect(resolvePython3({ spawnSync: probe.spawnSync, candidates })).toEqual({
      file: 'python3',
      args: [],
    });
    expect(probe.calls).toEqual([['python3', '--version']]);
  });

  it('skips the WindowsApps python3 stub that answers non-zero without a Store Python', () => {
    const probe = probeWith(
      new Map([
        // The stub prints "Python was not found" and exits 9009.
        ['python3', { status: 9009, stderr: 'Python was not found\n' }],
      ])
    );
    expect(resolvePython3({ spawnSync: probe.spawnSync, candidates })).toEqual({
      file: 'python',
      args: [],
    });
    expect(probe.calls.map(([file]) => file)).toEqual(['python3', 'python']);
  });

  it('falls back to the py launcher when only it has a Python 3', () => {
    const probe = probeWith(
      new Map([
        ['python3', { status: 1, stderr: 'not found' }],
        ['python', { status: 1, stderr: 'not found' }],
      ])
    );
    expect(resolvePython3({ spawnSync: probe.spawnSync, candidates })).toEqual({
      file: 'py',
      args: ['-3'],
    });
    expect(probe.calls[2]).toEqual(['py', '-3', '--version']);
  });

  it('accepts a version answer on stderr (older interpreters print there)', () => {
    const probe = probeWith(
      new Map([
        ['python3', { status: 0, stderr: 'Python 3.11.9\r\n' }],
      ])
    );
    expect(resolvePython3({ spawnSync: probe.spawnSync, candidates })).toEqual({
      file: 'python3',
      args: [],
    });
  });

  it('rejects an interpreter that is not Python 3 and says so when none qualifies', () => {
    const python2Everywhere = new Map(
      candidates.map(({ file }) => [file, { status: 0, stdout: 'Python 2.7.18\n' }])
    );
    const probe = probeWith(python2Everywhere);
    expect(resolvePython3({ spawnSync: probe.spawnSync, candidates })).toBeNull();
    expect(() => requirePython3({ spawnSync: probe.spawnSync, candidates })).toThrow(
      /No Python 3 interpreter found: tried python3, python, "py -3"\./
    );
  });
});

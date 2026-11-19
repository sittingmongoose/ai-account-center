import { spawnSync } from 'node:child_process';

// Windows installs provide python.exe or the py launcher instead of python3, and the
// WindowsApps python3 alias is a stub that prints "Python was not found" and exits
// non-zero until a Store Python is installed (tool-issue 51). Probe each candidate
// once and use the first that answers --version with Python 3; python3 stays first,
// so POSIX hosts keep exactly their current interpreter.
const CANDIDATES = [
  { file: 'python3', args: [] },
  { file: 'python', args: [] },
  { file: 'py', args: ['-3'] },
];

const isPython3 = (output) => /Python 3\.\d+/i.test(output);

export function resolvePython3(deps = {}) {
  const spawn = deps.spawnSync ?? spawnSync;
  const candidates = deps.candidates ?? CANDIDATES;
  for (const candidate of candidates) {
    // Older interpreters print --version to stderr, so both streams count as the answer.
    const probe = spawn(candidate.file, [...candidate.args, '--version'], { encoding: 'utf8' });
    const answer = `${probe.stdout ?? ''}${probe.stderr ?? ''}`;
    if (probe.status === 0 && isPython3(answer)) return candidate;
  }
  return null;
}

export function requirePython3(deps = {}) {
  const resolved = resolvePython3(deps);
  if (!resolved) {
    throw new Error('No Python 3 interpreter found: tried python3, python, "py -3".');
  }
  return resolved;
}

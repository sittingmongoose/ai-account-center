import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const cwd = fileURLToPath(new URL('../..', import.meta.url));
for (const [program, args] of [
  ['node', ['scripts/foundation/generate-id-vectors.mjs', '--check']],
  ['python3', ['tests/spec/parsers.py']],
  ['node', ['tests/spec/parser-ts.mjs']],
  ['node', ['tests/spec/inventory.mjs']],
  ['bun', ['tests/spec/capture.ts']],
])
  execFileSync(program, args, {
    cwd,
    stdio: 'inherit',
    env: { ...process.env, PYTHONDONTWRITEBYTECODE: '1', AAC_NO_LOCAL_RUST_BUILD: '1' },
  });

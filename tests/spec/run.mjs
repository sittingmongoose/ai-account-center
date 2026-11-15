import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const cwd = fileURLToPath(new URL('../..', import.meta.url));
for (const [program, args] of [
  ['node', ['scripts/foundation/generate-id-vectors.mjs', '--check']],
  ['python3', ['tests/spec/parsers.py']],
  ['node', ['tests/spec/parser-ts.mjs']],
  ['node', ['tests/spec/inventory.mjs']],
  ['bun', ['test', 'tests/spec/tables.test.ts']],
  [
    'bun',
    [
      'tests/spec/settings-parity.ts',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  [
    'bun',
    [
      'tests/spec/read-route-parity.ts',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  ['node', ['tests/spec/read-number-vectors.mjs']],
  ...[
    'read-view-parity',
    'read-file-parity',
    'read-http-parity',
    'account-read-parity',
    'profile-read-parity',
    'home-number-admission',
  ].map((name) => [
    'bun',
    [
      `tests/spec/${name}.ts`,
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ]),
  ['bun', ['tests/spec/capture.ts']],
])
  execFileSync(program, args, {
    cwd,
    stdio: 'inherit',
    env: { ...process.env, PYTHONDONTWRITEBYTECODE: '1', AAC_NO_LOCAL_RUST_BUILD: '1' },
  });

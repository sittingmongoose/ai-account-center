import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const cwd = fileURLToPath(new URL('../..', import.meta.url));
const legacyResetGroup = process.argv.includes('--group=reset-claims');
if (process.argv.includes('--cut-rust-only') || legacyResetGroup) {
  const binary =
    process.env.AAC_M6_TARGET_CLI ??
    (legacyResetGroup ? process.env.AAC_RUST_PUBLIC_BINARY : undefined);
  if (!binary) throw new Error('Set AAC_M6_TARGET_CLI to the native dashboard command.');
  execFileSync(
    'bun',
    [
      'tests/spec/m6-public-contract.ts',
      binary,
      '--cut-rust-only',
      '--require-complete',
      ...process.argv.slice(2),
    ],
    { cwd, stdio: 'inherit' }
  );
  process.exit(0);
}
for (const [program, args] of [
  ['node', ['scripts/foundation/generate-id-vectors.mjs', '--check']],
  ['python3', ['tests/spec/parsers.py']],
  ['node', ['tests/spec/parser-ts.mjs']],
  ['node', ['tests/spec/inventory.mjs']],
  ['bun', ['test', 'tests/spec/tables.test.ts']],
  // L2: one file policy (no raw file mechanics outside aac-files) and the shared registry.
  ['node', ['scripts/foundation/one-file-policy.mjs']],
  ['bun', ['test', 'tests/spec/private-files.test.ts']],
  ...(process.env.AAC_RUST_PUBLIC_BINARY
    ? [
        ['bun', ['tests/spec/m6-auth-parity.ts', process.env.AAC_RUST_PUBLIC_BINARY]],
        ['bun', ['tests/spec/m6-events-parity.ts', process.env.AAC_RUST_PUBLIC_BINARY]],
        ['bun', ['tests/spec/m6-tray-parity.ts', process.env.AAC_RUST_PUBLIC_BINARY]],
        [
          'bun',
          [
            'tests/spec/m6-public-contract.ts',
            process.env.AAC_RUST_PUBLIC_BINARY,
            ...(process.env.AAC_M6_REQUIRE_COMPLETE === '1' ? ['--require-complete'] : []),
          ],
        ],
      ]
    : []),
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
  [
    'bun',
    [
      'tests/spec/settings-write-parity.ts',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  [
    'bun',
    [
      'tests/spec/approval-parity.ts',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  // K/G11: offline provider fixtures compare the historical HTTP adapter to the Rust listener.
  ...(process.platform === 'win32' || !process.env.AAC_STATUS_MODULE
    ? []
    : ['reset-claim-parity', 'reset-source-parity'].map((name) => [
        'bun',
        [`tests/spec/${name}.ts`, ...(process.env.AAC_RUST_PUBLIC_BINARY ? [] : ['--ts-only'])],
      ])),
  [
    'bun',
    [
      'tests/spec/app-update-http-parity.ts',
      ...(process.env.AAC_SPEC_APP_UPDATES_UNBOUND === '1' ? ['--rust-unbound'] : []),
    ],
  ],
  [
    'bun',
    [
      'tests/spec/preferences-write-parity.ts',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  [
    'bun',
    [
      'tests/spec/history-cleanup-parity.ts',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  [
    'node',
    [
      'tests/spec/lifecycle-writes.mjs',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  [
    'node',
    [
      'tests/spec/status-answer-writes.mjs',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  [
    'bun',
    [
      'tests/spec/local-key-write-parity.ts',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  [
    'node',
    [
      'tests/spec/provider-install-writes.mjs',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  [
    'bun',
    [
      'tests/spec/signin-journal-parity.ts',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  // G21: the TS log's outputs that the Rust aac-log crate reproduces.
  ['bun', ['tests/spec/log-vectors.ts']],
  ['node', ['tests/spec/read-number-vectors.mjs']],
  // M6: the T3 hub, the static/Leptos bundle edges and the CLI entry points, recorded from TS and replayable
  // against another front door (AAC_M6_TARGET_URL / AAC_M6_TARGET_CLI).
  [
    'bun',
    [
      'tests/spec/m6-edge-parity.ts',
      ...(process.env.AAC_RUST_PUBLIC_BINARY && !process.env.AAC_M6_TARGET_URL
        ? ['--rust-fixture']
        : []),
    ],
  ],
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
  // L1: the service lifecycle on the real binary, under Node (the installed runtime), on dist/.
  [
    'node',
    [
      'tests/spec/rust-lifecycle.mjs',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE)
    ? [['node', ['tests/spec/dashboard-read-parity.mjs']]]
    : []),
  // R1: Home fed against today's handler, end to end on the real binary, under Node.
  [
    'node',
    [
      'tests/spec/home-read-parity.mjs',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  // R2: Status fed against today's handler, end to end on the real binary and the real owner, under Node; and
  // the Status fuzz (TS, the envelope read and the fed path) on a short run (the full run: 4 seeds x 400).
  [
    'node',
    [
      'tests/spec/status-read-parity.mjs',
      ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE) ? [] : ['--ts-only']),
    ],
  ],
  ...(['on', 'shadow'].includes(process.env.AAC_RUST_SERVICE)
    ? [['bun', ['tests/spec/status-fuzz.ts', '--cases', '100']]]
    : []),
  // R3: the store host's exit gate (both handles, kill drills, fallback, restore), under Node.
  [
    'node',
    ['tests/spec/store-host.mjs', ...(process.env.AAC_RUST_SERVICE === 'on' ? [] : ['--ts-only'])],
  ],
])
  execFileSync(program, args, {
    cwd,
    stdio: 'inherit',
    env: { ...process.env, PYTHONDONTWRITEBYTECODE: '1', AAC_NO_LOCAL_RUST_BUILD: '1' },
  });

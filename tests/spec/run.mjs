import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { requirePython3 } from './python3.mjs';
const cwd = fileURLToPath(new URL('../..', import.meta.url));
const legacyResetGroup = process.argv.includes('--group=reset-claims');
const routeGroup = process.argv.find((arg) => arg.startsWith('--route-group='));
const recording = process.argv.some((arg) => arg.startsWith('--record-native='));
if (routeGroup || recording || process.argv.includes('--cut-rust-only') || legacyResetGroup) {
  const binary =
    process.env.AAC_M6_TARGET_CLI ??
    (legacyResetGroup ? process.env.AAC_RUST_PUBLIC_BINARY : undefined);
  if (!binary) throw new Error('Set AAC_M6_TARGET_CLI to the native dashboard command.');
  const mode = recording && process.env.AAC_RECORD_RUST_ONLY !== '1' ? '--cut' : '--cut-rust-only';
  execFileSync(
    'bun',
    [
      'tests/spec/m6-public-contract.ts',
      binary,
      mode,
      ...process.argv.slice(2).filter((arg) => arg !== '--cut-rust-only' && arg !== '--cut'),
    ],
    { cwd, stdio: 'inherit' }
  );
  process.exit(0);
}
const python3 = requirePython3();
// The TypeScript server (src/) and its parity harnesses were retired; what remains checks the
// shared tables, the vectors and the native binary.
for (const [program, args] of [
  [python3.file, [...python3.args, 'tests/unit/account-usage/plan_usage_test.py']],
  ['node', ['tests/spec/inventory.mjs']],
  ['bun', ['test', 'tests/spec/tables.test.ts']],
  // L2: one file policy (no raw file mechanics outside aac-files).
  ['node', ['scripts/foundation/one-file-policy.mjs']],
  ['node', ['tests/spec/read-number-vectors.mjs']],
  ...(process.env.AAC_RUST_PUBLIC_BINARY
    ? [
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
])
  execFileSync(program, args, {
    cwd,
    stdio: 'inherit',
    env: { ...process.env, PYTHONDONTWRITEBYTECODE: '1', AAC_NO_LOCAL_RUST_BUILD: '1' },
  });

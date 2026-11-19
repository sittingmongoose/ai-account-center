import { execFileSync } from 'node:child_process';
import path from 'node:path';

const tsOnly = process.argv.includes('--ts-only');
for (const mode of tsOnly ? ['off', 'shadow'] : ['off', 'shadow', 'on']) {
  execFileSync(
    'bun',
    [
      'test',
      'tests/unit/web-server/account-lifecycle-routes.test.ts',
      '--test-name-pattern',
      'registered lifecycle state write parity',
    ],
    {
      stdio: 'inherit',
      env: {
        ...process.env,
        AAC_SPEC_LIFECYCLE_WRITE_MODE: mode,
        AAC_RUST_GROUP_WRITES: 'on',
        AAC_AGENT_TRANSPORT: 'off',
        AAC_AGENT_VERBS: 'off',
        AAC_STORE: 'off',
        AAC_RUST_SERVICE_BINARY:
          process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http'),
      },
    }
  );
}
console.log(
  'registered lifecycle state write parity: ' + (tsOnly ? 'off/shadow' : 'off/shadow/on')
);

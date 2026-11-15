import { it, expect } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';

const root = path.resolve(import.meta.dir, '../../..');
const { verifyStatusModule } = require('../../../scripts/verify-bundle.js');

for (const receipt of ['missing', 'stale']) {
  it(`refuses a ${receipt} native receipt before copying or verifying the package`, () => {
    const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-native-receipt-'));
    try {
      for (const dir of ['scripts', 'crates/aac-status/prebuilt', 'web-dashboard/public'])
        fs.mkdirSync(path.join(fixture, dir), { recursive: true });
      fs.copyFileSync(
        path.join(root, 'scripts/build-status-native.js'),
        path.join(fixture, 'scripts/build-status-native.js')
      );
      for (const name of ['Cargo.toml', 'build.rs', 'rust-toolchain.toml', 'src'])
        fs.cpSync(
          path.join(root, 'crates/aac-status', name),
          path.join(fixture, 'crates/aac-status', name),
          { recursive: true }
        );
      fs.copyFileSync(
        path.join(root, 'web-dashboard/public/computers.json'),
        path.join(fixture, 'web-dashboard/public/computers.json')
      );
      // The number layer the crate composes with is a build input too (L3).
      for (const name of ['Cargo.toml', 'Cargo.lock'])
        fs.copyFileSync(path.join(root, name), path.join(fixture, name));
      for (const name of ['Cargo.toml', 'src'])
        fs.cpSync(
          path.join(root, 'crates/aac-num', name),
          path.join(fixture, 'crates/aac-num', name),
          {
            recursive: true,
          }
        );
      // So is the one file policy it reads the saved Codex login with (L2).
      for (const input of [
        'crates/aac-files/Cargo.toml',
        'crates/aac-files/src',
        'crates/aac-core/Cargo.toml',
        'crates/aac-core/src',
        'config/private-files.json',
        'tests/spec/wire-words.json',
        'native/shared/run_corrections.rs',
      ])
        fs.cpSync(path.join(root, input), path.join(fixture, input), { recursive: true });
      const builder = require(path.join(fixture, 'scripts/build-status-native.js'));
      const prebuilt = path.join(fixture, 'crates/aac-status/prebuilt');
      const file = path.join(prebuilt, builder.moduleName());
      fs.writeFileSync(file, 'module must be rejected before loading');
      if (receipt === 'stale')
        fs.writeFileSync(`${file}.json`, JSON.stringify({ sourceSha256: 'old-registry' }));
      expect(() => builder.copyToDist()).toThrow('Rebuild it with: bun run status:build');
      expect(fs.existsSync(path.join(fixture, 'dist/native', builder.moduleName()))).toBe(false);
      expect(() => verifyStatusModule(prebuilt)).toThrow('Rebuild it with: bun run status:build');
    } finally {
      fs.rmSync(fixture, { recursive: true, force: true });
    }
  });
}

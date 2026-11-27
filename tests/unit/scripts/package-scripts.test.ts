import { describe, expect, test } from 'bun:test';
import fs from 'node:fs';
import path from 'node:path';

const root = path.resolve(import.meta.dir, '../../..');
const scripts: Record<string, string> = JSON.parse(
  fs.readFileSync(path.join(root, 'package.json'), 'utf8')
).scripts;

describe('package scripts', () => {
  test('the store and status checks call cargo inside their crate', () => {
    // rustup reads rust-toolchain.toml from the working folder, not from --manifest-path, so each
    // command starts with a cd into the crate instead of going through a wrapper script.
    for (const [name, crate] of [
      ['store', 'aac-store'],
      ['status', 'aac-status'],
    ]) {
      const suites = `cargo test -p ${crate} --release --locked --no-default-features`;
      expect(scripts[`${name}:test`]).toBe(`cd crates/${crate} && ${suites}`);
      expect(scripts[`${name}:check`]).toBe(
        `cd crates/${crate} && cargo fmt -p ${crate} --check && ` +
          `cargo clippy -p ${crate} --release --all-targets --locked -- -D warnings && ${suites}`
      );
    }
  });

  test('the dashboard starts from the built binary, without a launcher script', () => {
    const dashboard = ['dist', 'native', 'ai-account-center'].join('/');
    expect(scripts.dev).toBe(`bun run build && ${dashboard} dashboard --no-open`);
    expect(scripts['ui:preview']).toBe(`${dashboard} dashboard`);
  });

  test('every script file a package command names exists', () => {
    for (const command of Object.values(scripts)) {
      for (const [, file] of command.matchAll(/(scripts\/[\w./-]+\.(?:[cm]?js|sh|py))/g)) {
        expect(fs.existsSync(path.join(root, file))).toBe(true);
      }
    }
  });
});

import { expect, it } from 'bun:test';
import { spawnSync } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';

const root = path.resolve(import.meta.dir, '../../..');
// The fake cargo is a POSIX shell script.
const itPosix = process.platform === 'win32' ? it.skip : it;

/** Two fixture checkouts sharing one fake cargo on PATH; each carries the wrapper + guard. */
function fixture() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-cargo-target-'));
  const cargoLog = path.join(dir, 'cargo.log');
  fs.mkdirSync(path.join(dir, 'bin'), { recursive: true });
  fs.writeFileSync(
    path.join(dir, 'bin', 'cargo'),
    `#!/bin/sh\necho "$@" >> "${cargoLog}"\nexit 0\n`,
    { mode: 0o755 }
  );
  const checkouts = ['a', 'b'].map((name) => {
    const checkout = path.join(dir, name);
    fs.mkdirSync(path.join(checkout, 'scripts', 'foundation'), { recursive: true });
    fs.mkdirSync(path.join(checkout, 'crates', 'aac-store'), { recursive: true });
    fs.copyFileSync(
      path.join(root, 'scripts', 'store-cargo.js'),
      path.join(checkout, 'scripts', 'store-cargo.js')
    );
    fs.copyFileSync(
      path.join(root, 'scripts', 'foundation', 'cargo-target-guard.js'),
      path.join(checkout, 'scripts', 'foundation', 'cargo-target-guard.js')
    );
    return checkout;
  });
  return { dir, cargoLog, checkouts };
}

function run(checkout: string, dir: string, target: string | null) {
  const env: Record<string, string> = {
    ...process.env,
    PATH: `${path.join(dir, 'bin')}:${process.env.PATH}`,
  };
  if (target) env.CARGO_TARGET_DIR = target;
  else delete env.CARGO_TARGET_DIR;
  return spawnSync(process.execPath, [path.join(checkout, 'scripts', 'store-cargo.js'), 'test'], {
    cwd: checkout,
    env,
    encoding: 'utf8',
  });
}

const cargoCalls = (cargoLog: string) =>
  fs.existsSync(cargoLog) ? fs.readFileSync(cargoLog, 'utf8').trim().split('\n').length : 0;

itPosix('refuses a CARGO_TARGET_DIR created by another checkout, and allows the owner', () => {
  const { dir, cargoLog, checkouts } = fixture();
  const target = path.join(dir, 'shared-target');
  try {
    const first = run(checkouts[0], dir, target);
    expect(first.status).toBe(0);
    expect(cargoCalls(cargoLog)).toBe(1);
    expect(fs.readFileSync(path.join(target, '.aac-cargo-checkout'), 'utf8').trim()).toBe(
      checkouts[0]
    );

    // The second checkout must be refused before cargo runs: a shared folder silently
    // reuses the first checkout's build (the stale-pass incidents).
    const second = run(checkouts[1], dir, target);
    expect(second.status).toBe(1);
    expect(second.stderr).toContain(checkouts[0]);
    expect(second.stderr).toContain(checkouts[1]);
    expect(cargoCalls(cargoLog)).toBe(1);

    // The owning checkout keeps its incremental reuse.
    const again = run(checkouts[0], dir, target);
    expect(again.status).toBe(0);
    expect(cargoCalls(cargoLog)).toBe(2);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

itPosix('leaves the workspace target alone when CARGO_TARGET_DIR is unset', () => {
  const { dir, cargoLog, checkouts } = fixture();
  try {
    const result = run(checkouts[0], dir, null);
    expect(result.status).toBe(0);
    expect(cargoCalls(cargoLog)).toBe(1);
    expect(fs.existsSync(path.join(checkouts[0], '.aac-cargo-checkout'))).toBe(false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
import { expect, it } from 'bun:test';
import { spawnSync } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';

const root = path.resolve(import.meta.dir, '../../..');
// The fake cargo is a POSIX shell script; Windows has no /bin/sh contract here.
const itPosix = process.platform === 'win32' ? it.skip : it;

/** A fixture checkout with the builder, its release-profile sibling and a fake cargo on PATH. */
function fixture() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-dashboard-native-'));
  fs.mkdirSync(path.join(dir, 'scripts'), { recursive: true });
  for (const name of ['build-dashboard-native.js', 'release-profile.js'])
    fs.copyFileSync(path.join(root, 'scripts', name), path.join(dir, 'scripts', name));
  fs.mkdirSync(path.join(dir, 'bin'), { recursive: true });
  const cargoLog = path.join(dir, 'cargo.log');
  fs.writeFileSync(
    path.join(dir, 'bin', 'cargo'),
    `#!/bin/sh\necho "$@" >> "${cargoLog}"\nexit 0\n`,
    { mode: 0o755 }
  );
  return { dir, cargoLog };
}

function run(dir: string, env: Record<string, string>) {
  return spawnSync(process.execPath, [path.join(dir, 'scripts', 'build-dashboard-native.js')], {
    cwd: dir,
    env: {
      ...process.env,
      PATH: `${path.join(dir, 'bin')}:${process.env.PATH}`,
      CARGO_TARGET_DIR: path.join(dir, 'target'),
      ...env,
    },
    encoding: 'utf8',
  });
}

function releaseArtifacts(dir: string) {
  fs.mkdirSync(path.join(dir, 'target', 'release'), { recursive: true });
  for (const name of ['aac-http', 'ccs'])
    fs.writeFileSync(path.join(dir, 'target', 'release', name), name);
}

itPosix('never cargos under AAC_NO_LOCAL_RUST_BUILD=1 and skips the stage without artifacts', () => {
  const { dir, cargoLog } = fixture();
  try {
    const result = run(dir, { AAC_NO_LOCAL_RUST_BUILD: '1' });
    expect(result.status).toBe(0);
    expect(fs.existsSync(cargoLog)).toBe(false);
    expect(result.stderr + result.stdout).toContain('AAC_NO_LOCAL_RUST_BUILD=1');
    expect(fs.existsSync(path.join(dir, 'dist', 'native'))).toBe(false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

itPosix('copies existing artifacts under AAC_NO_LOCAL_RUST_BUILD=1 without cargo', () => {
  const { dir, cargoLog } = fixture();
  try {
    releaseArtifacts(dir);
    const result = run(dir, { AAC_NO_LOCAL_RUST_BUILD: '1' });
    expect(result.status).toBe(0);
    expect(fs.existsSync(cargoLog)).toBe(false);
    for (const name of ['aac-http', 'ai-account-center', 'ccs'])
      expect(fs.readFileSync(path.join(dir, 'dist', 'native', name), 'utf8')).toBe(
        name === 'ai-account-center' ? 'ccs' : name
      );
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

itPosix('still builds with cargo when the opt-out is not set', () => {
  const { dir, cargoLog } = fixture();
  try {
    releaseArtifacts(dir);
    const result = run(dir, {});
    expect(result.status).toBe(0);
    expect(fs.readFileSync(cargoLog, 'utf8')).toContain('build --release --locked');
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
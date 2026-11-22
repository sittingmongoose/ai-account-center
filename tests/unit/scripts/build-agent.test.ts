import { expect, test } from 'bun:test';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const { verifyDist, sourceHash, CRATES, EMBEDDED } = require('../../../scripts/build-agent.js');
const catalog = require('../../../config/agent-artifacts.json') as {
  artifacts: { target: string; file: string }[];
};

test('the bundle agent verifier reads the active catalog', () => {
  const dir = mkdtempSync(join(tmpdir(), 'aac-agent-package-'));
  try {
    writeFileSync(join(dir, 'manifest.json'), JSON.stringify({ name: 'aac-agent', targets: {} }));
    expect(verifyDist(dir)).toEqual({
      present: true,
      targets: [],
      missing: catalog.artifacts
        .filter((artifact) => artifact.file.startsWith('dist/agent/'))
        .map((artifact) => artifact.target),
    });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('changing only the Antigravity column table changes the agent source hash', () => {
  const dir = mkdtempSync(join(tmpdir(), 'aac-agent-fingerprint-'));
  try {
    const stub = (rel: string, content: string) => {
      mkdirSync(join(dir, rel, '..'), { recursive: true });
      writeFileSync(join(dir, rel), content);
    };
    for (const rel of ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/config.toml'])
      stub(rel, 'stub\n');
    for (const crate of CRATES) mkdirSync(join(dir, crate, 'src'), { recursive: true });
    for (const rel of EMBEDDED) stub(rel, 'stub\n');
    const table = 'config/tables/antigravity-columns.json';
    const real = readFileSync(join(__dirname, '../../..', table), 'utf8');
    writeFileSync(join(dir, table), real);
    const before = sourceHash(dir);
    const changed = JSON.parse(real);
    changed.columns[0].label = `${changed.columns[0].label} (probe)`;
    writeFileSync(join(dir, table), JSON.stringify(changed, null, 2));
    expect(sourceHash(dir)).not.toBe(before);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

import { expect, test } from 'bun:test';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const { verifyDist } = require('../../../scripts/build-agent.js');
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

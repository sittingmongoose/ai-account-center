import { afterEach, describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

// eslint-disable-next-line @typescript-eslint/no-require-imports
const build = require('../../../scripts/build-usage-scan.js') as {
  verifyDist: (dir: string) => { present: boolean; targets: string[] };
  binaryName: (target: string) => string;
};

let dir: string | undefined;
afterEach(() => {
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
});

describe('the packaged usage-log scanners', () => {
  it('are shipped only as their manifest names them, and an altered one fails the bundle', () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-usage-dist-'));
    const target = 'x86_64-unknown-linux-gnu';
    fs.mkdirSync(path.join(dir, target));
    const body = Buffer.from('#!/bin/sh\n');
    fs.writeFileSync(path.join(dir, target, build.binaryName(target)), body);
    const manifest = {
      name: 'aac-usage-scan',
      version: '0.1.0',
      protocol: 3,
      targets: {
        [target]: {
          file: `${target}/${build.binaryName(target)}`,
          sha256: createHash('sha256').update(body).digest('hex'),
          size: body.length,
        },
      },
    };
    fs.writeFileSync(path.join(dir, 'manifest.json'), JSON.stringify(manifest));
    expect(build.verifyDist(dir)).toEqual({ present: true, targets: [target] });
    fs.writeFileSync(path.join(dir, target, build.binaryName(target)), '#!/bin/sh\necho altered\n');
    expect(() => build.verifyDist(dir as string)).toThrow(/does not match its manifest/);
    expect(build.binaryName('x86_64-pc-windows-msvc')).toBe('aac-usage-scan.exe');
  });
});

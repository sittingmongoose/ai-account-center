import { describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import { packagedSchemasDir } from '../../../../src/web-server/status/agent-reports-reader';

// The machines the reader accepts (crates/aac-status/src/computers.rs). Every published schema
// must let all of them write: a schema missing one refuses that computer's reports outright.
const MACHINES = ['alexis', 'mac', 'nas1', 'odroid', 'vm', 'windows'];

function machineGroups(node: unknown, out: string[][]): string[][] {
  if (Array.isArray(node)) {
    for (const item of node) machineGroups(item, out);
  } else if (node && typeof node === 'object') {
    for (const [key, value] of Object.entries(node)) {
      if (key === 'pattern' && typeof value === 'string') {
        const group = /\((vm\|[a-z0-9|]+)\)/.exec(value);
        if (group) out.push(group[1].split('|').sort());
      } else {
        machineGroups(value, out);
      }
    }
  }
  return out;
}

describe('published report schemas', () => {
  it('let every known machine write', () => {
    const dir = packagedSchemasDir();
    let checked = 0;
    for (const name of fs.readdirSync(dir).filter((n) => n.endsWith('.json'))) {
      const groups = machineGroups(JSON.parse(fs.readFileSync(path.join(dir, name), 'utf8')), []);
      for (const group of groups) {
        expect({ schema: name, machines: group }).toEqual({ schema: name, machines: MACHINES });
        checked += 1;
      }
    }
    expect(checked).toBeGreaterThanOrEqual(5);
  });
});

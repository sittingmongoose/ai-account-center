/** Opt-in output-limit configuration reads preserve absent values and configured subsets. */

import { afterEach, beforeEach, describe, expect, it } from 'bun:test';

import * as fs from 'fs';

import * as os from 'os';

import * as path from 'path';

function createTestHome(configYaml: string): string {
  const tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-output-limits-'));
  const ccsDir = path.join(tempHome, '.ccs');
  fs.mkdirSync(ccsDir, { recursive: true });
  fs.writeFileSync(path.join(ccsDir, 'config.yaml'), configYaml, 'utf8');
  return tempHome;
}

async function importFacade(): Promise<typeof import('../config-loader-facade')> {
  return import(`../config-loader-facade?cachebust=${Date.now()}-${Math.random()}`);
}

describe('getOutputLimitsEnv (config.runtime.outputLimits)', () => {
  let tempHome: string;
  let originalCcsHome: string | undefined;

  beforeEach(() => {
    originalCcsHome = process.env.CCS_HOME;
  });

  afterEach(() => {
    if (originalCcsHome !== undefined) {
      process.env.CCS_HOME = originalCcsHome;
    } else {
      delete process.env.CCS_HOME;
    }
    if (tempHome && fs.existsSync(tempHome)) {
      fs.rmSync(tempHome, { recursive: true, force: true });
    }
  });

  it('injects nothing when no runtime section is present (defaults preserved)', async () => {
    tempHome = createTestHome(`version: 1\n`);
    process.env.CCS_HOME = tempHome;
    const facade = await importFacade();
    expect(facade.getOutputLimitsEnv()).toEqual({});
  });

  it('injects only configured keys as strings', async () => {
    tempHome = createTestHome(
      [
        'version: 1',
        'runtime:',
        '  outputLimits:',
        '    maxMcpOutputTokens: 100000',
        '    bashMaxOutputLength: 200000',
        '',
      ].join('\n')
    );
    process.env.CCS_HOME = tempHome;
    const facade = await importFacade();
    const env = facade.getOutputLimitsEnv();
    expect(env).toEqual({
      MAX_MCP_OUTPUT_TOKENS: '100000',
      BASH_MAX_OUTPUT_LENGTH: '200000',
    });
    for (const value of Object.values(env)) {
      expect(typeof value).toBe('string');
    }
  });

  it('injects only the configured subset', async () => {
    tempHome = createTestHome(
      ['version: 1', 'runtime:', '  outputLimits:', '    maxMcpOutputTokens: 50000', ''].join('\n')
    );
    process.env.CCS_HOME = tempHome;
    const facade = await importFacade();
    const env = facade.getOutputLimitsEnv();
    expect(env).toEqual({ MAX_MCP_OUTPUT_TOKENS: '50000' });
    expect(env).not.toHaveProperty('BASH_MAX_OUTPUT_LENGTH');
  });

  it('injects nothing when runtime.outputLimits is empty', async () => {
    tempHome = createTestHome(['version: 1', 'runtime:', '  outputLimits: {}', ''].join('\n'));
    process.env.CCS_HOME = tempHome;
    const facade = await importFacade();
    expect(facade.getOutputLimitsEnv()).toEqual({});
  });
});

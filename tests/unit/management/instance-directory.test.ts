import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

import {
  listAccountInstanceNames,
  listAccountInstancePaths,
} from '../../../src/management/instance-directory';

describe('account instance directory enumeration', () => {
  let tempRoot = '';
  let originalCcsHome: string | undefined;
  let originalCcsDir: string | undefined;

  const ccsDir = () => path.join(tempRoot, '.ccs');
  const instancesDir = () => path.join(ccsDir(), 'instances');

  beforeEach(() => {
    tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-instance-directory-test-'));
    originalCcsHome = process.env.CCS_HOME;
    originalCcsDir = process.env.CCS_DIR;

    process.env.CCS_HOME = tempRoot;
    delete process.env.CCS_DIR;
    spyOn(os, 'homedir').mockReturnValue(tempRoot);
    spyOn(console, 'log').mockImplementation(() => {});
  });

  afterEach(() => {
    mock.restore();

    if (originalCcsHome !== undefined) process.env.CCS_HOME = originalCcsHome;
    else delete process.env.CCS_HOME;

    if (originalCcsDir !== undefined) process.env.CCS_DIR = originalCcsDir;
    else delete process.env.CCS_DIR;

    if (tempRoot && fs.existsSync(tempRoot)) {
      fs.rmSync(tempRoot, { recursive: true, force: true });
    }
  });

  it('lists only user account instance directories', () => {
    fs.mkdirSync(path.join(instancesDir(), 'work'), { recursive: true });
    fs.mkdirSync(path.join(instancesDir(), 'personal'), { recursive: true });
    fs.mkdirSync(path.join(instancesDir(), '.locks'), { recursive: true });
    fs.mkdirSync(path.join(instancesDir(), '.cache'), { recursive: true });
    fs.writeFileSync(path.join(instancesDir(), 'README.txt'), 'not an instance', 'utf8');

    expect(listAccountInstanceNames(instancesDir()).sort()).toEqual(['personal', 'work']);
    expect(listAccountInstancePaths(instancesDir()).sort()).toEqual(
      [path.join(instancesDir(), 'personal'), path.join(instancesDir(), 'work')].sort()
    );
  });

  it('skips transient entries that cannot be statted', () => {
    fs.mkdirSync(path.join(instancesDir(), 'work'), { recursive: true });
    fs.symlinkSync(
      path.join(instancesDir(), 'missing-instance'),
      path.join(instancesDir(), 'transient'),
      'dir'
    );

    expect(listAccountInstanceNames(instancesDir())).toEqual(['work']);
  });

  it('rejects instance entries that symlink outside the managed root', () => {
    const externalInstance = path.join(tempRoot, 'external-instance');
    fs.mkdirSync(externalInstance, { recursive: true });
    fs.mkdirSync(instancesDir(), { recursive: true });
    fs.symlinkSync(externalInstance, path.join(instancesDir(), 'escaped'), 'dir');

    expect(listAccountInstanceNames(instancesDir())).toEqual([]);
    expect(listAccountInstancePaths(instancesDir())).toEqual([]);
  });

  it('rejects an instances root that is itself a symlink', () => {
    const externalInstances = path.join(tempRoot, 'external-instances');
    fs.mkdirSync(path.join(externalInstances, 'work'), { recursive: true });
    fs.mkdirSync(path.dirname(instancesDir()), { recursive: true });
    fs.symlinkSync(externalInstances, instancesDir(), 'dir');

    expect(listAccountInstanceNames(instancesDir())).toEqual([]);
    expect(listAccountInstancePaths(instancesDir())).toEqual([]);
  });
});

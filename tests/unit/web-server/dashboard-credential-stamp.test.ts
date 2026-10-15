/**
 * Sessions now survive a restart, so a sign-in credential changed outside the dashboard (with
 * `ccs config auth setup` or in the environment) must still sign earlier sessions out, the way a
 * restart used to. Temporary CCS_HOME only; credentials come from the environment.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { withAuthWriteGate } from '../../../src/web-server/services/dashboard-auth-files';
import {
  currentSessionEpoch,
  resetDashboardAuthStateForTests,
  saveCredentialStamp,
  settleCredentialStamp,
} from '../../../src/web-server/services/dashboard-auth-state';

const NAMES = [
  'CCS_HOME',
  'CCS_DIR',
  'CCS_DASHBOARD_AUTH_ENABLED',
  'CCS_DASHBOARD_USERNAME',
  'CCS_DASHBOARD_PASSWORD_HASH',
] as const;
const HASH_A = `$2b$04$${'a'.repeat(53)}`;
const HASH_B = `$2b$04$${'b'.repeat(53)}`;

let home = '';
let saved: Record<string, string | undefined> = {};

beforeEach(() => {
  saved = Object.fromEntries(NAMES.map((name) => [name, process.env[name]]));
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-credential-stamp-'));
  delete process.env.CCS_DIR;
  process.env.CCS_HOME = home;
  fs.mkdirSync(path.join(home, '.ccs'));
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
  process.env.CCS_DASHBOARD_USERNAME = 'owner';
  process.env.CCS_DASHBOARD_PASSWORD_HASH = HASH_A;
  resetDashboardAuthStateForTests();
});

afterEach(() => {
  for (const name of NAMES) {
    if (saved[name] === undefined) delete process.env[name];
    else process.env[name] = saved[name];
  }
  fs.rmSync(home, { recursive: true, force: true });
  resetDashboardAuthStateForTests();
});

describe('credential changes made outside the dashboard', () => {
  it('records the first credential without signing anyone out', async () => {
    await settleCredentialStamp();
    expect(currentSessionEpoch()).toBe(0);
    expect(fs.existsSync(path.join(home, '.ccs', 'auth', 'credential.json'))).toBe(true);
    await settleCredentialStamp();
    expect(currentSessionEpoch()).toBe(0);
  });

  it('signs every earlier session out at startup after the password changed meanwhile', async () => {
    await settleCredentialStamp();
    process.env.CCS_DASHBOARD_PASSWORD_HASH = HASH_B;
    await settleCredentialStamp();
    const after = currentSessionEpoch();
    expect(after).not.toBe(0);
    // The new credential is recorded: the next start changes nothing.
    await settleCredentialStamp();
    expect(currentSessionEpoch()).toBe(after);
  });

  it("leaves sessions alone after the dashboard's own password change", async () => {
    await settleCredentialStamp();
    process.env.CCS_DASHBOARD_PASSWORD_HASH = HASH_B;
    // What the password change route does inside the auth write gate.
    await withAuthWriteGate(saveCredentialStamp);
    await settleCredentialStamp();
    expect(currentSessionEpoch()).toBe(0);
  });

  it('signs earlier sessions out when sign-in was turned off and on again meanwhile', async () => {
    await settleCredentialStamp();
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
    await settleCredentialStamp();
    const off = currentSessionEpoch();
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
    await settleCredentialStamp();
    expect(currentSessionEpoch()).not.toBe(off);
  });

  it('does nothing before sign-in is set up', async () => {
    delete process.env.CCS_DASHBOARD_USERNAME;
    delete process.env.CCS_DASHBOARD_PASSWORD_HASH;
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
    await settleCredentialStamp();
    expect(fs.existsSync(path.join(home, '.ccs', 'auth', 'credential.json'))).toBe(false);
  });
});

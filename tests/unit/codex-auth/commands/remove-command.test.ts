/**
 * Tests for codex-auth remove command.
 */
import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

let tempDir: string;
let ccsHome: string;
let codexHome: string;
const ORIG_CCS_HOME = process.env.CCS_HOME;
const ORIG_CCS_CODEX_PROFILE = process.env.CCS_CODEX_PROFILE;

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codex-remove-test-'));
  ccsHome = path.join(tempDir, 'ccs');
  codexHome = path.join(tempDir, 'native-codex');
  fs.mkdirSync(codexHome);
  fs.mkdirSync(path.join(ccsHome, '.ccs'), { recursive: true });
  process.env.CCS_HOME = ccsHome;
  delete process.env.CCS_CODEX_PROFILE;
});

afterEach(() => {
  if (ORIG_CCS_HOME === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = ORIG_CCS_HOME;
  if (ORIG_CCS_CODEX_PROFILE === undefined) delete process.env.CCS_CODEX_PROFILE;
  else process.env.CCS_CODEX_PROFILE = ORIG_CCS_CODEX_PROFILE;
  fs.rmSync(tempDir, { recursive: true, force: true });
  mock.restore();
});

async function makeCtx(...names: string[]) {
  const { CodexProfileRegistry } = await import(
    '../../../../src/codex-auth/codex-profile-registry'
  );
  const reg = new CodexProfileRegistry();
  for (const n of names) {
    reg.createProfile(n, { created: new Date().toISOString(), last_used: null });
    // Create the profile dir too
    const dir = path.join(ccsHome, '.ccs', 'codex-instances', n);
    fs.mkdirSync(dir, { recursive: true });
  }
  return { registry: reg, version: '0.0.0-test' };
}

function mockConfirmYes() {
  return import('../../../../src/utils/prompt').then((mod) => {
    spyOn(mod.InteractivePrompt, 'confirm').mockResolvedValue(true);
  });
}

function mockConfirmNo() {
  return import('../../../../src/utils/prompt').then((mod) => {
    spyOn(mod.InteractivePrompt, 'confirm').mockResolvedValue(false);
  });
}

// ── non-default removes cleanly ───────────────────────────────────────────────

describe('handleRemoveCodex — normal removal', () => {
  it('removes a non-default profile cleanly', async () => {
    await mockConfirmYes();
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('alpha', 'beta');
    ctx.registry.setDefault('alpha');

    const out: string[] = [];
    const origLog = console.log;
    console.log = (...a: unknown[]) => out.push(a.join(' '));
    try {
      await handleRemoveCodex(ctx, ['beta', '--yes'], { codexHome });
    } finally {
      console.log = origLog;
    }

    expect(ctx.registry.hasProfile('beta')).toBe(false);
    expect(out.some((l) => l.includes('removed'))).toBe(true);
  });
});

// ── default with others → refuses without --force ────────────────────────────

describe('handleRemoveCodex — default guard', () => {
  it('refuses to remove default when others exist without --force', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('alpha', 'beta');
    ctx.registry.setDefault('alpha');

    let exitCode = -1;
    const origExit = process.exit;
    process.exit = (code?: number) => {
      exitCode = code ?? 0;
      throw new Error('exit');
    };

    const out: string[] = [];
    const origLog = console.log;
    console.log = (...a: unknown[]) => out.push(a.join(' '));
    try {
      await handleRemoveCodex(ctx, ['alpha'], { codexHome });
    } catch {
      /* process.exit */
    } finally {
      process.exit = origExit;
      console.log = origLog;
    }

    expect(exitCode).toBeGreaterThan(0);
    expect(ctx.registry.hasProfile('alpha')).toBe(true); // not removed
    // Hint lines still go to stdout; user-facing error now goes to stderr via exitWithError
    expect(out.some((l) => l.includes('Saved default protection'))).toBe(true);
  });

  it('allows removal of default with --force', async () => {
    await mockConfirmYes();
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('alpha', 'beta');
    ctx.registry.setDefault('alpha');

    await handleRemoveCodex(ctx, ['alpha', '--force', '--yes'], { codexHome });
    expect(ctx.registry.hasProfile('alpha')).toBe(false);
  });

  it('restores data when the target becomes default between precheck and registry write', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('alpha', 'beta');
    ctx.registry.setDefault('beta');
    const profileDir = path.join(ccsHome, '.ccs', 'codex-instances', 'alpha');
    const authJsonPath = path.join(profileDir, 'auth.json');
    fs.writeFileSync(authJsonPath, JSON.stringify({ tokens: { id_token: 'h.e30K.s' } }));

    const realCp = fs.promises.cp;
    spyOn(fs.promises, 'cp').mockImplementation(async (src, dest, options) => {
      const result = await realCp(src, dest, options);
      if (typeof dest === 'string' && dest.includes('.preserved.')) {
        ctx.registry.setDefault('alpha');
      }
      return result;
    });

    let exitCode = -1;
    const origExit = process.exit;
    const origErr = console.error;
    process.exit = (code?: number) => {
      exitCode = code ?? 0;
      throw new Error('exit');
    };
    console.error = () => {};

    try {
      await handleRemoveCodex(ctx, ['alpha', '--yes'], { codexHome });
    } catch {
      /* process.exit */
    } finally {
      process.exit = origExit;
      console.error = origErr;
    }

    expect(exitCode).toBeGreaterThan(0);
    expect(fs.existsSync(authJsonPath)).toBe(true);
    expect(ctx.registry.hasProfile('alpha')).toBe(true);
    expect(ctx.registry.getDefault()).toBe('alpha');
  });
});

// ── only profile → allows removal ────────────────────────────────────────────

describe('handleRemoveCodex — only profile', () => {
  it('allows removal of the only profile (even if default)', async () => {
    await mockConfirmYes();
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('solo');
    ctx.registry.setDefault('solo');

    await handleRemoveCodex(ctx, ['solo', '--yes'], { codexHome });
    expect(ctx.registry.hasProfile('solo')).toBe(false);
  });
});

// ── confirmation prompt ───────────────────────────────────────────────────────

describe('handleRemoveCodex — confirmation', () => {
  it('rejects extra positional arguments before deleting anything', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('work');

    let exitCode = -1;
    const err: string[] = [];
    const origExit = process.exit;
    const origError = console.error;
    const origWrite = process.stderr.write.bind(process.stderr);
    process.exit = (code?: number) => {
      exitCode = code ?? 0;
      throw new Error('exit');
    };
    console.error = (...a: unknown[]) => err.push(a.join(' '));
    process.stderr.write = (chunk: string | Uint8Array) => {
      err.push(String(chunk));
      return true;
    };

    try {
      await handleRemoveCodex(ctx, ['work', 'accidental', '--yes'], { codexHome });
    } catch {
      /* process.exit */
    } finally {
      process.exit = origExit;
      console.error = origError;
      process.stderr.write = origWrite;
    }

    expect(exitCode).toBeGreaterThan(0);
    expect(ctx.registry.hasProfile('work')).toBe(true);
    expect(err.join('')).toContain('Unexpected arguments: "accidental"');
  });

  it('cancels when user declines confirmation', async () => {
    await mockConfirmNo();
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('keepme');

    const out: string[] = [];
    const origLog = console.log;
    console.log = (...a: unknown[]) => out.push(a.join(' '));
    try {
      await handleRemoveCodex(ctx, ['keepme'], { codexHome }); // no --yes
    } finally {
      console.log = origLog;
    }

    expect(ctx.registry.hasProfile('keepme')).toBe(true); // not removed
    expect(out.some((l) => l.includes('Cancelled'))).toBe(true);
  });

  it('--yes skips prompt entirely', async () => {
    // No mock — if prompt were called it would hang/throw in test
    const promptMod = await import('../../../../src/utils/prompt');
    let promptCalled = false;
    spyOn(promptMod.InteractivePrompt, 'confirm').mockImplementation(async () => {
      promptCalled = true;
      return true;
    });

    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('skipconfirm');

    const origLog = console.log;
    console.log = () => {};
    try {
      await handleRemoveCodex(ctx, ['skipconfirm', '--yes'], { codexHome });
    } finally {
      console.log = origLog;
    }

    expect(promptCalled).toBe(false);
    expect(ctx.registry.hasProfile('skipconfirm')).toBe(false);
  });

  it('preserves profile data when registry removal fails', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('preserveme');
    const profileDir = path.join(ccsHome, '.ccs', 'codex-instances', 'preserveme');
    const authJsonPath = path.join(profileDir, 'auth.json');
    fs.writeFileSync(authJsonPath, JSON.stringify({ tokens: { id_token: 'h.e30K.s' } }));

    spyOn(ctx.registry, 'removeProfile').mockImplementation(() => {
      throw new Error('registry write denied');
    });

    let exitCode = -1;
    const origExit = process.exit;
    const origErr = console.error;
    process.exit = (code?: number) => {
      exitCode = code ?? 0;
      throw new Error('exit');
    };
    console.error = () => {};

    try {
      await handleRemoveCodex(ctx, ['preserveme', '--yes'], { codexHome });
    } catch {
      /* process.exit */
    } finally {
      process.exit = origExit;
      console.error = origErr;
    }

    expect(exitCode).toBeGreaterThan(0);
    expect(fs.existsSync(authJsonPath)).toBe(true);
    expect(ctx.registry.hasProfile('preserveme')).toBe(true);
  });

  it('cleans a partial preservation copy when delete preparation fails', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('copyfail');
    const profileDir = path.join(ccsHome, '.ccs', 'codex-instances', 'copyfail');
    const parentDir = path.dirname(profileDir);
    const authJsonPath = path.join(profileDir, 'auth.json');
    fs.writeFileSync(authJsonPath, JSON.stringify({ tokens: { id_token: 'h.e30K.s' } }));

    const realCp = fs.promises.cp;
    spyOn(fs.promises, 'cp').mockImplementation(async (src, dest, options) => {
      if (typeof dest === 'string' && dest.includes('.preserved.')) {
        fs.mkdirSync(dest, { recursive: true });
        fs.writeFileSync(path.join(dest, 'auth.json'), '{}');
        throw new Error('copy failed after partial write');
      }
      return realCp(src, dest, options);
    });

    let exitCode = -1;
    const origExit = process.exit;
    const origErr = console.error;
    process.exit = (code?: number) => {
      exitCode = code ?? 0;
      throw new Error('exit');
    };
    console.error = () => {};

    try {
      await handleRemoveCodex(ctx, ['copyfail', '--yes'], { codexHome });
    } catch {
      /* process.exit */
    } finally {
      process.exit = origExit;
      console.error = origErr;
    }

    expect(exitCode).toBeGreaterThan(0);
    expect(fs.existsSync(authJsonPath)).toBe(true);
    expect(ctx.registry.hasProfile('copyfail')).toBe(true);
    expect(fs.readdirSync(parentDir).some((entry) => entry.startsWith('copyfail.preserved.'))).toBe(
      false
    );
  });

  it('restores profile data and registry when final deletion fails', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('restoreme');
    ctx.registry.setDefault('restoreme');
    const profileDir = path.join(ccsHome, '.ccs', 'codex-instances', 'restoreme');
    const authJsonPath = path.join(profileDir, 'auth.json');
    fs.writeFileSync(authJsonPath, JSON.stringify({ tokens: { id_token: 'h.e30K.s' } }));

    const realRm = fs.promises.rm;
    spyOn(fs.promises, 'rm').mockImplementation(async (target, options) => {
      if (typeof target === 'string' && target.includes('.deleting.')) {
        throw new Error('delete denied');
      }
      return realRm(target, options);
    });

    let exitCode = -1;
    const origExit = process.exit;
    const origErr = console.error;
    process.exit = (code?: number) => {
      exitCode = code ?? 0;
      throw new Error('exit');
    };
    console.error = () => {};

    try {
      await handleRemoveCodex(ctx, ['restoreme', '--yes'], { codexHome });
    } catch {
      /* process.exit */
    } finally {
      process.exit = origExit;
      console.error = origErr;
    }

    expect(exitCode).toBeGreaterThan(0);
    expect(fs.existsSync(authJsonPath)).toBe(true);
    expect(ctx.registry.hasProfile('restoreme')).toBe(true);
    expect(ctx.registry.getDefault()).toBe('restoreme');
  });

  it('restores from preserved copy when final deletion partially removes auth.json', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('partialrestore');
    const profileDir = path.join(ccsHome, '.ccs', 'codex-instances', 'partialrestore');
    const authJsonPath = path.join(profileDir, 'auth.json');
    fs.writeFileSync(authJsonPath, JSON.stringify({ tokens: { id_token: 'h.e30K.s' } }));

    const realRm = fs.promises.rm;
    spyOn(fs.promises, 'rm').mockImplementation(async (target, options) => {
      if (typeof target === 'string' && target.includes('.deleting.')) {
        await realRm(path.join(target, 'auth.json'), { force: true });
        throw new Error('delete failed after auth removal');
      }
      return realRm(target, options);
    });

    let exitCode = -1;
    const origExit = process.exit;
    const origErr = console.error;
    process.exit = (code?: number) => {
      exitCode = code ?? 0;
      throw new Error('exit');
    };
    console.error = () => {};

    try {
      await handleRemoveCodex(ctx, ['partialrestore', '--yes'], { codexHome });
    } catch {
      /* process.exit */
    } finally {
      process.exit = origExit;
      console.error = origErr;
    }

    expect(exitCode).toBeGreaterThan(0);
    expect(fs.existsSync(authJsonPath)).toBe(true);
    expect(ctx.registry.hasProfile('partialrestore')).toBe(true);
  });
});

function syntheticAuth(name: 'gmail' | 'platyr'): Buffer {
  return fs.readFileSync(path.join(__dirname, '../fixtures/activation', `${name}.auth.json`));
}

function savedAuth(name: string): string {
  return path.join(ccsHome, '.ccs', 'codex-instances', name, 'auth.json');
}

async function removalFailure(operation: () => Promise<void>) {
  let code = 0;
  let lockedAtExit = false;
  const errors: string[] = [];
  const originalExit = process.exit;
  const originalError = console.error;
  process.exit = (exitCode?: number) => {
    code = exitCode ?? 0;
    lockedAtExit = fs.existsSync(path.join(codexHome, '.ccs-activation.lock'));
    throw new Error('fixture exit');
  };
  console.error = (...args: unknown[]) => errors.push(args.join(' '));
  try {
    await operation();
  } catch (error) {
    if (!(error instanceof Error) || error.message !== 'fixture exit') throw error;
  } finally {
    process.exit = originalExit;
    console.error = originalError;
  }
  expect(code).toBeGreaterThan(0);
  expect(lockedAtExit).toBe(false);
  return errors.join('\n');
}

describe('handleRemoveCodex — native shared-login protection', () => {
  for (const flags of [['--yes'], ['--yes', '--force']]) {
    it(`retains the native current login with ${flags.join(' ')}`, async () => {
      const { handleRemoveCodex } = await import(
        '../../../../src/codex-auth/commands/remove-command'
      );
      const ctx = await makeCtx('primary', 'other');
      ctx.registry.setDefault('other');
      fs.writeFileSync(savedAuth('primary'), syntheticAuth('gmail'));
      fs.writeFileSync(path.join(codexHome, 'auth.json'), syntheticAuth('gmail'));
      // Cached metadata and the legacy env marker do not determine the login.
      ctx.registry.updateProfile('primary', { email: 'stale@example.test' });
      process.env.CCS_CODEX_PROFILE = 'other';
      const message = await removalFailure(() =>
        handleRemoveCodex(ctx, ['primary', ...flags], { codexHome })
      );
      expect(message).toContain('current Codex account');
      expect(ctx.registry.hasProfile('primary')).toBe(true);
      expect(fs.readFileSync(savedAuth('primary'))).toEqual(syntheticAuth('gmail'));
      expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(syntheticAuth('gmail'));
    });
  }

  it('retains a same-email saved alias even when another alias exists', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('original', 'alias');
    fs.writeFileSync(savedAuth('original'), syntheticAuth('gmail'));
    fs.writeFileSync(savedAuth('alias'), syntheticAuth('gmail'));
    fs.writeFileSync(path.join(codexHome, 'auth.json'), syntheticAuth('gmail'));
    await removalFailure(() => handleRemoveCodex(ctx, ['alias', '--yes'], { codexHome }));
    expect(ctx.registry.hasProfile('alias')).toBe(true);
  });

  it('removes a fresh inactive login without modifying native auth, config or history', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('primary', 'inactive');
    fs.writeFileSync(savedAuth('primary'), syntheticAuth('gmail'));
    fs.writeFileSync(savedAuth('inactive'), syntheticAuth('platyr'));
    fs.writeFileSync(path.join(codexHome, 'auth.json'), syntheticAuth('gmail'));
    fs.writeFileSync(path.join(codexHome, 'config.toml'), 'keep config');
    fs.mkdirSync(path.join(codexHome, 'sessions'));
    fs.writeFileSync(path.join(codexHome, 'sessions', 'history.jsonl'), 'keep history');
    await handleRemoveCodex(ctx, ['inactive', '--yes'], { codexHome });
    expect(ctx.registry.hasProfile('inactive')).toBe(false);
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(syntheticAuth('gmail'));
    expect(fs.readFileSync(path.join(codexHome, 'config.toml'), 'utf8')).toBe('keep config');
    expect(fs.readFileSync(path.join(codexHome, 'sessions', 'history.jsonl'), 'utf8')).toBe(
      'keep history'
    );
  });

  it('rechecks native login after confirmation, without holding the lock during the prompt', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const { InteractivePrompt } = await import('../../../../src/utils/prompt');
    const ctx = await makeCtx('primary', 'target');
    fs.writeFileSync(savedAuth('target'), syntheticAuth('platyr'));
    fs.writeFileSync(path.join(codexHome, 'auth.json'), syntheticAuth('gmail'));
    spyOn(InteractivePrompt, 'confirm').mockImplementation(async () => {
      expect(fs.existsSync(path.join(codexHome, '.ccs-activation.lock'))).toBe(false);
      fs.writeFileSync(path.join(codexHome, 'auth.json'), syntheticAuth('platyr'));
      return true;
    });
    await removalFailure(() => handleRemoveCodex(ctx, ['target'], { codexHome }));
    expect(ctx.registry.hasProfile('target')).toBe(true);
    expect(fs.existsSync(savedAuth('target'))).toBe(true);
  });

  for (const invalid of ['{"tokens":{"id_token":"secret-fragment"', '{}']) {
    it('fails closed for a present malformed/unknown native identity', async () => {
      const { handleRemoveCodex } = await import(
        '../../../../src/codex-auth/commands/remove-command'
      );
      const ctx = await makeCtx('target');
      fs.writeFileSync(savedAuth('target'), syntheticAuth('platyr'));
      fs.writeFileSync(path.join(codexHome, 'auth.json'), invalid);
      const message = await removalFailure(() =>
        handleRemoveCodex(ctx, ['target', '--yes', '--force'], { codexHome })
      );
      expect(message).toContain('Could not verify the current native');
      expect(message).not.toContain('secret-fragment');
      expect(ctx.registry.hasProfile('target')).toBe(true);
      expect(fs.readFileSync(path.join(codexHome, 'auth.json'), 'utf8')).toBe(invalid);
    });
  }

  it('fails closed on unreadable native auth and an unverifiable saved target', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('target');
    const livePath = path.join(codexHome, 'auth.json');
    const realRead = fs.readFileSync;
    spyOn(fs, 'readFileSync').mockImplementation((file, ...args) => {
      if (file === livePath) throw Object.assign(new Error('private detail'), { code: 'EACCES' });
      return realRead(file, ...args);
    });
    const message = await removalFailure(() =>
      handleRemoveCodex(ctx, ['target', '--yes'], { codexHome })
    );
    expect(message).toContain('Could not read the current native');
    expect(message).not.toContain('private detail');
    expect(ctx.registry.hasProfile('target')).toBe(true);
  });

  it('retains a missing or invalid target identity while native auth is present', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('missing', 'invalid');
    ctx.registry.setDefault('invalid');
    fs.writeFileSync(path.join(codexHome, 'auth.json'), syntheticAuth('gmail'));
    await removalFailure(() => handleRemoveCodex(ctx, ['missing', '--yes'], { codexHome }));
    fs.writeFileSync(savedAuth('invalid'), '{}');
    await removalFailure(() =>
      handleRemoveCodex(ctx, ['invalid', '--yes', '--force'], { codexHome })
    );
    expect(ctx.registry.listProfiles().sort()).toEqual(['invalid', 'missing']);
  });

  it('cleans a ghost registry entry when native auth is absent', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('ghost');
    fs.rmSync(path.dirname(savedAuth('ghost')), { recursive: true });
    await handleRemoveCodex(ctx, ['ghost', '--yes'], { codexHome });
    expect(ctx.registry.hasProfile('ghost')).toBe(false);
    expect(fs.existsSync(path.join(codexHome, '.ccs-activation.lock'))).toBe(false);
  });

  it('retains ghost entries while the native login cannot be compared to fresh saved auth', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('ghost');
    ctx.registry.updateProfile('ghost', { email: 'stale-unrelated@example.test' });
    fs.rmSync(path.dirname(savedAuth('ghost')), { recursive: true });
    fs.writeFileSync(path.join(codexHome, 'auth.json'), syntheticAuth('gmail'));
    await removalFailure(() => handleRemoveCodex(ctx, ['ghost', '--yes'], { codexHome }));
    expect(ctx.registry.hasProfile('ghost')).toBe(true);
  });

  it('restores staged data if external native login changes during the preservation copy', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('target');
    fs.writeFileSync(savedAuth('target'), syntheticAuth('platyr'));
    fs.writeFileSync(path.join(codexHome, 'auth.json'), syntheticAuth('gmail'));
    const realCp = fs.promises.cp;
    spyOn(fs.promises, 'cp').mockImplementation(async (src, dest, options) => {
      await realCp(src, dest, options);
      fs.writeFileSync(path.join(codexHome, 'auth.json'), syntheticAuth('platyr'));
    });
    await removalFailure(() => handleRemoveCodex(ctx, ['target', '--yes'], { codexHome }));
    expect(ctx.registry.hasProfile('target')).toBe(true);
    expect(fs.readFileSync(savedAuth('target'))).toEqual(syntheticAuth('platyr'));
  });

  it('holds the common activation lock through asynchronous delete and restoration', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('target');
    fs.writeFileSync(savedAuth('target'), syntheticAuth('platyr'));
    const realCp = fs.promises.cp;
    const realRm = fs.promises.rm;
    spyOn(fs.promises, 'cp').mockImplementation(async (src, dest, options) => {
      expect(fs.existsSync(path.join(codexHome, '.ccs-activation.lock'))).toBe(true);
      await new Promise((resolve) => setTimeout(resolve, 20));
      return realCp(src, dest, options);
    });
    spyOn(fs.promises, 'rm').mockImplementation(async (target, options) => {
      expect(fs.existsSync(path.join(codexHome, '.ccs-activation.lock'))).toBe(true);
      if (typeof target === 'string' && target.includes('.deleting.')) {
        await realRm(path.join(target, 'auth.json'), { force: true });
        throw new Error('fixture delete failure');
      }
      return realRm(target, options);
    });
    await removalFailure(() => handleRemoveCodex(ctx, ['target', '--yes'], { codexHome }));
    expect(ctx.registry.hasProfile('target')).toBe(true);
    expect(fs.readFileSync(savedAuth('target'))).toEqual(syntheticAuth('platyr'));
  });
});

/** A synthetic auth.json bound to one workspace and, optionally, one principal. */
function boundAuth(identity: { accountId: string; userId?: string; email?: string }): Buffer {
  const header = Buffer.from('{"alg":"none"}').toString('base64url');
  const payload = Buffer.from(
    JSON.stringify({
      email: identity.email ?? 'shared@example.test',
      'https://api.openai.com/auth': {
        chatgpt_account_id: identity.accountId,
        ...(identity.userId ? { chatgpt_user_id: identity.userId } : {}),
      },
    })
  ).toString('base64url');
  return Buffer.from(
    JSON.stringify({
      tokens: {
        id_token: `${header}.${payload}.fakesig`,
        access_token: 'synthetic-access',
        refresh_token: 'synthetic-refresh',
        account_id: identity.accountId,
      },
    })
  );
}

/** Run a removal that must succeed; a refusal fails the test instead of exiting the runner. */
async function removed(operation: () => Promise<void>): Promise<void> {
  const errors: string[] = [];
  const originalExit = process.exit;
  const originalError = console.error;
  process.exit = (code?: number) => {
    throw new Error(`removal exited ${code ?? 0}: ${errors.join(' ')}`);
  };
  console.error = (...args: unknown[]) => errors.push(args.join(' '));
  try {
    await operation();
  } finally {
    process.exit = originalExit;
    console.error = originalError;
  }
}

describe('handleRemoveCodex — live login matched by workspace and principal', () => {
  const personal = { accountId: 'personal', userId: 'same-user' };
  const workspace = { accountId: 'workspace', userId: 'same-user' };

  it('removes the inactive login of a personal and a workspace pair under one email', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('personal', 'workspace');
    fs.writeFileSync(savedAuth('personal'), boundAuth(personal));
    fs.writeFileSync(savedAuth('workspace'), boundAuth(workspace));
    fs.writeFileSync(path.join(codexHome, 'auth.json'), boundAuth(workspace));
    await removed(() => handleRemoveCodex(ctx, ['personal', '--yes'], { codexHome }));
    expect(ctx.registry.hasProfile('personal')).toBe(false);
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(boundAuth(workspace));
    // The live workspace login itself stays protected.
    const message = await removalFailure(() =>
      handleRemoveCodex(ctx, ['workspace', '--yes', '--force'], { codexHome })
    );
    expect(message).toContain('current Codex account');
    expect(message).not.toContain('same-user');
    expect(ctx.registry.hasProfile('workspace')).toBe(true);
    expect(fs.readFileSync(savedAuth('workspace'))).toEqual(boundAuth(workspace));
  });

  it('retains a saved copy of the live principal after its display email changed', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('primary', 'renamed');
    fs.writeFileSync(savedAuth('renamed'), boundAuth({ ...workspace, email: 'old@example.test' }));
    fs.writeFileSync(path.join(codexHome, 'auth.json'), boundAuth(workspace));
    await removalFailure(() => handleRemoveCodex(ctx, ['renamed', '--yes'], { codexHome }));
    expect(ctx.registry.hasProfile('renamed')).toBe(true);
  });

  it('retains a legacy saved copy of the live workspace and email without a principal', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('primary', 'legacy');
    fs.writeFileSync(savedAuth('legacy'), boundAuth({ accountId: 'workspace' }));
    fs.writeFileSync(path.join(codexHome, 'auth.json'), boundAuth(workspace));
    await removalFailure(() => handleRemoveCodex(ctx, ['legacy', '--yes'], { codexHome }));
    expect(ctx.registry.hasProfile('legacy')).toBe(true);
  });

  it('falls back to the email when a login carries no readable workspace binding', async () => {
    const { handleRemoveCodex } = await import(
      '../../../../src/codex-auth/commands/remove-command'
    );
    const ctx = await makeCtx('primary', 'unbound', 'other');
    // A JWT without the workspace claim still yields an email, but no binding.
    const header = Buffer.from('{"alg":"none"}').toString('base64url');
    const unbound = (email: string) =>
      JSON.stringify({
        tokens: {
          id_token: `${header}.${Buffer.from(JSON.stringify({ email })).toString('base64url')}.sig`,
        },
      });
    fs.writeFileSync(savedAuth('unbound'), unbound('shared@example.test'));
    fs.writeFileSync(savedAuth('other'), unbound('other@example.test'));
    fs.writeFileSync(path.join(codexHome, 'auth.json'), boundAuth(workspace));
    await removalFailure(() => handleRemoveCodex(ctx, ['unbound', '--yes'], { codexHome }));
    expect(ctx.registry.hasProfile('unbound')).toBe(true);
    await removed(() => handleRemoveCodex(ctx, ['other', '--yes'], { codexHome }));
    expect(ctx.registry.hasProfile('other')).toBe(false);
  });
});

describe('handleRemoveCodex — lock wait and real exit', () => {
  for (const change of ['membership', 'identity'] as const) {
    it(`revalidates target ${change} after waiting for an activation lock`, async () => {
      const { handleRemoveCodex } = await import(
        '../../../../src/codex-auth/commands/remove-command'
      );
      const { acquireCodexActivationLock } = await import(
        '../../../../src/codex-auth/codex-activation-lock'
      );
      const ctx = await makeCtx('primary', 'target');
      fs.writeFileSync(savedAuth('target'), syntheticAuth('platyr'));
      fs.writeFileSync(path.join(codexHome, 'auth.json'), syntheticAuth('gmail'));
      const release = await acquireCodexActivationLock(codexHome);
      let preflightRead!: () => void;
      const preflight = new Promise<void>((resolve) => {
        preflightRead = resolve;
      });
      const realGet = ctx.registry.getProfile.bind(ctx.registry);
      spyOn(ctx.registry, 'getProfile').mockImplementation((name) => {
        const meta = realGet(name);
        preflightRead();
        return meta;
      });
      const pending = removalFailure(() =>
        handleRemoveCodex(ctx, ['target', '--yes'], { codexHome })
      );
      await preflight;
      if (change === 'membership') ctx.registry.removeProfile('target');
      else fs.writeFileSync(savedAuth('target'), syntheticAuth('gmail'));
      await release();
      const message = await pending;
      expect(message).toContain(
        change === 'membership' ? 'Profile not found' : 'current Codex account'
      );
      expect(fs.existsSync(savedAuth('target'))).toBe(true);
      expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(syntheticAuth('gmail'));
    });
  }

  it('releases its lock before a genuine process.exit protecting the sole current profile', async () => {
    const ctx = await makeCtx('sole');
    fs.writeFileSync(savedAuth('sole'), syntheticAuth('gmail'));
    fs.writeFileSync(path.join(codexHome, 'auth.json'), syntheticAuth('gmail'));
    const scriptPath = path.join(tempDir, 'actual-exit.ts');
    const sourceRoot = path.resolve(__dirname, '../../../../src/codex-auth');
    fs.writeFileSync(
      scriptPath,
      `
      import { handleRemoveCodex } from ${JSON.stringify(path.join(sourceRoot, 'commands/remove-command.ts'))};
      import { CodexProfileRegistry } from ${JSON.stringify(path.join(sourceRoot, 'codex-profile-registry.ts'))};
      await handleRemoveCodex(
        { registry: new CodexProfileRegistry(), version: 'test' },
        ['sole', '--yes', '--force'],
        { codexHome: ${JSON.stringify(codexHome)} }
      );
    `
    );
    const child = Bun.spawn([process.execPath, scriptPath], {
      env: { ...process.env, CCS_HOME: ccsHome },
      stdout: 'pipe',
      stderr: 'pipe',
    });
    const [exitCode, , errorText] = await Promise.all([
      child.exited,
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
    ]);
    expect(exitCode).toBe(7);
    expect(errorText).toContain('current Codex account');
    expect(fs.existsSync(path.join(codexHome, '.ccs-activation.lock'))).toBe(false);
    expect(ctx.registry.hasProfile('sole')).toBe(true);
    expect(fs.readFileSync(savedAuth('sole'))).toEqual(syntheticAuth('gmail'));
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(syntheticAuth('gmail'));
  });
});

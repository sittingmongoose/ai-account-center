import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { discoverOmpSessionRoots } from '../../../../src/web-server/usage/omp-session-root-discovery';
import {
  isOmpSessionFilename,
  resolveOmpSessionRoots,
  OMP_SCAN_CACHE_TTL_MS,
} from '../../../../src/web-server/usage/omp-native-usage-collector';

let temporary: string;
beforeEach(() => {
  temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-omp-discovery-'));
});
afterEach(() => fs.rmSync(temporary, { recursive: true, force: true }));

const LOG = '2026-10-01T15-00_fixture.jsonl';
const TTL = 1000;
function fixture() {
  const base = path.join(temporary, 'experiments');
  const cacheDir = path.join(temporary, 'cache');
  const rules = {
    version: 1,
    maxDepth: 4,
    maxRoots: 512,
    ttlMs: TTL,
    skipDirs: new Set<string>(),
    isSessionFilename: isOmpSessionFilename,
    isSandbox: async () => false,
    hasSessionMarker: async (directory: string): Promise<boolean | null> =>
      (await fs.promises.readdir(directory, { withFileTypes: true })).some(
        (entry) => entry.isFile() && isOmpSessionFilename(entry.name)
      ),
  };
  const makeRoot = (name: string) => {
    const root = path.join(base, 'experiment', name);
    fs.mkdirSync(root, { recursive: true });
    fs.writeFileSync(path.join(root, LOG), '{}\n');
    return root;
  };
  return {
    base,
    rules,
    makeRoot,
    scan: (now: number) => discoverOmpSessionRoots(base, { cacheDir, now }, rules),
  };
}

describe('OMP cached root revalidation', () => {
  it.each(['worker-sessions', path.join('worker', 'sessions')])(
    'releases a pruned %s root at the 512-root cap before rediscovery',
    async (suffix) => {
      const { makeRoot, scan } = fixture();
      const roots = Array.from({ length: 512 }, (_, index) =>
        makeRoot(path.join(String(index).padStart(3, '0'), suffix))
      );
      const first = await scan(0);
      expect(first.roots).toHaveLength(512);
      expect(first.truncated).toBe(false);
      fs.unlinkSync(path.join(roots[0], LOG));
      expect(fs.statSync(roots[0]).isDirectory()).toBe(true);
      const added = makeRoot(path.join('zzz', suffix));
      for (let pass = 1; pass <= 3; pass++) {
        const result = await scan(pass * (TTL + 1));
        expect(result.roots).toHaveLength(512);
        expect(result.roots).not.toContain(roots[0]);
        expect(result.roots).toContain(added);
        expect(result.truncated).toBe(false);
      }
    }
  );

  it.each(['directory', 'marker'])(
    'retains cached roots on a transient %s failure and retries absence later',
    async (failure) => {
      const { makeRoot, scan, rules } = fixture();
      const root = makeRoot(failure === 'directory' ? 'worker-sessions' : 'sessions');
      expect((await scan(0)).roots).toContain(root);
      fs.unlinkSync(path.join(root, LOG));
      const original = fs.promises.readdir;
      const originalMarker = rules.hasSessionMarker;
      const blocked = spyOn(fs.promises, 'readdir').mockImplementation((async (
        ...args: Parameters<typeof fs.promises.readdir>
      ) => {
        if (failure === 'directory' && String(args[0]) === root)
          throw Object.assign(new Error('fixture read failure'), { code: 'EACCES' });
        return Reflect.apply(original, fs.promises, args);
      }) as typeof fs.promises.readdir);
      if (failure !== 'directory') rules.hasSessionMarker = async () => null;
      try {
        const result = await scan(TTL + 1);
        expect(result.roots).toContain(root);
        expect(result.truncated).toBe(true);
      } finally {
        blocked.mockRestore();
        rules.hasSessionMarker = originalMarker;
      }
      const recovered = await scan(2 * (TTL + 1));
      expect(recovered.roots).not.toContain(root);
      expect(recovered.truncated).toBe(false);
    }
  );

  it.each(['read failure', 'entry bound'])(
    'keeps native sessions roots when their nested marker probe hits a %s',
    async (failure) => {
      const homeDir = path.join(temporary, 'home');
      const root = path.join(homeDir, 'PM-Experiments', 'experiment', 'sessions');
      const nested = path.join(root, 'nested');
      fs.mkdirSync(nested, { recursive: true });
      const log = path.join(nested, LOG);
      fs.writeFileSync(log, '{}\n');
      const discovery = { truncated: false };
      const scan = (pass: number) =>
        resolveOmpSessionRoots({
          env: {},
          homeDir,
          cacheDir: path.join(temporary, 'native-cache'),
          now: () => pass * (OMP_SCAN_CACHE_TTL_MS + 1),
          discovery,
        });
      expect(await scan(0)).toContain(root);
      const original = fs.promises.readdir;
      const blocked = spyOn(fs.promises, 'readdir').mockImplementation((async (
        ...args: Parameters<typeof fs.promises.readdir>
      ) => {
        if (failure === 'read failure' && String(args[0]) === nested)
          throw Object.assign(new Error('fixture read failure'), { code: 'EACCES' });
        return Reflect.apply(original, fs.promises, args);
      }) as typeof fs.promises.readdir);
      if (failure === 'entry bound') {
        fs.unlinkSync(log);
        for (let index = 0; index < 201; index++)
          fs.writeFileSync(path.join(nested, `note-${index}.txt`), 'fixture\n');
      }
      try {
        expect(await scan(1)).toContain(root);
        expect(discovery.truncated).toBe(true);
      } finally {
        blocked.mockRestore();
      }
      // A clean bounded probe can now establish that this root is empty.
      fs.rmSync(nested, { recursive: true });
      fs.mkdirSync(nested);
      expect(await scan(2)).not.toContain(root);
      expect(discovery.truncated).toBe(false);
    }
  );
});

import { execFile } from 'child_process';
import fs from 'fs';

export async function waitUntil(
  condition: () => boolean | Promise<boolean>,
  description: string,
  timeoutMs = 2000
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!(await condition())) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new Error(`${description} was not observed within ${timeoutMs} ms`);
    await new Promise((resolve) => setTimeout(resolve, Math.min(10, remaining)));
  }
}

async function processState(pid: number, timeoutMs: number): Promise<string | null> {
  if (process.platform === 'linux') {
    try {
      return fs.readFileSync(`/proc/${pid}/stat`, 'utf8').split(') ')[1].split(' ')[0];
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
      throw error;
    }
  }
  // The POSIX broker fixtures also run on macOS, which has no /proc filesystem.
  return new Promise((resolve, reject) => {
    execFile(
      '/bin/ps',
      ['-o', 'stat=', '-p', String(pid)],
      {
        env: { PATH: '/usr/bin:/bin' },
        timeout: timeoutMs,
        maxBuffer: 1024,
      },
      (error, stdout) => {
        if (error && error.code !== 1) reject(error);
        else resolve(stdout.trim()[0] ?? null);
      }
    );
  });
}

/** Poll only a fixture-owned PID; signal delivery and OS reaping are asynchronous. */
export async function waitForProcessState(
  pid: number,
  expected: readonly (string | null)[],
  timeoutMs = 2000
): Promise<void> {
  let observed: string | null | undefined;
  const deadline = Date.now() + timeoutMs;
  try {
    await waitUntil(
      async () => {
        observed = await processState(pid, Math.max(1, Math.min(500, deadline - Date.now())));
        return expected.includes(observed);
      },
      `Fixture PID ${pid}: expected ${expected.map((state) => state ?? 'gone').join('/')}`,
      timeoutMs
    );
  } catch (error) {
    throw new Error(
      `Fixture PID ${pid}: last state ${observed === undefined ? 'unread' : (observed ?? 'gone')}`,
      { cause: error }
    );
  }
}

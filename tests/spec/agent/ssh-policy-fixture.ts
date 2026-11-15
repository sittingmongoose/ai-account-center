/** Runs only inside ssh_compatibility.py's owned fixture namespace. */
import { execFile } from 'child_process';
import assert from 'assert/strict';
import * as policy from '../../../src/web-server/services/ssh-policy';
const started = Date.now();
let timerAt = 0;
const timer = new Promise<void>((resolve) =>
  setTimeout(() => {
    timerAt = Date.now() - started;
    resolve();
  }, 50)
);
const argv = await policy.sshArgv('fixture', 'printf fixture-connected');
const env =
  'sshEnvironment' in policy
    ? (policy.sshEnvironment as () => NodeJS.ProcessEnv)()
    : { PATH: process.env.PATH, HOME: process.env.HOME };
const output = await new Promise<string>((resolve, reject) => {
  execFile('ssh', argv, { env, timeout: 12000 }, (error, stdout) =>
    error ? reject(error) : resolve(stdout)
  );
});
await timer;
assert.equal(output, 'fixture-connected');
assert.ok(timerAt < 500, `50 ms timer blocked for ${timerAt} ms`);
console.log(JSON.stringify({ connected: true, timerAt, elapsed: Date.now() - started }));

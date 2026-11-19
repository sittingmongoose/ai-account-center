import { readFileSync } from 'fs';
import path from 'path';

/**
 * The supervised PTY driver for one Antigravity sign-in (CONTRACT-registry-
 * lifecycle 6.6, in-browser flow). A fixed Python program, spawned by the
 * sign-in job runner with plain pipes; it owns the pseudo-terminal the
 * official CLI runs on, the same way signin-process.ts's relay owns the
 * device-code CLIs'.
 *
 * What the driver does (observed against the real CLI 1.2.16 on Ubuntu, in
 * the signin-sandbox isolation; every screen below arrived within ~1 s, and
 * the authorization URL 0.9 s after the job started on the 2026-10-05 run):
 *
 * - it starts the given argv (bubblewrap + the official CLI) on a PTY with
 *   echo off and a 40x160 window, and never lets raw screen bytes leave:
 *   the job's output parser sees only the one authorization URL line;
 * - on the first-run login-method screen (`Select login method`, with
 *   `> 1. Google OAuth` highlighted) it presses Enter exactly once, so the
 *   CLI reaches its Google OAuth screen without the user's own keys; a CLI
 *   that skips that screen still works, because the link is read whenever it
 *   appears;
 *   a CLI that highlights another entry gets no key at all, and its job ends
 *   `unexpected_output` after the first-output timeout rather than the driver
 *   choosing an entry for the user;
 * - it extracts the authorization URL from the CLI output and prints that
 *   single line. Terminals that advertise OSC 8 (TERM=xterm-256color and
 *   friends) carry the whole URL in the hyperlink parameter; plainer
 *   terminals (TERM=dumb) get it as visually wrapped text, which the driver
 *   rejoins from its URL-character chunks once a finished line after the
 *   last chunk shows the link has ended (a PTY read can stop anywhere,
 *   including between two wrapped lines). Either way the URL is complete
 *   and its origin is checked against the one allowed origin before it is
 *   printed, so the parser's allowlist can never see a foreign URL;
 * - it forwards the runner's stdin (the pasted authorization code plus a
 *   newline) to the CLI, translating `\n` to `\r`: the CLI's input line
 *   submits on carriage return only;
 * - it polls the new credential file in the isolated home (the same
 *   completeness and safety judgement as signin-sandbox's
 *   maskedCredentialState) and, once it is complete twice in a row, stops
 *   the CLI before any task can be typed into it and exits 0;
 * - terminal queries the CLI sends (DA2, DECRQM, kitty) are deliberately
 *   left unanswered: the CLI was observed to fall back and continue without
 *   them, and answering would mean emulating a terminal this flow does not
 *   need.
 *
 * Exit codes: 0 the credential is complete (the runner then imports and
 * verifies it); 3 the CLI died without a complete credential; 2 a driver
 * misuse that the argv guard below makes unreachable in production.
 * Signals (the runner's cancel, timeout and shutdown) stop the CLI's whole
 * process group: SIGTERM, then SIGKILL one second later, like the relay.
 */
export const AGY_DRIVER_PYTHON = '/usr/bin/python3';

/** Fixed driver program; its argv after `-c <program>` is documented below. */
export const AGY_SIGNIN_DRIVER_PROGRAM = readFileSync(
  path.resolve(__dirname, '../../scripts/antigravity/signin_driver.py'),
  'utf8'
).trimEnd();

/**
 * Sign-in output parsing (CONTRACT-registry-lifecycle section 6.6): only the
 * allowlisted verification URL and the user code are kept.
 */
import { describe, expect, it } from 'bun:test';
import { MUSE_DEVICE_AUTH_ORIGINS } from '../../../src/web-server/services/muse-account-lifecycle';
import {
  MAX_SIGNIN_OUTPUT_BYTES,
  SignInOutputParser,
  stripTerminalControls,
} from '../../../src/web-server/services/signin-output';

const CODEX = { allowedOrigins: ['https://auth.openai.com'], expectsUserCode: true };

/** The shape `codex login --device-auth` prints (strings from codex-cli 0.159). */
const DEVICE_OUTPUT = [
  '\x1b[1mWelcome to Codex\x1b[0m',
  'Follow these steps to sign in with ChatGPT using device code authorization:',
  '',
  '1. Open this link in your browser and sign in to your account',
  '   \x1b[94mhttps://auth.openai.com/codex/device\x1b[0m',
  '',
  '2. Enter this one-time code \x1b[2m(expires in 15 minutes)\x1b[0m',
  '   \x1b[94mABCD-12345\x1b[0m',
  '',
].join('\r\n');

describe('stripTerminalControls', () => {
  it('removes colors, OSC 8 hyperlinks and cursor moves', () => {
    expect(stripTerminalControls('\x1b[31mred\x1b[0m')).toBe('red');
    expect(
      stripTerminalControls(
        '\x1b]8;;https://evil.example/\x1b\\https://auth.openai.com/x\x1b]8;;\x1b\\'
      )
    ).toBe('https://auth.openai.com/x');
    expect(stripTerminalControls('\x1b[2K\x1b[1Gline\x07')).toBe('line');
  });
});

describe('SignInOutputParser', () => {
  it('finds the device URL and code in Codex output, in any chunking', () => {
    for (const size of [1, 3, 17, DEVICE_OUTPUT.length]) {
      const parser = new SignInOutputParser(CODEX);
      let state = parser.current;
      for (let index = 0; index < DEVICE_OUTPUT.length; index += size) {
        state = parser.push(DEVICE_OUTPUT.slice(index, index + size));
      }
      expect(state).toBe('ready');
      expect(parser.verification()).toEqual({
        url: 'https://auth.openai.com/codex/device',
        userCode: 'ABCD-12345',
      });
    }
  });

  it('finds the device URL and code in the real `muse login` shape', () => {
    // FW2-B: labels from the launcher script and the 1.4.2 binary strings,
    // URL and code shape measured independently; the example code is invented.
    const parser = new SignInOutputParser({
      allowedOrigins: MUSE_DEVICE_AUTH_ORIGINS,
      expectsUserCode: true,
    });
    const output = [
      'Open this page to sign in:',
      '  https://auth.meta.com/oauth/device/?code=WDJB-MQRT',
      'Confirm this code matches:',
      '  WDJB-MQRT',
      'Waiting for approval (link expires in 15 minutes)... ',
    ].join('\n');
    expect(parser.push(output)).toBe('ready');
    expect(parser.verification()).toEqual({
      url: 'https://auth.meta.com/oauth/device/',
      userCode: 'WDJB-MQRT',
    });
  });

  it('drops the query string of a device URL and keeps it for supervised flows', () => {
    const device = new SignInOutputParser(CODEX);
    device.push('https://auth.openai.com/codex/device?user_code=ABCD-1234&x=1\nABCD-1234\n');
    expect(device.verification()?.url).toBe('https://auth.openai.com/codex/device');
    const supervised = new SignInOutputParser({
      allowedOrigins: ['https://accounts.google.com'],
      expectsUserCode: false,
      keepQuery: true,
    });
    expect(supervised.push('Go to https://accounts.google.com/o/oauth2/auth?state=s1.\n')).toBe(
      'ready'
    );
    expect(supervised.verification()).toEqual({
      url: 'https://accounts.google.com/o/oauth2/auth?state=s1',
      userCode: null,
    });
  });

  it('fails on an https URL off the allowlist, plain http or embedded credentials', () => {
    for (const line of [
      'Open https://auth.openai.com.evil.example/codex/device',
      'Open https://evil.example/codex/device',
      'Open http://auth.openai.com/codex/device',
      'Open https://user:pass@auth.openai.com/codex/device',
    ]) {
      const parser = new SignInOutputParser(CODEX);
      expect(parser.push(`${line}\nABCD-1234\n`)).toBe('rejected');
      expect(parser.verification()).toBeNull();
    }
  });

  it('waits for both facts and reads an unterminated last line at the end', () => {
    const parser = new SignInOutputParser(CODEX);
    expect(parser.push('https://auth.openai.com/codex/device\n')).toBe('pending');
    expect(parser.push('Codes look like WXYZ-9876 but this line is prose\n')).toBe('pending');
    expect(parser.push('  WXYZ-9876')).toBe('pending');
    expect(parser.end()).toBe('ready');
    expect(parser.verification()?.userCode).toBe('WXYZ-9876');
  });

  it('stops after 64 KB and never keeps anything else', () => {
    const parser = new SignInOutputParser(CODEX);
    const filler = `${'x'.repeat(1023)}\n`;
    let state = parser.current;
    for (let total = 0; total <= MAX_SIGNIN_OUTPUT_BYTES; total += filler.length) {
      state = parser.push(filler);
    }
    expect(state).toBe('rejected');
    expect(parser.push(DEVICE_OUTPUT)).toBe('rejected');
    const long = new SignInOutputParser(CODEX);
    expect(long.push('y'.repeat(9000))).toBe('rejected');
  });
});

import { describe, expect, it } from 'bun:test';

import { parseConfigCommandArgs } from '../../../src/commands/config-command-options';

describe('dashboard command options parser', () => {
  it('defaults to a localhost-only bind without an explicit host', () => {
    const result = parseConfigCommandArgs([]);

    expect(result.help).toBe(false);
    expect(result.error).toBeUndefined();
    expect(result.options.host).toBe('localhost');
    expect(result.options.hostProvided).toBe(false);
  });

  it.each([
    ['--host', '0.0.0.0', '--port', '4100'],
    ['--host=0.0.0.0', '--port=4100'],
    ['-H', '0.0.0.0', '-p', '4100'],
  ])('parses host and port overrides in supported forms: %j', (...args) => {
    const result = parseConfigCommandArgs(args);

    expect(result.error).toBeUndefined();
    expect(result.options.host).toBe('0.0.0.0');
    expect(result.options.hostProvided).toBe(true);
    expect(result.options.port).toBe(4100);
  });

  it.each([['--host'], ['--host='], ['--host', '--no-open']])(
    'rejects missing host values: %j',
    (...args) => {
      expect(parseConfigCommandArgs(args).error).toBe('Invalid host value');
    }
  );

  it.each(['1', '65535'])('accepts a port at the range boundary: %s', (port) => {
    const result = parseConfigCommandArgs(['--port', port]);

    expect(result.error).toBeUndefined();
    expect(result.options.port).toBe(Number(port));
  });

  it.each(['0', '65536', '-1', '4100junk', '3.5', '+4100', '1e3', '0x1000', ''])(
    'rejects a port that is not decimal digits in the valid range: %j',
    (port) => {
      expect(parseConfigCommandArgs([`--port=${port}`]).error).toBe('Invalid port number');
    }
  );

  it('rejects a port flag with no value', () => {
    expect(parseConfigCommandArgs(['--port']).error).toBe('Invalid port number');
  });

  it.each([
    ['--port', '3000', '--port', '4100'],
    ['--port=3000', '-p', '4100'],
    ['--host', 'localhost', '--host', '0.0.0.0'],
    ['--host=localhost', '-H', '0.0.0.0'],
  ])('rejects duplicate host or port options: %j', (...args) => {
    expect(parseConfigCommandArgs(args).error).toBeDefined();
  });

  it('accepts an explicit no-open flag', () => {
    const result = parseConfigCommandArgs(['--no-open']);

    expect(result.error).toBeUndefined();
    expect(result.options.noOpen).toBe(true);
  });

  it('gives an explicit migration error for the retired Vite development option', () => {
    const result = parseConfigCommandArgs(['--dev']);

    expect(result.help).toBe(false);
    expect(result.error).toMatch(/retired|no longer/i);
    expect(result.error).toMatch(/dashboard|account center/i);
  });

  it.each(['--help', '-h'])('recognizes help without launching: %s', (flag) => {
    const result = parseConfigCommandArgs([flag]);

    expect(result.help).toBe(true);
    expect(result.error).toBeUndefined();
  });

  it('rejects unknown options', () => {
    expect(parseConfigCommandArgs(['--hst', '0.0.0.0']).error).toBe(
      'Unexpected arguments: --hst 0.0.0.0'
    );
  });

  it('rejects unexpected trailing positionals', () => {
    expect(parseConfigCommandArgs(['--port', '3000', 'extra']).error).toBe(
      'Unexpected arguments: extra'
    );
  });
});

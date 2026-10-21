import { afterEach, describe, expect, it, mock } from 'bun:test';
import {
  DeadlineExceededError,
  withDeadline,
  withProcessDeadline,
} from '../../../src/utils/settle-deadline';

const never = <T>() => new Promise<T>(() => undefined);

afterEach(() => {
  mock.restore();
});

describe('withDeadline', () => {
  it('passes through a result that settles in time', async () => {
    await expect(withDeadline(Promise.resolve('ready'), 1000)).resolves.toBe('ready');
    await expect(withDeadline(Promise.reject(new Error('failed')), 1000)).rejects.toThrow('failed');
  });

  it('settles work that never completes, runs onTimeout first and ignores a late result', async () => {
    let complete: (value: string) => void = () => {};
    const work = new Promise<string>((resolve) => {
      complete = resolve;
    });
    const onTimeout = mock(() => undefined);
    const settled = withDeadline(work, 10, onTimeout, 'Fixture read');
    await expect(settled).rejects.toBeInstanceOf(DeadlineExceededError);
    await expect(settled).rejects.toThrow('Fixture read did not finish before its deadline');
    expect(onTimeout).toHaveBeenCalledTimes(1);
    complete('late');
    await expect(settled).rejects.toBeInstanceOf(DeadlineExceededError);
  });

  it('still settles when the abort callback throws', async () => {
    const settled = withDeadline(never<string>(), 5, () => {
      throw new Error('abort failed');
    });
    await expect(settled).rejects.toBeInstanceOf(DeadlineExceededError);
  });
});

describe('withProcessDeadline', () => {
  it('kills a child whose exit callback never arrives and rejects with the caller error', async () => {
    const kill = mock((_signal?: NodeJS.Signals) => true);
    const settled = withProcessDeadline(
      never<string>(),
      () => ({ kill }),
      10,
      () => new Error('Fixture source unavailable.'),
      10
    );
    await expect(settled).rejects.toThrow('Fixture source unavailable.');
    expect(kill).toHaveBeenCalledWith('SIGKILL');
  });

  it('keeps the process error when the child exits in time', async () => {
    const kill = mock((_signal?: NodeJS.Signals) => true);
    const settled = withProcessDeadline(
      Promise.reject(new Error('exit 1')),
      () => ({ kill }),
      1000,
      () => new Error('expired')
    );
    await expect(settled).rejects.toThrow('exit 1');
    expect(kill).not.toHaveBeenCalled();
  });
});

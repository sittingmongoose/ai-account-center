/**
 * Reads a sign-in CLI's output in memory and keeps only two facts
 * (CONTRACT-registry-lifecycle section 6.6):
 * - the verification URL, whose origin must be on the provider allowlist
 *   (any https URL elsewhere fails the job, never shown);
 * - the one-time user code, a whole line matching
 *   `^[A-Z0-9]{4,6}(-[A-Z0-9]{4,6})?$`.
 * Everything else is discarded unread. At most 64 KB is examined; terminal
 * control sequences (colors, OSC 8 hyperlinks, cursor moves) are removed first.
 * Nothing here logs or persists output.
 */
export const MAX_SIGNIN_OUTPUT_BYTES = 64 * 1024;
const MAX_LINE_CHARS = 8 * 1024;
export const USER_CODE_PATTERN = /^[A-Z0-9]{4,6}(?:-[A-Z0-9]{4,6})?$/;
const URL_PATTERN = /\bhttps?:\/\/[^\s"'<>`]+/gi;

/** Remove OSC, CSI and two-character escapes, then any other control character. */
export function stripTerminalControls(text: string): string {
  return text
    .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)?/g, '')
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '')
    .replace(/\x1b[@-Z\\-_]/g, '')
    .replace(/[\x00-\x08\x0b-\x1f\x7f]/g, '');
}

export interface SignInVerification {
  url: string;
  userCode: string | null;
}

export interface SignInOutputOptions {
  /** Exact origins, e.g. `https://auth.openai.com`. */
  allowedOrigins: readonly string[];
  /** Device-code flows need a user code; supervised flows only a URL. */
  expectsUserCode: boolean;
  /** Keep the query string (supervised flows); device-code URLs never keep it. */
  keepQuery?: boolean;
}

export type SignInOutputState = 'pending' | 'ready' | 'rejected';

export class SignInOutputParser {
  private examined = 0;
  private partial = '';
  private url: string | null = null;
  private userCode: string | null = null;
  private state: SignInOutputState = 'pending';

  constructor(private readonly options: SignInOutputOptions) {}

  get current(): SignInOutputState {
    return this.state;
  }

  verification(): SignInVerification | null {
    return this.state === 'ready' && this.url ? { url: this.url, userCode: this.userCode } : null;
  }

  /** Feed more output; returns the state after it. Output after `ready` is ignored. */
  push(chunk: string): SignInOutputState {
    if (this.state !== 'pending') return this.state;
    const budget = MAX_SIGNIN_OUTPUT_BYTES - this.examined;
    const bytes = Buffer.byteLength(chunk, 'utf8');
    const accepted =
      bytes > budget ? Buffer.from(chunk, 'utf8').subarray(0, budget).toString() : chunk;
    this.examined += Math.min(bytes, budget);
    const lines = (this.partial + accepted).split(/\r\n|\n|\r/);
    this.partial = lines.pop() ?? '';
    for (const line of lines) {
      this.line(line);
      if (this.state !== 'pending') return this.state;
    }
    if (this.partial.length > MAX_LINE_CHARS) return this.reject();
    if (this.examined >= MAX_SIGNIN_OUTPUT_BYTES) return this.reject();
    return this.state;
  }

  /** The process ended: read the unterminated last line too. */
  end(): SignInOutputState {
    if (this.state === 'pending' && this.partial) {
      const last = this.partial;
      this.partial = '';
      this.line(last);
    }
    return this.state;
  }

  private reject(): SignInOutputState {
    this.state = 'rejected';
    this.partial = '';
    return this.state;
  }

  private line(raw: string): void {
    const text = stripTerminalControls(raw).trim();
    if (!text) return;
    for (const match of text.match(URL_PATTERN) ?? []) {
      const candidate = this.allowedUrl(match.replace(/[.,;:)\]}>]+$/, ''));
      if (candidate === null) {
        this.reject();
        return;
      }
      this.url ??= candidate;
    }
    if (this.userCode === null && USER_CODE_PATTERN.test(text)) this.userCode = text;
    if (this.url && (this.userCode || !this.options.expectsUserCode)) this.state = 'ready';
  }

  private allowedUrl(value: string): string | null {
    let parsed: URL;
    try {
      parsed = new URL(value);
    } catch {
      return null;
    }
    if (
      parsed.protocol !== 'https:' ||
      parsed.username ||
      parsed.password ||
      !this.options.allowedOrigins.includes(parsed.origin)
    ) {
      return null;
    }
    return `${parsed.origin}${parsed.pathname}${this.options.keepQuery ? parsed.search : ''}`;
  }
}

/**
 * How the owner's answers reach the agents after they are stored and signed:
 * - one pm-mail ring per asker per send (not one per card), naming the batch if any, the count and
 *   the card ids, with each card's exact choice and words, the UTC time and the card hash;
 * - a signed send summary `<reports>/v1/answers/batches/<sendId>.json` listing every record of
 *   the send.
 * The signed files are the authority; the mail is only a doorbell. A card without a pm-mail name
 * is not rung: its agent reads its answers file.
 */
import type { SignedAnswer } from './answer-signing';
import type { CardDoc } from './report-schema';
import {
  childEnv,
  commandFailure,
  findExecutable,
  runCommand,
  type CommandRunner,
} from './status-collectors';

export const DOORBELL_SENDER = 'aac-answers';
export const SEND_SCHEMA = 'agent-reports/send@1';
/** The doorbell detail for a card with no pm-mail name: delivery is complete without a ring. */
export const NO_MAIL_DETAIL = 'No mail name; the agent reads its answers file.';
export const DOORBELL_OFF_DETAIL = 'The pm-mail doorbell is turned off on this server.';

export interface RingResult {
  ok: boolean;
  detail: string;
}

export type Doorbell = (to: string, subject: string, body: string) => Promise<RingResult>;

/**
 * `pm-mail --as aac-answers send <to> <subject>` with the body on stdin (a send of 50 notes is
 * longer than one command-line argument may be), spawned with a timeout.
 */
export function pmMailDoorbell(run: CommandRunner = runCommand): Doorbell {
  return async (to, subject, body) => {
    const bin = findExecutable('pm-mail', ['/mnt/Cursor/Inbox/pm-mail']);
    const result = await run(bin, ['--as', DOORBELL_SENDER, 'send', to, subject], {
      timeoutMs: 30_000,
      maxBytes: 64 * 1024,
      env: childEnv(),
      input: body,
    });
    return result.code === 0
      ? { ok: true, detail: `Rang ${to} through pm-mail.` }
      : { ok: false, detail: commandFailure('pm-mail', result) };
  };
}

/** One record of a send, as the ring and the summary need it. */
export interface SentRecord {
  answer: SignedAnswer;
  card: CardDoc;
  /** The record's signed file under `<reports>/v1/answers/`. */
  file: string;
}

const KIND_WORDS: Record<SignedAnswer['kind'], string> = {
  answer: 'answer',
  note: 'note (the card stays open; reply on the card or withdraw it)',
  object: 'objection (the card stays open; reply on the card or withdraw it)',
  done: 'done',
};

/**
 * Card files are written by agents and the ring is read by agents, so a card's words stay on the one
 * line they were given: a newline in a label could otherwise forge lines of the owner's answer.
 */
function oneLine(value: string): string {
  return value.replace(/[\s\u0000-\u001f\u007f-\u009f]+/g, ' ').trim();
}

function batchesOf(records: SentRecord[]): string[] {
  return [...new Set(records.flatMap((r) => (r.card.batch ? [oneLine(r.card.batch)] : [])))].sort();
}

export function ringSubject(records: SentRecord[]): string {
  const batches = batchesOf(records);
  const count = `${records.length} ${records.length === 1 ? 'answer' : 'answers'}`;
  return batches.length > 0
    ? `${count} from the owner (${batches.join(', ')})`
    : `${count} from the owner`;
}

function choiceText(record: SentRecord): string {
  const labels = new Map((record.card.options ?? []).map((option) => [option.key, option.label]));
  return record.answer.choice.length > 0
    ? record.answer.choice
        .map((key) => `${oneLine(key)} (${oneLine(labels.get(key) ?? 'unknown option')})`)
        .join(', ')
    : 'none';
}

/** The ring for one asker: every record of the send that belongs to that asker. */
export function ringBody(sendId: string | null, records: SentRecord[], keyId: string): string {
  const batches = batchesOf(records);
  const lines = [
    `The owner sent ${records.length} ${records.length === 1 ? 'answer' : 'answers'} on the AI Account Center Status tab.`,
    ...(batches.length > 0 ? [`Batch: ${batches.join(', ')}`] : []),
    ...(sendId ? [`Send: ${sendId}`] : []),
    `Cards: ${records.map((r) => oneLine(r.answer.cardId)).join(', ')}`,
    '',
  ];
  for (const record of records) {
    const { answer } = record;
    lines.push(
      `Card ${oneLine(answer.cardId)} (seq ${answer.cardSeq})`,
      `  Kind: ${KIND_WORDS[answer.kind] ?? answer.kind}`,
      `  Choice: ${choiceText(record)}`,
      `  Words: ${answer.note ? answer.note.replace(/\r\n?|\n/g, '\n    ') : '(none)'}`,
      `  Time (UTC): ${answer.answeredAt}`,
      `  Card sha256: ${answer.cardSha256}`,
      `  Signed file: agent-reports/v1/answers/${oneLine(record.file)}`,
      ''
    );
  }
  lines.push(
    'The signed files are the authority' +
      (sendId ? `, with the whole send in agent-reports/v1/answers/batches/${sendId}.json` : '') +
      ` (key agent-reports/v1/keys/${keyId}.pub). This mail is only a doorbell.`
  );
  return lines.join('\n');
}

export interface SendSummary {
  schema: typeof SEND_SCHEMA;
  sendId: string;
  at: string;
  answeredBy: 'owner';
  device: string;
  entries: Array<{
    cardId: string;
    seq: number;
    cardSha256: string;
    kind: SignedAnswer['kind'];
    choice: string[];
    note: string;
  }>;
}

/** The unsigned send summary: one entry per record of the send, in the order sent. */
export function sendSummary(
  sendId: string,
  at: string,
  device: string,
  answers: SignedAnswer[]
): SendSummary {
  return {
    schema: SEND_SCHEMA,
    sendId,
    at,
    answeredBy: 'owner',
    device,
    entries: answers.map((answer) => ({
      cardId: answer.cardId,
      seq: answer.cardSeq,
      cardSha256: answer.cardSha256,
      kind: answer.kind,
      choice: answer.choice,
      note: answer.note,
    })),
  };
}
